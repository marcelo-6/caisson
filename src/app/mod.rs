//! Application layer wrappers around the package validation and image
//! import workflows.
//!
//! The CLI should not need to know how validation, staging, Docker import, and
//! persistence are stitched together.

use std::path::{Path, PathBuf};

use semver::Version;

use crate::docker::{DockerImageClient, ImageImportError, ImageImportService};
use crate::domain::{ImageImportRecord, ServiceCatalog, ValidationRecord};
use crate::package::{PackageIntakeError, PackageIntakeService};
use crate::persistence::{FilesystemStore, StateStore};

/// Input for the package-validation use case.
#[derive(Debug, Clone)]
pub struct ValidatePackageRequest {
    pub package_path: PathBuf,
}

/// Input for the Docker image-import use case.
#[derive(Debug, Clone)]
pub struct ImportValidatedImageRequest {
    pub validation_record: ValidationRecord,
}

/// Application facade used by the CLI and tests for package validation.
#[derive(Debug)]
pub struct ValidationApp<S> {
    package_intake: PackageIntakeService<S>,
    staging_root: PathBuf,
}

impl ValidationApp<FilesystemStore> {
    /// Creates an app that uses the filesystem store.
    pub fn filesystem(
        catalog: ServiceCatalog,
        store: FilesystemStore,
        current_updater_version: Version,
    ) -> Self {
        let staging_root = store.root().join("staging");

        Self {
            package_intake: PackageIntakeService::new(catalog, store, current_updater_version),
            staging_root,
        }
    }
}

impl<S> ValidationApp<S>
where
    S: StateStore,
{
    /// Validates a package and persists the result locally.
    pub fn validate_package(
        &self,
        request: ValidatePackageRequest,
    ) -> Result<ValidationRecord, PackageIntakeError> {
        self.package_intake
            .validate_package(request.package_path, &self.staging_root)
    }

    /// Returns the staging root used by the app.
    #[must_use]
    pub fn staging_root(&self) -> &Path {
        &self.staging_root
    }
}

/// Application facade used by the CLI and tests for Docker image import.
#[derive(Debug)]
pub struct ImageImportApp<D, S> {
    image_import: ImageImportService<D, S>,
}

impl<D> ImageImportApp<D, FilesystemStore>
where
    D: DockerImageClient,
{
    /// Creates a filesystem-backed image-import app.
    pub fn filesystem(docker: D, store: FilesystemStore) -> Self {
        Self {
            image_import: ImageImportService::new(docker, store),
        }
    }
}

impl<D, S> ImageImportApp<D, S>
where
    D: DockerImageClient,
    S: StateStore,
{
    /// Imports a Docker image from a previously accepted validation record.
    pub fn import_validated_image(
        &self,
        request: ImportValidatedImageRequest,
    ) -> Result<ImageImportRecord, ImageImportError> {
        self.image_import
            .import_validated_package(&request.validation_record)
    }
}
