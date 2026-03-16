//! Application- ayer wrapper around the package intake workflow.
//!
//! This keeps the CLI from needing to know how validation, staging, and
//! persistence are stitched together.

use std::path::{Path, PathBuf};

use semver::Version;

use crate::domain::{ServiceCatalog, ValidationRecord};
use crate::package::{PackageIntakeError, PackageIntakeService};
use crate::persistence::{FilesystemStore, StateStore};

/// Input for the package-validation use case.
#[derive(Debug, Clone)]
pub struct ValidatePackageRequest {
    pub package_path: PathBuf,
}

/// Small application facade used by the CLI and tests.
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
