//! Command line entrypoints for the updater.

use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use semver::Version;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::UPDATER_VERSION;
use crate::app::{
    ApplyCandidateReleaseRequest, ImageImportApp, ImportValidatedImageRequest, UpdateApp,
    ValidatePackageRequest, ValidationApp,
};
use crate::config::{ConfigError, load_service_catalog};
use crate::docker::{BollardDockerClient, DockerClientError, ImageImportError};
use crate::domain::{
    AuditEvent, AuditEventKind, ImageImportRecord, ImageImportStatus, ManagedService,
    PackageManifest, RuntimeMode, ServiceCatalog, ServiceStateRecord, UpdateAttemptRecord,
    UpdateAttemptStatus, ValidationRecord, ValidationStatus,
};
use crate::persistence::{FilesystemStore, PersistenceError, StateStore};
use crate::update::UpdateError;

/// Runs the CLI and hands back the exit code.
pub fn run() -> Result<ExitCode, CliError> {
    let cli = Cli::parse();
    let workflow = RealPackageWorkflow;
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();

    execute(cli, &workflow, &mut input, &mut output)
}

fn execute<W, R, F>(
    cli: Cli,
    workflow: &F,
    input: &mut R,
    output: &mut W,
) -> Result<ExitCode, CliError>
where
    W: Write,
    R: BufRead,
    F: PackageWorkflow,
{
    let Cli {
        services,
        state_dir,
        command,
    } = cli;

    match command {
        Commands::Service { command } => run_service_command(command, services, state_dir, output),
        Commands::Package { command } => {
            run_package_command(command, services, state_dir, workflow, input, output)
        }
        Commands::History { command } => run_history_command(command, state_dir, output),
    }
}

