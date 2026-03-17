//! Command line entrypoints for the updater.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use semver::Version;
use thiserror::Error;

use crate::UPDATER_VERSION;
use crate::app::{
    ApplyCandidateReleaseRequest, ImageImportApp, ImportValidatedImageRequest, UpdateApp,
    ValidatePackageRequest, ValidationApp,
};
use crate::compose::ShellComposeClient;
use crate::config::{ConfigError, load_service_catalog};
use crate::docker::{BollardDockerClient, DockerClientError, ImageImportError};
use crate::domain::{
    ImageImportRecord, ImageImportStatus, UpdateAttemptRecord, UpdateAttemptStatus,
    ValidationRecord, ValidationStatus,
};
use crate::persistence::FilesystemStore;
use crate::update::UpdateError;

/// Runs the CLI and returns the desired exit code.
pub fn run() -> Result<ExitCode, CliError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Validate {
            package,
            services,
            state_dir,
        } => {
            let record = validate_package(package, services, state_dir)?;
            print_validation_record(&record);

            Ok(match record.status {
                ValidationStatus::Accepted => ExitCode::SUCCESS,
                ValidationStatus::Rejected => ExitCode::from(2),
            })
        }
        Commands::ImportImage {
            package,
            services,
            state_dir,
        } => {
            let state_dir_for_validation = state_dir.clone();
            let validation_record = validate_package(package, services, state_dir_for_validation)?;
            print_validation_record(&validation_record);

            if validation_record.status == ValidationStatus::Rejected {
                return Ok(ExitCode::from(2));
            }

            let docker = BollardDockerClient::connect_local_defaults()?;
            let import_app = ImageImportApp::filesystem(docker, FilesystemStore::new(state_dir));
            let import_record = import_app
                .import_validated_image(ImportValidatedImageRequest { validation_record })?;
            print_image_import_record(&import_record);

            Ok(match import_record.status {
                ImageImportStatus::Imported => ExitCode::SUCCESS,
                ImageImportStatus::Failed => ExitCode::from(3),
            })
        }
        Commands::ApplyPackage {
            package,
            services,
            state_dir,
        } => {
            let catalog = load_service_catalog(&services)?;
            let validation_record =
                validate_package_with_catalog(catalog.clone(), package, state_dir.clone())?;
            print_validation_record(&validation_record);

            if validation_record.status == ValidationStatus::Rejected {
                return Ok(ExitCode::from(2));
            }

            let docker = BollardDockerClient::connect_local_defaults()?;
            let store = FilesystemStore::new(state_dir);
            let import_app = ImageImportApp::filesystem(docker.clone(), store.clone());
            let import_record = import_app
                .import_validated_image(ImportValidatedImageRequest { validation_record })?;
            print_image_import_record(&import_record);

            if import_record.status == ImageImportStatus::Failed {
                return Ok(ExitCode::from(3));
            }

            let candidate_release_id = import_record
                .candidate_release_id
                .expect("successful import should produce a candidate release");
            let update_app = UpdateApp::new(catalog, docker, ShellComposeClient, store);
            let update_record =
                update_app.apply_candidate_release(ApplyCandidateReleaseRequest {
                    candidate_release_id,
                })?;
            print_update_attempt_record(&update_record);

            Ok(match update_record.status {
                UpdateAttemptStatus::Succeeded => ExitCode::SUCCESS,
                UpdateAttemptStatus::RollbackFailed => ExitCode::from(5),
                UpdateAttemptStatus::Failed
                | UpdateAttemptStatus::RolledBack
                | UpdateAttemptStatus::Applying
                | UpdateAttemptStatus::HealthChecking
                | UpdateAttemptStatus::RollbackStarted => ExitCode::from(4),
            })
        }
    }
}

fn validate_package(
    package: PathBuf,
    services: PathBuf,
    state_dir: PathBuf,
) -> Result<ValidationRecord, CliError> {
    let catalog = load_service_catalog(&services)?;
    validate_package_with_catalog(catalog, package, state_dir)
}

