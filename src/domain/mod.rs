//! Domain types shared across the crate.

use std::fmt::{self, Display};
use std::path::PathBuf;

use semver::Version;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// The only manifest format version accepted right now.
pub const SUPPORTED_MANIFEST_FORMAT_VERSION: u64 = 1;

/// The only service-catalog format version accepted right now.
pub const SUPPORTED_SERVICE_CATALOG_VERSION: u64 = 1;

/// A predefined service from `services.toml`.
///
/// Think of this as the one blessed description of what the updater is allowed
/// to touch. If a service is not in here, the updater should act like it does
/// not exist.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ManagedService {
    pub name: String,
    pub service_revision: String,
    pub platform: String,
    pub runtime: RuntimeMode,
}

/// The supported runtime modes for predefined services.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuntimeMode {
    Docker(DockerRuntime),
    Compose(ComposeRuntime),
}

/// Runtime details for a Docker service.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct DockerRuntime {
    pub container_name: String,
    pub image_reference: String,
}

/// Runtime details for a constrained Compose service.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ComposeRuntime {
    pub project: String,
    pub file: PathBuf,
    pub service: String,
}

/// The full predefined service catalog.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ServiceCatalog {
    pub catalog_version: u64,
    pub services: Vec<ManagedService>,
}

impl ServiceCatalog {
    /// Looks up a service by name.
    #[must_use]
    pub fn find_service(&self, name: &str) -> Option<&ManagedService> {
        self.services.iter().find(|service| service.name == name)
    }
}

/// The manifest parsed from `manifest.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PackageManifest {
    pub format_version: u64,
    pub package_type: PackageType,
    pub package_version: Version,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub target: PackageTarget,
    pub image: ImageSpec,
    pub compatibility: CompatibilitySpec,
}

/// The package kind accepted by the baseline updater.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PackageType {
    Service,
}

impl PackageType {
    /// Returns the TOML value for the package type.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
        }
    }
}

impl Serialize for PackageType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PackageType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;

        match raw.as_str() {
            "service" | "service-update" => Ok(Self::Service),
            _ => Err(serde::de::Error::custom(format!(
                "unsupported package_type `{raw}`"
            ))),
        }
    }
}

/// Target selection data from the package manifest.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct PackageTarget {
    pub service: String,
    pub service_revision: String,
    pub platform: String,
}

/// Image metadata from the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ImageSpec {
    pub reference: String,
}

/// Optional compatibility hints accepted in the baseline manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub struct CompatibilitySpec {
    pub min_updater_version: Option<Version>,
}

/// Metadata about the staged `image.tar` entry.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ImageArchiveMetadata {
    pub entry_name: String,
    pub size_bytes: u64,
}

/// Normalized metadata captured after a Docker image is imported and inspected.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ImportedImageMetadata {
    pub image_id: String,
    pub repo_tags: Vec<String>,
    pub repo_digests: Vec<String>,
    pub architecture: Option<String>,
    pub os: Option<String>,
}

/// Validation status for a package intake attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Accepted,
    Rejected,
}

/// Import status for a Docker image-load attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageImportStatus {
    Imported,
    Failed,
}

/// A single actionable validation or import issue.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
}

impl ValidationIssue {
    /// Helper for issue value.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl Display for ValidationIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}] {}", self.code, self.message)
    }
}

/// The persisted result of one package validation attempt.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ValidationRecord {
    pub attempt_id: Uuid,
    pub status: ValidationStatus,
    pub source_path: PathBuf,
    pub staged_path: Option<PathBuf>,
    pub source_file_size_bytes: Option<u64>,
    #[serde(with = "time::serde::rfc3339")]
    pub validated_at: OffsetDateTime,
    pub manifest: Option<PackageManifest>,
    pub image_archive: Option<ImageArchiveMetadata>,
    pub issues: Vec<ValidationIssue>,
}

impl ValidationRecord {
    /// Creates a fresh rejected record.
    ///
    /// Starting from "rejected until proven otherwise".
    #[must_use]
    pub fn new(attempt_id: Uuid, source_path: PathBuf, validated_at: OffsetDateTime) -> Self {
        Self {
            attempt_id,
            status: ValidationStatus::Rejected,
            source_path,
            staged_path: None,
            source_file_size_bytes: None,
            validated_at,
            manifest: None,
            image_archive: None,
            issues: Vec::new(),
        }
    }

    /// Marks the record as accepted.
    pub fn accept(&mut self) {
        self.status = ValidationStatus::Accepted;
    }

    /// Adds an issue and keeps the record rejected.
    pub fn reject_with(&mut self, issue: ValidationIssue) {
        self.status = ValidationStatus::Rejected;
        self.issues.push(issue);
    }
}

/// The persisted result of one Docker image-import attempt.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ImageImportRecord {
    pub import_id: Uuid,
    pub validation_attempt_id: Uuid,
    pub status: ImageImportStatus,
    pub service_name: String,
    pub image_reference: String,
    pub package_version: Version,
    #[serde(with = "time::serde::rfc3339")]
    pub imported_at: OffsetDateTime,
    pub imported_image: Option<ImportedImageMetadata>,
    pub candidate_release_id: Option<Uuid>,
    pub issues: Vec<ValidationIssue>,
}

impl ImageImportRecord {
    /// Creates a failed import record.
    #[must_use]
    pub fn new(
        import_id: Uuid,
        validation_attempt_id: Uuid,
        service_name: String,
        image_reference: String,
        package_version: Version,
        imported_at: OffsetDateTime,
    ) -> Self {
        Self {
            import_id,
            validation_attempt_id,
            status: ImageImportStatus::Failed,
            service_name,
            image_reference,
            package_version,
            imported_at,
            imported_image: None,
            candidate_release_id: None,
            issues: Vec::new(),
        }
    }

    /// Marks the import as successful and links the candidate release.
    pub fn mark_imported(
        &mut self,
        imported_image: ImportedImageMetadata,
        candidate_release_id: Uuid,
    ) {
        self.status = ImageImportStatus::Imported;
        self.imported_image = Some(imported_image);
        self.candidate_release_id = Some(candidate_release_id);
    }

    /// Adds an issue and keeps the import marked as failed.
    pub fn fail_with(&mut self, issue: ValidationIssue) {
        self.status = ImageImportStatus::Failed;
        self.issues.push(issue);
    }
}

/// A persisted imported image that is ready for the later apply workflow.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct CandidateReleaseRecord {
    pub candidate_release_id: Uuid,
    pub import_id: Uuid,
    pub validation_attempt_id: Uuid,
    pub service_name: String,
    pub image_reference: String,
    pub package_version: Version,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub imported_image: ImportedImageMetadata,
}

impl CandidateReleaseRecord {
    /// Builds a candidate release from a successful import.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        candidate_release_id: Uuid,
        import_id: Uuid,
        validation_attempt_id: Uuid,
        service_name: String,
        image_reference: String,
        package_version: Version,
        created_at: OffsetDateTime,
        imported_image: ImportedImageMetadata,
    ) -> Self {
        Self {
            candidate_release_id,
            import_id,
            validation_attempt_id,
            service_name,
            image_reference,
            package_version,
            created_at,
            imported_image,
        }
    }
}

/// A local audit event emitted during package validation or image import.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct AuditEvent {
    pub event_id: Uuid,
    pub attempt_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub occurred_at: OffsetDateTime,
    pub kind: AuditEventKind,
    pub message: String,
}

/// High-level event kinds used in the local audit log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventKind {
    ValidationStarted,
    ValidationAccepted,
    ValidationRejected,
    ImageImportStarted,
    ImageImportSucceeded,
    ImageImportFailed,
}