fn run_service_command<W>(
    command: ServiceCommands,
    services_path: PathBuf,
    state_dir: PathBuf,
    output: &mut W,
) -> Result<ExitCode, CliError>
where
    W: Write,
{
    let catalog = load_service_catalog(&services_path)?;
    let store = FilesystemStore::new(state_dir);
    let states = store.list_service_states()?;
    let attempts = store.list_update_attempts()?;

    match command {
        ServiceCommands::List => write_service_list(output, &catalog, &states, &attempts)?,
        ServiceCommands::Show { service } => {
            let managed_service = catalog.find_service(&service).ok_or_else(|| {
                CliError::Lookup(format!(
                    "service `{service}` is not defined in the service catalog"
                ))
            })?;
            let state = states
                .iter()
                .find(|record| record.service_name == managed_service.name);
            let last_attempt = latest_attempt_for_service(&attempts, &managed_service.name);
            write_service_details(output, managed_service, state, last_attempt)?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn run_package_command<W, R, F>(
    command: PackageCommands,
    services_path: PathBuf,
    state_dir: PathBuf,
    workflow: &F,
    input: &mut R,
    output: &mut W,
) -> Result<ExitCode, CliError>
where
    W: Write,
    R: BufRead,
    F: PackageWorkflow,
{
    let catalog = load_service_catalog(&services_path)?;

    match command {
        PackageCommands::Validate { package } => {
            writeln!(output, "Validating package...")?;
            let record = workflow.validate_package(catalog, package, state_dir)?;
            write_validation_summary(output, &record)?;

            Ok(match record.status {
                ValidationStatus::Accepted => ExitCode::SUCCESS,
                ValidationStatus::Rejected => ExitCode::from(2),
            })
        }
        PackageCommands::Load { package, yes } => {
            writeln!(output, "Validating package...")?;
            let validation_record =
                workflow.validate_package(catalog.clone(), package, state_dir.clone())?;
            write_validation_summary(output, &validation_record)?;

            if validation_record.status == ValidationStatus::Rejected {
                return Ok(ExitCode::from(2));
            }

            let manifest = validation_record.manifest.as_ref().ok_or_else(|| {
                CliError::Invariant(
                    "accepted validation record is missing the parsed package manifest".into(),
                )
            })?;
            let store = FilesystemStore::new(&state_dir);
            let current_state = store.load_service_state(&manifest.target.service)?;

            if !yes && !confirm_package_load(output, input, manifest, current_state.as_ref())? {
                writeln!(output, "Cancelled before changing the service.")?;
                return Ok(ExitCode::from(3));
            }

            writeln!(output, "Importing image into Docker...")?;
            let import_record =
                workflow.import_validated_image(validation_record, state_dir.clone())?;
            write_image_import_summary(output, &import_record)?;

            if import_record.status == ImageImportStatus::Failed {
                return Ok(ExitCode::from(4));
            }

            let candidate_release_id = import_record.candidate_release_id.ok_or_else(|| {
                CliError::Invariant(
                    "successful image import is missing the staged release identifier".into(),
                )
            })?;

            writeln!(output, "Applying update...")?;
            let update_record =
                workflow.apply_candidate_release(catalog, candidate_release_id, state_dir)?;
            write_update_summary(output, &update_record)?;

            Ok(exit_code_for_update_status(update_record.status))
        }
    }
}

fn run_history_command<W>(
    command: HistoryCommands,
    state_dir: PathBuf,
    output: &mut W,
) -> Result<ExitCode, CliError>
where
    W: Write,
{
    let store = FilesystemStore::new(state_dir);

    match command {
        HistoryCommands::List { service, limit } => {
            let attempts = store.list_update_attempts()?;
            write_history_list(output, &attempts, service.as_deref(), limit)?;
        }
        HistoryCommands::Show { update_id } => {
            let attempt = store
                .load_update_attempt(update_id)?
                .ok_or_else(|| CliError::Lookup(format!("update `{update_id}` was not found")))?;
            let audit_events = store.list_audit_events()?;
            write_history_details(output, &attempt, &audit_events)?;
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn write_service_list<W>(
    output: &mut W,
    catalog: &ServiceCatalog,
    states: &[ServiceStateRecord],
    attempts: &[UpdateAttemptRecord],
) -> Result<(), CliError>
where
    W: Write,
{
    let states_by_service = states
        .iter()
        .map(|record| (record.service_name.clone(), record))
        .collect::<BTreeMap<_, _>>();

    writeln!(output, "Known services")?;

    for service in &catalog.services {
        let state = states_by_service.get(&service.name).copied();
        let last_attempt = latest_attempt_for_service(attempts, &service.name);
        writeln!(
            output,
            "- {} | runtime: {} | active image: {} | last update: {}",
            service.name,
            runtime_label(&service.runtime),
            active_image_label(service, state),
            last_update_label(last_attempt)
        )?;
    }

    if catalog.services.is_empty() {
        writeln!(
            output,
            "No services are defined in the current service catalog."
        )?;
    }

    Ok(())
}

fn write_service_details<W>(
    output: &mut W,
    service: &ManagedService,
    state: Option<&ServiceStateRecord>,
    last_attempt: Option<&UpdateAttemptRecord>,
) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "Service: {}", service.name)?;
    writeln!(output, "Revision: {}", service.service_revision)?;
    writeln!(output, "Platform: {}", service.platform)?;
    writeln!(output, "Runtime: {}", runtime_label(&service.runtime))?;

    match &service.runtime {
        RuntimeMode::Docker(runtime) => {
            writeln!(output, "Container: {}", runtime.container_name)?;
            writeln!(output, "Catalog image: {}", runtime.image_reference)?;
        }
        RuntimeMode::Compose(runtime) => {
            writeln!(output, "Compose project: {}", runtime.project)?;
            writeln!(output, "Compose file: {}", runtime.file.display())?;
            writeln!(output, "Compose service: {}", runtime.service)?;
        }
    }

    writeln!(output, "Health check: {}", health_check_label(service))?;
    writeln!(
        output,
        "Active image: {}",
        active_image_label(service, state)
    )?;
    writeln!(
        output,
        "Previous known-good image: {}",
        state
            .and_then(|record| record.previous_known_good_image_reference.as_deref())
            .unwrap_or("not recorded yet")
    )?;

    if let Some(state) = state {
        writeln!(
            output,
            "State updated: {}",
            format_timestamp(state.updated_at)
        )?;
    }

    match last_attempt {
        Some(attempt) => {
            writeln!(
                output,
                "Last update: {}",
                update_status_label(attempt.status)
            )?;
            writeln!(
                output,
                "Last target image: {}",
                attempt.target_image_reference
            )?;
            writeln!(
                output,
                "Last started: {}",
                format_timestamp(attempt.started_at)
            )?;
            if let Some(finished_at) = attempt.finished_at {
                writeln!(output, "Last finished: {}", format_timestamp(finished_at))?;
            }
        }
        None => writeln!(output, "Last update: none yet")?,
    }

    Ok(())
}

fn write_validation_summary<W>(output: &mut W, record: &ValidationRecord) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "Package validation")?;
    writeln!(output, "Status: {}", validation_status_label(record.status))?;
    writeln!(output, "Source package: {}", record.source_path.display())?;

    if let Some(manifest) = record.manifest.as_ref() {
        writeln!(output, "Target service: {}", manifest.target.service)?;
        writeln!(
            output,
            "Service revision: {}",
            manifest.target.service_revision
        )?;
        writeln!(output, "Platform: {}", manifest.target.platform)?;
        writeln!(output, "Package version: {}", manifest.package_version)?;
        writeln!(output, "Target image: {}", manifest.image.reference)?;
    }

    if let Some(image_archive) = record.image_archive.as_ref() {
        writeln!(
            output,
            "Image archive: {} ({} bytes)",
            image_archive.entry_name, image_archive.size_bytes
        )?;
    }

    write_issue_block(output, &record.issues)?;

    Ok(())
}

fn write_image_import_summary<W>(output: &mut W, record: &ImageImportRecord) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "Image import")?;
    writeln!(
        output,
        "Status: {}",
        image_import_status_label(record.status)
    )?;
    writeln!(output, "Service: {}", record.service_name)?;
    writeln!(output, "Image: {}", record.image_reference)?;

    if let Some(imported_image) = record.imported_image.as_ref() {
        writeln!(output, "Imported image id: {}", imported_image.image_id)?;
    }

    write_issue_block(output, &record.issues)?;

    Ok(())
}

