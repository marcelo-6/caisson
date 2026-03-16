//! Command line entrypoints for the updater.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use semver::Version;
use thiserror::Error;

use crate::UPDATER_VERSION;
use crate::app::{ValidatePackageRequest, ValidationApp};
use crate::config::{ConfigError, load_service_catalog};
use crate::domain::{ValidationRecord, ValidationStatus};
use crate::persistence::FilesystemStore;

/// Runs the CLI and returns the desired exit code.
pub fn run() -> Result<ExitCode, CliError> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Validate {
            package,
            services,
            state_dir,
        } => {
            let catalog = load_service_catalog(&services)?;
            let store = FilesystemStore::new(state_dir);
            let updater_version =
                Version::parse(UPDATER_VERSION).map_err(CliError::VersionParse)?;
            let app = ValidationApp::filesystem(catalog, store, updater_version);
            let record = app.validate_package(ValidatePackageRequest {
                package_path: package,
            })?;

            print_record(&record);

            let exit_code = match record.status {
                ValidationStatus::Accepted => ExitCode::SUCCESS,
                ValidationStatus::Rejected => ExitCode::from(2),
            };

            Ok(exit_code)
        }
    }
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
}

fn print_record(record: &ValidationRecord) {
    println!("attempt_id: {}", record.attempt_id);
    println!("status: {:?}", record.status);
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
        println!("issues: none");
    } else {
        println!("issues:");
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
}
