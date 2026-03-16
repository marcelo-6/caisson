//! Domain types shared across the crate.
//!

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
/// to touch. If a service is not in here, the updater should pretend it does
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
///
/// For now there is only one allowed value.
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

/// Validation status for a package intake attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationStatus {
    Accepted,
    Rejected,
}

/// A single actionable validation issue.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ValidationIssue {
    pub code: String,
    pub message: String,
}

impl ValidationIssue {
    /// Helper for creating a validation issue
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

    /// Adds a validation issue and keeps the record rejected.
    pub fn reject_with(&mut self, issue: ValidationIssue) {
        self.status = ValidationStatus::Rejected;
        self.issues.push(issue);
    }
}

/// A local audit event emitted during package validation.
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
}