fn write_update_summary<W>(output: &mut W, record: &UpdateAttemptRecord) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "Update result")?;
    writeln!(output, "Status: {}", update_status_label(record.status))?;
    writeln!(output, "Service: {}", record.service_name)?;
    writeln!(output, "Target image: {}", record.target_image_reference)?;

    if let Some(previous_image) = record.previous_image_reference.as_ref() {
        writeln!(output, "Previous image: {previous_image}")?;
    }

    writeln!(output, "Started: {}", format_timestamp(record.started_at))?;

    if let Some(finished_at) = record.finished_at {
        writeln!(output, "Finished: {}", format_timestamp(finished_at))?;
    }

    if let Some(health_check) = record.health_check.as_ref() {
        writeln!(
            output,
            "Health check: {} ({})",
            health_check_outcome_label(health_check.outcome),
            health_check.message
        )?;
    }

    write_issue_block(output, &record.issues)?;

    writeln!(
        output,
        "Summary: {}",
        update_summary_sentence(record.status)
    )?;

    Ok(())
}

fn write_history_list<W>(
    output: &mut W,
    attempts: &[UpdateAttemptRecord],
    service_filter: Option<&str>,
    limit: Option<usize>,
) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "Update history")?;

    let mut shown = 0usize;
    for attempt in attempts.iter().filter(|attempt| {
        service_filter
            .map(|service_name| attempt.service_name == service_name)
            .unwrap_or(true)
    }) {
        if shown >= limit.unwrap_or(usize::MAX) {
            break;
        }

        writeln!(
            output,
            "- {} | {} | {} | {}",
            attempt.update_id,
            format_timestamp(attempt.started_at),
            attempt.service_name,
            update_status_label(attempt.status)
        )?;
        writeln!(output, "  target image: {}", attempt.target_image_reference)?;
        shown += 1;
    }

    if shown == 0 {
        writeln!(output, "No update history matched the current filter.")?;
    }

    Ok(())
}

fn write_history_details<W>(
    output: &mut W,
    attempt: &UpdateAttemptRecord,
    audit_events: &[AuditEvent],
) -> Result<(), CliError>
where
    W: Write,
{
    writeln!(output, "Update: {}", attempt.update_id)?;
    writeln!(output, "Service: {}", attempt.service_name)?;
    writeln!(output, "Status: {}", update_status_label(attempt.status))?;
    writeln!(output, "Target image: {}", attempt.target_image_reference)?;

    if let Some(previous_image) = attempt.previous_image_reference.as_ref() {
        writeln!(output, "Previous image: {previous_image}")?;
    }

    writeln!(output, "Started: {}", format_timestamp(attempt.started_at))?;

    if let Some(finished_at) = attempt.finished_at {
        writeln!(output, "Finished: {}", format_timestamp(finished_at))?;
    }

    if let Some(health_check) = attempt.health_check.as_ref() {
        writeln!(
            output,
            "Health check: {} ({})",
            health_check_outcome_label(health_check.outcome),
            health_check.message
        )?;
    }

    write_issue_block(output, &attempt.issues)?;
    writeln!(output, "Events:")?;

    let mut event_count = 0usize;
    for event in audit_events.iter().filter(|event| {
        event.attempt_id == attempt.update_id || event.attempt_id == attempt.validation_attempt_id
    }) {
        writeln!(
            output,
            "- {} | {} | {}",
            format_timestamp(event.occurred_at),
            audit_kind_label(event.kind),
            event.message
        )?;
        event_count += 1;
    }

    if event_count == 0 {
        writeln!(output, "- none recorded")?;
    }

    Ok(())
}