fn validate_package_with_catalog(
    catalog: crate::domain::ServiceCatalog,
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

#[derive(Debug, Parser)]
#[command(name = "caisson", version = UPDATER_VERSION, about = "Offline Docker service updater")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Validate a local `.edgepkg` against the service catalog.
    Validate {
        /// Path to the local `.edgepkg` file.
        package: PathBuf,
        /// Path to `services.toml`.
        #[arg(long, default_value = "services.toml")]
        services: PathBuf,
        /// Root directory for local state, staging, and audit files.
        #[arg(long, default_value = ".caisson-state")]
        state_dir: PathBuf,
    },
    /// Validate a package and import its staged `image.tar` into Docker.
    ImportImage {
        /// Path to the local `.edgepkg` file.
        package: PathBuf,
        /// Path to `services.toml`.
        #[arg(long, default_value = "services.toml")]
        services: PathBuf,
        /// Root directory for local state, staging, and audit files.
        #[arg(long, default_value = ".caisson-state")]
        state_dir: PathBuf,
    },
    /// Validate, import, and apply a local `.edgepkg`.
    ApplyPackage {
        /// Path to the local `.edgepkg` file.
        package: PathBuf,
        /// Path to `services.toml`.
        #[arg(long, default_value = "services.toml")]
        services: PathBuf,
        /// Root directory for local state, staging, and audit files.
        #[arg(long, default_value = ".caisson-state")]
        state_dir: PathBuf,
    },
}

fn print_validation_record(record: &ValidationRecord) {
    println!("attempt_id: {}", record.attempt_id);
    println!("validation_status: {:?}", record.status);
    println!("source_path: {}", record.source_path.display());

    if let Some(staged_path) = record.staged_path.as_ref() {
        println!("staged_path: {}", staged_path.display());
    }

    if let Some(manifest) = record.manifest.as_ref() {
        println!("service: {}", manifest.target.service);
        println!("service_revision: {}", manifest.target.service_revision);
        println!("platform: {}", manifest.target.platform);
        println!("package_version: {}", manifest.package_version);
        println!("image_reference: {}", manifest.image.reference);
    }

    if let Some(image_archive) = record.image_archive.as_ref() {
        println!("image_entry: {}", image_archive.entry_name);
        println!("image_size_bytes: {}", image_archive.size_bytes);
    }

    if record.issues.is_empty() {
        println!("validation_issues: none");
    } else {
        println!("validation_issues:");
        for issue in &record.issues {
            println!("- [{}] {}", issue.code, issue.message);
        }
    }
}

fn print_image_import_record(record: &ImageImportRecord) {
    println!("import_id: {}", record.import_id);
    println!("image_import_status: {:?}", record.status);
    println!("service: {}", record.service_name);
    println!("image_reference: {}", record.image_reference);

    if let Some(candidate_release_id) = record.candidate_release_id {
        println!("candidate_release_id: {candidate_release_id}");
    }

    if let Some(imported_image) = record.imported_image.as_ref() {
        println!("imported_image_id: {}", imported_image.image_id);
        println!("imported_repo_tags: {:?}", imported_image.repo_tags);
        println!("imported_repo_digests: {:?}", imported_image.repo_digests);
        if let Some(os) = imported_image.os.as_ref() {
            println!("imported_os: {os}");
        }
        if let Some(architecture) = imported_image.architecture.as_ref() {
            println!("imported_architecture: {architecture}");
        }
    }

    if record.issues.is_empty() {
        println!("image_import_issues: none");
    } else {
        println!("image_import_issues:");
        for issue in &record.issues {
            println!("- [{}] {}", issue.code, issue.message);
        }
    }
}

fn print_update_attempt_record(record: &UpdateAttemptRecord) {
    println!("update_id: {}", record.update_id);
    println!("update_status: {:?}", record.status);
    println!("service: {}", record.service_name);
    println!("runtime_mode: {:?}", record.runtime_mode);
    println!("target_image_reference: {}", record.target_image_reference);

    if let Some(previous_image_reference) = record.previous_image_reference.as_ref() {
        println!("previous_image_reference: {previous_image_reference}");
    }

    if let Some(health_check) = record.health_check.as_ref() {
        println!("health_check_kind: {:?}", health_check.kind);
        println!("health_check_outcome: {:?}", health_check.outcome);
        println!("health_check_message: {}", health_check.message);
    }

    if record.issues.is_empty() {
        println!("update_issues: none");
    } else {
        println!("update_issues:");
        for issue in &record.issues {
            println!("- [{}] {}", issue.code, issue.message);
        }
    }
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
}