fn confirm_package_load<W, R>(
    output: &mut W,
    input: &mut R,
    manifest: &PackageManifest,
    current_state: Option<&ServiceStateRecord>,
) -> Result<bool, CliError>
where
    W: Write,
    R: BufRead,
{
    writeln!(output, "Ready to load package")?;
    writeln!(output, "Target service: {}", manifest.target.service)?;
    writeln!(
        output,
        "Service revision: {}",
        manifest.target.service_revision
    )?;
    writeln!(output, "Package version: {}", manifest.package_version)?;
    writeln!(output, "Target image: {}", manifest.image.reference)?;
    writeln!(
        output,
        "Current known image: {}",
        current_state
            .map(|state| state.active_image_reference.as_str())
            .unwrap_or("not recorded yet")
    )?;
    write!(output, "Continue and update the service? [y/N]: ")?;
    output.flush()?;

    let mut answer = String::new();
    input.read_line(&mut answer)?;

    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn write_issue_block<W>(
    output: &mut W,
    issues: &[crate::domain::ValidationIssue],
) -> Result<(), CliError>
where
    W: Write,
{
    if issues.is_empty() {
        writeln!(output, "Issues: none")?;
        return Ok(());
    }

    writeln!(output, "Issues:")?;
    for issue in issues {
        writeln!(output, "- [{}] {}", issue.code, issue.message)?;
    }

    Ok(())
}

fn latest_attempt_for_service<'a>(
    attempts: &'a [UpdateAttemptRecord],
    service_name: &str,
) -> Option<&'a UpdateAttemptRecord> {
    attempts
        .iter()
        .find(|attempt| attempt.service_name == service_name)
}

fn active_image_label(service: &ManagedService, state: Option<&ServiceStateRecord>) -> String {
    state
        .map(|record| record.active_image_reference.clone())
        .or_else(|| catalog_image_for_service(service).map(ToOwned::to_owned))
        .unwrap_or_else(|| "not recorded yet".into())
}

fn catalog_image_for_service(service: &ManagedService) -> Option<&str> {
    match &service.runtime {
        RuntimeMode::Docker(runtime) => Some(runtime.image_reference.as_str()),
        RuntimeMode::Compose(_) => None,
    }
}

fn runtime_label(runtime: &RuntimeMode) -> &'static str {
    match runtime {
        RuntimeMode::Docker(_) => "docker",
        RuntimeMode::Compose(_) => "compose",
    }
}

fn health_check_label(service: &ManagedService) -> &'static str {
    match service.health_check.kind {
        crate::domain::HealthCheckKind::Running => "running",
        crate::domain::HealthCheckKind::ContainerHealth => "container_health",
    }
}

fn last_update_label(last_attempt: Option<&UpdateAttemptRecord>) -> String {
    last_attempt
        .map(|attempt| update_status_label(attempt.status).to_string())
        .unwrap_or_else(|| "none yet".into())
}

fn validation_status_label(status: ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::Accepted => "accepted",
        ValidationStatus::Rejected => "rejected",
    }
}

fn image_import_status_label(status: ImageImportStatus) -> &'static str {
    match status {
        ImageImportStatus::Imported => "imported",
        ImageImportStatus::Failed => "failed",
    }
}

fn update_status_label(status: UpdateAttemptStatus) -> &'static str {
    match status {
        UpdateAttemptStatus::Applying => "applying",
        UpdateAttemptStatus::HealthChecking => "health checking",
        UpdateAttemptStatus::Succeeded => "succeeded",
        UpdateAttemptStatus::Failed => "failed",
        UpdateAttemptStatus::RollbackStarted => "rolling back",
        UpdateAttemptStatus::RolledBack => "rolled back",
        UpdateAttemptStatus::RollbackFailed => "rollback failed",
    }
}

fn update_summary_sentence(status: UpdateAttemptStatus) -> &'static str {
    match status {
        UpdateAttemptStatus::Succeeded => "the package was loaded and the service stayed healthy",
        UpdateAttemptStatus::RolledBack => {
            "the update failed, so the service was moved back to the previous image"
        }
        UpdateAttemptStatus::RollbackFailed => {
            "the update failed and the automatic rollback did not finish cleanly"
        }
        UpdateAttemptStatus::Failed => "the update did not finish cleanly",
        UpdateAttemptStatus::Applying => "the update is still marked as applying",
        UpdateAttemptStatus::HealthChecking => "the update is still waiting on health checks",
        UpdateAttemptStatus::RollbackStarted => "rollback started but did not finish yet",
    }
}

fn health_check_outcome_label(outcome: crate::domain::HealthCheckOutcome) -> &'static str {
    match outcome {
        crate::domain::HealthCheckOutcome::Passed => "passed",
        crate::domain::HealthCheckOutcome::Failed => "failed",
        crate::domain::HealthCheckOutcome::TimedOut => "timed out",
    }
}

fn audit_kind_label(kind: AuditEventKind) -> &'static str {
    match kind {
        AuditEventKind::ValidationStarted => "validation_started",
        AuditEventKind::ValidationAccepted => "validation_accepted",
        AuditEventKind::ValidationRejected => "validation_rejected",
        AuditEventKind::ImageImportStarted => "image_import_started",
        AuditEventKind::ImageImportSucceeded => "image_import_succeeded",
        AuditEventKind::ImageImportFailed => "image_import_failed",
        AuditEventKind::UpdateStarted => "update_started",
        AuditEventKind::HealthCheckStarted => "health_check_started",
        AuditEventKind::HealthCheckPassed => "health_check_passed",
        AuditEventKind::HealthCheckFailed => "health_check_failed",
        AuditEventKind::RollbackStarted => "rollback_started",
        AuditEventKind::RollbackSucceeded => "rollback_succeeded",
        AuditEventKind::RollbackFailed => "rollback_failed",
        AuditEventKind::UpdateCommitted => "update_committed",
    }
}

fn format_timestamp(timestamp: OffsetDateTime) -> String {
    timestamp
        .format(&Rfc3339)
        .unwrap_or_else(|_| timestamp.unix_timestamp().to_string())
}

fn exit_code_for_update_status(status: UpdateAttemptStatus) -> ExitCode {
    match status {
        UpdateAttemptStatus::Succeeded => ExitCode::SUCCESS,
        UpdateAttemptStatus::RollbackFailed => ExitCode::from(5),
        UpdateAttemptStatus::Failed
        | UpdateAttemptStatus::RolledBack
        | UpdateAttemptStatus::Applying
        | UpdateAttemptStatus::HealthChecking
        | UpdateAttemptStatus::RollbackStarted => ExitCode::from(4),
    }
}

trait PackageWorkflow {
    fn validate_package(
        &self,
        catalog: ServiceCatalog,
        package: PathBuf,
        state_dir: PathBuf,
    ) -> Result<ValidationRecord, CliError>;

    fn import_validated_image(
        &self,
        validation_record: ValidationRecord,
        state_dir: PathBuf,
    ) -> Result<ImageImportRecord, CliError>;

    fn apply_candidate_release(
        &self,
        catalog: ServiceCatalog,
        candidate_release_id: Uuid,
        state_dir: PathBuf,
    ) -> Result<UpdateAttemptRecord, CliError>;
}

#[derive(Debug, Clone, Copy)]
struct RealPackageWorkflow;

impl PackageWorkflow for RealPackageWorkflow {
    fn validate_package(
        &self,
        catalog: ServiceCatalog,
        package: PathBuf,
        state_dir: PathBuf,
    ) -> Result<ValidationRecord, CliError> {
        let store = FilesystemStore::new(state_dir);
        let updater_version = Version::parse(UPDATER_VERSION).map_err(CliError::VersionParse)?;
        let app = ValidationApp::filesystem(catalog, store, updater_version);

        app.validate_package(ValidatePackageRequest {
            package_path: package,
        })
        .map_err(CliError::Package)
    }

    fn import_validated_image(
        &self,
        validation_record: ValidationRecord,
        state_dir: PathBuf,
    ) -> Result<ImageImportRecord, CliError> {
        let docker = BollardDockerClient::connect_local_defaults()?;
        let app = ImageImportApp::filesystem(docker, FilesystemStore::new(state_dir));

        app.import_validated_image(ImportValidatedImageRequest { validation_record })
            .map_err(CliError::Import)
    }

    fn apply_candidate_release(
        &self,
        catalog: ServiceCatalog,
        candidate_release_id: Uuid,
        state_dir: PathBuf,
    ) -> Result<UpdateAttemptRecord, CliError> {
        let docker = BollardDockerClient::connect_local_defaults()?;
        let app = UpdateApp::filesystem(catalog, docker, FilesystemStore::new(state_dir));

        app.apply_candidate_release(ApplyCandidateReleaseRequest {
            candidate_release_id,
        })
        .map_err(CliError::Update)
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "caisson",
    version = UPDATER_VERSION,
    about = "Offline Docker service updater",
    propagate_version = true
)]
struct Cli {
    /// Path to `services.toml`.
    #[arg(long, global = true, default_value = "services.toml")]
    services: PathBuf,
    /// Root directory for local state, staging, and audit files.
    #[arg(long, global = true, default_value = ".caisson-state")]
    state_dir: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect predefined services and their locally known state.
    Service {
        #[command(subcommand)]
        command: ServiceCommands,
    },
    /// Work with local update packages.
    Package {
        #[command(subcommand)]
        command: PackageCommands,
    },
    /// Read local update history and audit events.
    History {
        #[command(subcommand)]
        command: HistoryCommands,
    },
}

#[derive(Debug, Subcommand)]
enum ServiceCommands {
    /// List all predefined services.
    List,
    /// Show one predefined service in more detail.
    Show {
        /// Name of the predefined service.
        service: String,
    },
}

#[derive(Debug, Subcommand)]
enum PackageCommands {
    /// Validate a local `.edgepkg` against the service catalog.
    Validate {
        /// Path to the local `.edgepkg` file.
        package: PathBuf,
    },
    /// Validate, import, and apply a local `.edgepkg`.
    Load {
        /// Path to the local `.edgepkg` file.
        package: PathBuf,
        /// Skip the confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HistoryCommands {
    /// List recent locally recorded update attempts.
    List {
        /// Only show update attempts for one service.
        #[arg(long)]
        service: Option<String>,
        /// Maximum number of update attempts to show.
        #[arg(long)]
        limit: Option<usize>,
    },
    /// Show one locally recorded update attempt in detail.
    Show {
        /// Identifier of the locally recorded update attempt.
        update_id: Uuid,
    },
}

/// Errors from CLI.
#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("failed to parse crate version `{UPDATER_VERSION}` as semver: {0}")]
    VersionParse(semver::Error),
    #[error("{0}")]
    Package(#[from] crate::package::PackageIntakeError),
    #[error("{0}")]
    Docker(#[from] DockerClientError),
    #[error("{0}")]
    Import(#[from] ImageImportError),
    #[error("{0}")]
    Update(#[from] UpdateError),
    #[error("{0}")]
    Persistence(#[from] PersistenceError),
    #[error("failed to read or write CLI I/O: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Lookup(String),
    #[error("{0}")]
    Invariant(String),
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::str::FromStr;

    use clap::Parser;
    use std::io::Cursor;
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    use uuid::Uuid;

    use super::{Cli, CliError, PackageWorkflow, execute};
    use crate::domain::{
        AuditEvent, AuditEventKind, HealthCheckKind, HealthCheckOutcome, HealthCheckReport,
        ImageImportRecord, ImportedImageMetadata, PackageManifest, RuntimeModeKind,
        ServiceStateRecord, UpdateAttemptRecord, ValidationIssue, ValidationRecord,
    };
    use crate::persistence::{FilesystemStore, StateStore};

    #[derive(Debug, Clone)]
    struct FakeWorkflow {
        validation_record: ValidationRecord,
        import_record: ImageImportRecord,
        update_record: UpdateAttemptRecord,
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl FakeWorkflow {
        fn new(
            validation_record: ValidationRecord,
            import_record: ImageImportRecord,
            update_record: UpdateAttemptRecord,
        ) -> Self {
            Self {
                validation_record,
                import_record,
                update_record,
                calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().expect("lock").clone()
        }
    }

    impl PackageWorkflow for FakeWorkflow {
        fn validate_package(
            &self,
            _catalog: crate::domain::ServiceCatalog,
            _package: PathBuf,
            _state_dir: PathBuf,
        ) -> Result<ValidationRecord, CliError> {
            self.calls.lock().expect("lock").push("validate".into());
            Ok(self.validation_record.clone())
        }

        fn import_validated_image(
            &self,
            _validation_record: ValidationRecord,
            _state_dir: PathBuf,
        ) -> Result<ImageImportRecord, CliError> {
            self.calls.lock().expect("lock").push("import".into());
            Ok(self.import_record.clone())
        }

        fn apply_candidate_release(
            &self,
            _catalog: crate::domain::ServiceCatalog,
            _candidate_release_id: Uuid,
            _state_dir: PathBuf,
        ) -> Result<UpdateAttemptRecord, CliError> {
            self.calls.lock().expect("lock").push("apply".into());
            Ok(self.update_record.clone())
        }
    }

    #[test]
    fn package_load_stops_after_operator_cancel() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let workflow = FakeWorkflow::new(
            accepted_validation_record(temp_dir.path().join("frontend.edgepkg")),
            imported_image_record(),
            succeeded_update_record(),
        );
        let cli = parse_cli(vec![
            "caisson".into(),
            "--services".into(),
            fixture_path("services.valid.toml"),
            "--state-dir".into(),
            temp_dir.path().display().to_string(),
            "package".into(),
            "load".into(),
            temp_dir
                .path()
                .join("frontend.edgepkg")
                .display()
                .to_string(),
        ]);
        let mut input = Cursor::new(b"n\n".to_vec());
        let mut output = Vec::new();

        let exit_code = execute(cli, &workflow, &mut input, &mut output).expect("command");
        let rendered = String::from_utf8(output).expect("utf8");

        assert_eq!(exit_code, ExitCode::from(3));
        assert!(rendered.contains("Ready to load package"));
        assert!(rendered.contains("Cancelled before changing the service."));
        assert_eq!(workflow.calls(), vec!["validate"]);
    }

    #[test]
    fn package_load_runs_full_flow_with_yes_flag() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let workflow = FakeWorkflow::new(
            accepted_validation_record(temp_dir.path().join("frontend.edgepkg")),
            imported_image_record(),
            succeeded_update_record(),
        );
        let cli = parse_cli(vec![
            "caisson".into(),
            "--services".into(),
            fixture_path("services.valid.toml"),
            "--state-dir".into(),
            temp_dir.path().display().to_string(),
            "package".into(),
            "load".into(),
            "--yes".into(),
            temp_dir
                .path()
                .join("frontend.edgepkg")
                .display()
                .to_string(),
        ]);
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();

        let exit_code = execute(cli, &workflow, &mut input, &mut output).expect("command");
        let rendered = String::from_utf8(output).expect("utf8");

        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert!(rendered.contains("Validating package..."));
        assert!(rendered.contains("Importing image into Docker..."));
        assert!(rendered.contains("Applying update..."));
        assert!(rendered.contains("Status: succeeded"));
        assert_eq!(workflow.calls(), vec!["validate", "import", "apply"]);
    }

    #[test]
    fn package_validate_returns_rejected_exit_code_and_issues() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let workflow = FakeWorkflow::new(
            rejected_validation_record(temp_dir.path().join("frontend.edgepkg")),
            imported_image_record(),
            succeeded_update_record(),
        );
        let cli = parse_cli(vec![
            "caisson".into(),
            "--services".into(),
            fixture_path("services.valid.toml"),
            "--state-dir".into(),
            temp_dir.path().display().to_string(),
            "package".into(),
            "validate".into(),
            temp_dir
                .path()
                .join("frontend.edgepkg")
                .display()
                .to_string(),
        ]);
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();

        let exit_code = execute(cli, &workflow, &mut input, &mut output).expect("command");
        let rendered = String::from_utf8(output).expect("utf8");

        assert_eq!(exit_code, ExitCode::from(2));
        assert!(rendered.contains("Status: rejected"));
        assert!(rendered.contains("service is not present in services.toml"));
        assert_eq!(workflow.calls(), vec!["validate"]);
    }

    #[test]
    fn service_list_reads_local_state_and_latest_attempt() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(temp_dir.path());
        store
            .save_service_state(&ServiceStateRecord::new(
                "frontend".into(),
                None,
                "example/frontend:1.2.3".into(),
                None,
                Some("example/frontend:current".into()),
                parse_time("2026-03-17T12:00:00Z"),
            ))
            .expect("save state");
        let mut attempt = UpdateAttemptRecord::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "frontend".into(),
            RuntimeModeKind::Docker,
            "example/frontend:1.2.3".into(),
            None,
            Some("example/frontend:current".into()),
            parse_time("2026-03-17T11:00:00Z"),
        );
        attempt.mark_succeeded(parse_time("2026-03-17T11:05:00Z"));
        store.save_update_attempt(&attempt).expect("save attempt");

        let cli = parse_cli(vec![
            "caisson".into(),
            "--services".into(),
            fixture_path("services.valid.toml"),
            "--state-dir".into(),
            temp_dir.path().display().to_string(),
            "service".into(),
            "list".into(),
        ]);
        let workflow = FakeWorkflow::new(
            accepted_validation_record(temp_dir.path().join("frontend.edgepkg")),
            imported_image_record(),
            succeeded_update_record(),
        );
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();

        let exit_code = execute(cli, &workflow, &mut input, &mut output).expect("command");
        let rendered = String::from_utf8(output).expect("utf8");

        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert!(rendered.contains("frontend | runtime: docker | active image: example/frontend:1.2.3 | last update: succeeded"));
        assert!(rendered.contains("backend | runtime: compose"));
        assert!(workflow.calls().is_empty());
    }

    #[test]
    fn service_show_displays_runtime_specific_details() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let cli = parse_cli(vec![
            "caisson".into(),
            "--services".into(),
            fixture_path("services.valid.toml"),
            "--state-dir".into(),
            temp_dir.path().display().to_string(),
            "service".into(),
            "show".into(),
            "backend".into(),
        ]);
        let workflow = FakeWorkflow::new(
            accepted_validation_record(temp_dir.path().join("frontend.edgepkg")),
            imported_image_record(),
            succeeded_update_record(),
        );
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();

        let exit_code = execute(cli, &workflow, &mut input, &mut output).expect("command");
        let rendered = String::from_utf8(output).expect("utf8");

        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert!(rendered.contains("Service: backend"));
        assert!(rendered.contains("Runtime: compose"));
        assert!(rendered.contains("Compose project: caisson-stack"));
        assert!(rendered.contains("Compose service: backend"));
        assert!(workflow.calls().is_empty());
    }

    #[test]
    fn history_show_combines_validation_and_update_events() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(temp_dir.path());
        let validation_attempt_id =
            Uuid::from_str("00000000-0000-0000-0000-000000000011").expect("uuid");
        let update_id = Uuid::from_str("00000000-0000-0000-0000-000000000012").expect("uuid");
        let mut attempt = UpdateAttemptRecord::new(
            update_id,
            Uuid::new_v4(),
            validation_attempt_id,
            "frontend".into(),
            RuntimeModeKind::Docker,
            "example/frontend:1.2.3".into(),
            None,
            Some("example/frontend:current".into()),
            parse_time("2026-03-17T11:00:00Z"),
        );
        attempt.health_check = Some(HealthCheckReport {
            kind: HealthCheckKind::Running,
            outcome: HealthCheckOutcome::Passed,
            message: "container is still running".into(),
            checked_at: parse_time("2026-03-17T11:02:00Z"),
        });
        attempt.mark_succeeded(parse_time("2026-03-17T11:03:00Z"));
        store.save_update_attempt(&attempt).expect("save attempt");
        store
            .append_audit_event(&AuditEvent {
                event_id: Uuid::from_str("00000000-0000-0000-0000-000000000021").expect("uuid"),
                attempt_id: validation_attempt_id,
                occurred_at: parse_time("2026-03-17T10:59:00Z"),
                kind: AuditEventKind::ValidationAccepted,
                message: "accepted package for service `frontend`".into(),
            })
            .expect("save event");
        store
            .append_audit_event(&AuditEvent {
                event_id: Uuid::from_str("00000000-0000-0000-0000-000000000022").expect("uuid"),
                attempt_id: update_id,
                occurred_at: parse_time("2026-03-17T11:01:00Z"),
                kind: AuditEventKind::UpdateStarted,
                message: "started applying image `example/frontend:1.2.3` to service `frontend`"
                    .into(),
            })
            .expect("save event");

        let cli = parse_cli(vec![
            "caisson".into(),
            "--state-dir".into(),
            temp_dir.path().display().to_string(),
            "history".into(),
            "show".into(),
            update_id.to_string(),
        ]);
        let workflow = FakeWorkflow::new(
            accepted_validation_record(temp_dir.path().join("frontend.edgepkg")),
            imported_image_record(),
            succeeded_update_record(),
        );
        let mut input = Cursor::new(Vec::new());
        let mut output = Vec::new();

        let exit_code = execute(cli, &workflow, &mut input, &mut output).expect("command");
        let rendered = String::from_utf8(output).expect("utf8");

        assert_eq!(exit_code, ExitCode::SUCCESS);
        assert!(rendered.contains("Status: succeeded"));
        assert!(rendered.contains("validation_accepted"));
        assert!(rendered.contains("update_started"));
        assert!(workflow.calls().is_empty());
    }

    fn parse_cli(args: Vec<String>) -> Cli {
        Cli::try_parse_from(args).expect("cli parse")
    }

    fn accepted_validation_record(package_path: PathBuf) -> ValidationRecord {
        let manifest = load_manifest_fixture("manifests/valid-frontend.toml");
        let mut record = ValidationRecord::new(
            Uuid::new_v4(),
            package_path,
            parse_time("2026-03-17T10:00:00Z"),
        );
        record.manifest = Some(manifest);
        record.staged_path = Some(PathBuf::from("/tmp/staging/frontend.edgepkg"));
        record.accept();
        record
    }

    fn rejected_validation_record(package_path: PathBuf) -> ValidationRecord {
        let mut record = ValidationRecord::new(
            Uuid::new_v4(),
            package_path,
            parse_time("2026-03-17T10:00:00Z"),
        );
        record.reject_with(ValidationIssue::new(
            "manifest.unknown_service",
            "service is not present in services.toml",
        ));
        record
    }

    fn imported_image_record() -> ImageImportRecord {
        let mut record = ImageImportRecord::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            "frontend".into(),
            "example/frontend:1.2.3".into(),
            semver::Version::parse("1.2.3").expect("version"),
            parse_time("2026-03-17T10:01:00Z"),
        );
        record.mark_imported(
            ImportedImageMetadata {
                image_id: "sha256:frontend".into(),
                repo_tags: vec!["example/frontend:1.2.3".into()],
                repo_digests: vec!["example/frontend@sha256:frontend".into()],
                architecture: Some("amd64".into()),
                os: Some("linux".into()),
            },
            Uuid::from_str("00000000-0000-0000-0000-000000000031").expect("uuid"),
        );
        record
    }

    fn succeeded_update_record() -> UpdateAttemptRecord {
        let mut record = UpdateAttemptRecord::new(
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            "frontend".into(),
            RuntimeModeKind::Docker,
            "example/frontend:1.2.3".into(),
            None,
            Some("example/frontend:current".into()),
            parse_time("2026-03-17T10:02:00Z"),
        );
        record.health_check = Some(HealthCheckReport {
            kind: HealthCheckKind::Running,
            outcome: HealthCheckOutcome::Passed,
            message: "container kept running".into(),
            checked_at: parse_time("2026-03-17T10:03:00Z"),
        });
        record.mark_succeeded(parse_time("2026-03-17T10:04:00Z"));
        record
    }

    fn fixture_path(relative_path: &str) -> String {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(relative_path)
            .display()
            .to_string()
    }

    fn load_manifest_fixture(relative_path: &str) -> PackageManifest {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(relative_path);
        let contents = std::fs::read_to_string(path).expect("fixture");
        toml::from_str(&contents).expect("manifest")
    }

    fn parse_time(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &Rfc3339).expect("timestamp")
    }
}
