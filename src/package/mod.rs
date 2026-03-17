//! Package intake, staging, and validation.
//!
//! A local package file is untrusted until every baseline check says otherwise.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use tar::{Archive, EntryType};
use tempfile::NamedTempFile;
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::audit;
use crate::domain::{
    CompatibilitySpec, ImageArchiveMetadata, ImageSpec, PackageManifest, PackageTarget,
    PackageType, SUPPORTED_MANIFEST_FORMAT_VERSION, ServiceCatalog, ValidationIssue,
    ValidationRecord,
};
use crate::persistence::{PersistenceError, StateStore};

const MAX_MANIFEST_SIZE_BYTES: u64 = 64 * 1024;

/// Package validation service.
#[derive(Debug)]
pub struct PackageIntakeService<S> {
    catalog: ServiceCatalog,
    store: S,
    current_updater_version: Version,
}

impl<S> PackageIntakeService<S>
where
    S: StateStore,
{
    /// Creates a new package intake service.
    pub fn new(catalog: ServiceCatalog, store: S, current_updater_version: Version) -> Self {
        Self {
            catalog,
            store,
            current_updater_version,
        }
    }

    /// Validates a local `.edgepkg` and persists the result.
    ///
    /// Invalid packages still return a `ValidationRecord`; only unexpected local
    /// I/O or persistence failures return as errors.
    pub fn validate_package(
        &self,
        source_path: impl AsRef<Path>,
        staging_root: &Path,
    ) -> Result<ValidationRecord, PackageIntakeError> {
        let source_path = source_path.as_ref().to_path_buf();
        let attempt_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let mut record = ValidationRecord::new(attempt_id, source_path.clone(), now);

        self.store
            .append_audit_event(&audit::validation_started(attempt_id, &source_path, now))
            .map_err(PackageIntakeError::Persistence)?;

        let staged_package = match StagingManager::new(staging_root.to_path_buf())
            .stage_package(&source_path, attempt_id)
        {
            Ok(staged) => {
                record.staged_path = Some(staged.path.clone());
                record.source_file_size_bytes = Some(staged.size_bytes);
                Some(staged)
            }
            Err(StageError::Issue(issue)) => {
                record.reject_with(issue);
                None
            }
            Err(StageError::Io(source)) => {
                return Err(PackageIntakeError::Staging(source));
            }
        };

        let inspection = match staged_package.as_ref() {
            Some(staged) => match PackageArchiveInspector::inspect(&staged.path) {
                Ok(inspection) => Some(inspection),
                Err(ArchiveInspectionError::Issue(issue)) => {
                    record.reject_with(issue);
                    None
                }
                Err(ArchiveInspectionError::Io(source)) => {
                    return Err(PackageIntakeError::ArchiveIo(source));
                }
            },
            None => None,
        };

        if let Some(inspection) = inspection.as_ref() {
            record.image_archive = Some(inspection.image_archive.clone());

            match ManifestParser::parse(&inspection.manifest_bytes) {
                Ok(manifest) => {
                    for issue in
                        validate_manifest(&manifest, &self.catalog, &self.current_updater_version)
                    {
                        record.reject_with(issue);
                    }

                    record.manifest = Some(manifest);
                }
                Err(issue) => record.reject_with(issue),
            }
        }

        if record.issues.is_empty() {
            record.accept();
        }

        self.store
            .save_validation_record(&record)
            .map_err(PackageIntakeError::Persistence)?;
        self.store
            .append_audit_event(&audit::validation_finished(
                &record,
                OffsetDateTime::now_utc(),
            ))
            .map_err(PackageIntakeError::Persistence)?;

        Ok(record)
    }
}

/// Fatal errors for the package intake path.
#[derive(Debug, Error)]
pub enum PackageIntakeError {
    #[error("failed to persist validation state: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("failed to stage package: {0}")]
    Staging(std::io::Error),
    #[error("failed to read staged archive: {0}")]
    ArchiveIo(std::io::Error),
}

/// Errors produced while accessing `image.tar` from a staged package.
#[derive(Debug, Error)]
pub enum ImageArchiveAccessError {
    #[error("{0}")]
    Issue(ValidationIssue),
    #[error("failed to access staged package archive: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
struct StagingManager {
    root: PathBuf,
}

impl StagingManager {
    fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn stage_package(
        &self,
        source_path: &Path,
        attempt_id: Uuid,
    ) -> Result<StagedPackage, StageError> {
        let extension = source_path.extension().and_then(std::ffi::OsStr::to_str);
        if extension != Some("edgepkg") {
            return Err(StageError::Issue(ValidationIssue::new(
                "package.invalid_extension",
                "package file must use the `.edgepkg` extension",
            )));
        }

        let metadata = match fs::metadata(source_path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(StageError::Issue(ValidationIssue::new(
                    "package.not_found",
                    format!("package file `{}` does not exist", source_path.display()),
                )));
            }
            Err(error) => return Err(StageError::Io(error)),
        };

        if !metadata.is_file() {
            return Err(StageError::Issue(ValidationIssue::new(
                "package.not_a_file",
                format!(
                    "package path `{}` is not a regular file",
                    source_path.display()
                ),
            )));
        }

        let staging_dir = self.root.join(attempt_id.to_string());
        fs::create_dir_all(&staging_dir).map_err(StageError::Io)?;

        let staged_path = staging_dir.join("package.edgepkg");
        fs::copy(source_path, &staged_path).map_err(StageError::Io)?;

        Ok(StagedPackage {
            path: staged_path,
            size_bytes: metadata.len(),
        })
    }
}

#[derive(Debug)]
struct StagedPackage {
    path: PathBuf,
    size_bytes: u64,
}

#[derive(Debug, Error)]
enum StageError {
    #[error("{0}")]
    Issue(ValidationIssue),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
struct ArchiveInspection {
    manifest_bytes: Vec<u8>,
    image_archive: ImageArchiveMetadata,
}

#[derive(Debug, Error)]
enum ArchiveInspectionError {
    #[error("{0}")]
    Issue(ValidationIssue),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

struct PackageArchiveInspector;

impl PackageArchiveInspector {
    fn inspect(path: &Path) -> Result<ArchiveInspection, ArchiveInspectionError> {
        let file = File::open(path).map_err(ArchiveInspectionError::Io)?;
        let mut archive = Archive::new(file);
        let mut manifest_bytes = None;
        let mut image_archive = None;
        let mut seen_entries = std::collections::BTreeSet::new();

        let entries = archive.entries().map_err(|error| {
            ArchiveInspectionError::Issue(ValidationIssue::new(
                "archive.read_failed",
                format!("failed to read tar archive: {error}"),
            ))
        })?;

        for entry in entries {
            let mut entry = entry.map_err(|error| {
                ArchiveInspectionError::Issue(ValidationIssue::new(
                    "archive.entry_read_failed",
                    format!("failed to read tar entry: {error}"),
                ))
            })?;

            let entry_name = normalize_entry_path(&entry)?;

            if !seen_entries.insert(entry_name.clone()) {
                return Err(ArchiveInspectionError::Issue(ValidationIssue::new(
                    "archive.duplicate_entry",
                    format!("duplicate archive entry `{entry_name}` is not allowed"),
                )));
            }

            let entry_type = entry.header().entry_type();
            if entry_type != EntryType::Regular {
                return Err(ArchiveInspectionError::Issue(ValidationIssue::new(
                    "archive.unsafe_entry_type",
                    format!(
                        "archive entry `{entry_name}` uses unsupported tar entry type `{entry_type:?}`"
                    ),
                )));
            }

            let size = entry.header().size().map_err(|error| {
                ArchiveInspectionError::Issue(ValidationIssue::new(
                    "archive.invalid_entry_size",
                    format!("archive entry `{entry_name}` has an invalid size: {error}"),
                ))
            })?;

            match entry_name.as_str() {
                "manifest.toml" => {
                    if size > MAX_MANIFEST_SIZE_BYTES {
                        return Err(ArchiveInspectionError::Issue(ValidationIssue::new(
                            "manifest.too_large",
                            format!(
                                "manifest.toml is too large ({size} bytes); max supported size is {MAX_MANIFEST_SIZE_BYTES} bytes"
                            ),
                        )));
                    }

                    let mut buffer = Vec::with_capacity(size as usize);
                    entry.read_to_end(&mut buffer).map_err(|error| {
                        ArchiveInspectionError::Issue(ValidationIssue::new(
                            "manifest.read_failed",
                            format!("failed to read manifest.toml: {error}"),
                        ))
                    })?;
                    manifest_bytes = Some(buffer);
                }
                "image.tar" => {
                    if size == 0 {
                        return Err(ArchiveInspectionError::Issue(ValidationIssue::new(
                            "image.empty_archive",
                            "image.tar must not be empty",
                        )));
                    }

                    drain_entry(&mut entry).map_err(|error| {
                        ArchiveInspectionError::Issue(ValidationIssue::new(
                            "image.read_failed",
                            format!("failed to read image.tar entry: {error}"),
                        ))
                    })?;

                    image_archive = Some(ImageArchiveMetadata {
                        entry_name,
                        size_bytes: size,
                    });
                }
                _ => {
                    drain_entry(&mut entry).map_err(|error| {
                        ArchiveInspectionError::Issue(ValidationIssue::new(
                            "archive.entry_read_failed",
                            format!("failed to read archive entry `{entry_name}`: {error}"),
                        ))
                    })?;
                }
            }
        }

        let manifest_bytes = manifest_bytes.ok_or_else(|| {
            ArchiveInspectionError::Issue(ValidationIssue::new(
                "archive.missing_manifest",
                "archive is missing required entry `manifest.toml`",
            ))
        })?;
        let image_archive = image_archive.ok_or_else(|| {
            ArchiveInspectionError::Issue(ValidationIssue::new(
                "archive.missing_image",
                "archive is missing required entry `image.tar`",
            ))
        })?;

        Ok(ArchiveInspection {
            manifest_bytes,
            image_archive,
        })
    }
}

/// Extracts `image.tar` from a staged `.edgepkg` into a temporary file.
///
/// This keeps the Docker import path focused on a single payload file without
/// turning the package module into a general extraction API.
pub fn extract_image_archive_to_temp_file(
    staged_package_path: &Path,
) -> Result<NamedTempFile, ImageArchiveAccessError> {
    let file = File::open(staged_package_path)?;
    let mut archive = Archive::new(file);

    for entry in archive.entries()? {
        let mut entry = entry.map_err(ImageArchiveAccessError::Io)?;
        let entry_name = normalize_image_access_entry_path(&entry)?;
        let entry_type = entry.header().entry_type();

        if entry_type != EntryType::Regular {
            return Err(ImageArchiveAccessError::Issue(ValidationIssue::new(
                "archive.unsafe_entry_type",
                format!(
                    "archive entry `{entry_name}` uses unsupported tar entry type `{entry_type:?}`"
                ),
            )));
        }

        if entry_name != "image.tar" {
            drain_entry(&mut entry)?;
            continue;
        }

        let size = entry.header().size().map_err(|error| {
            ImageArchiveAccessError::Issue(ValidationIssue::new(
                "archive.invalid_entry_size",
                format!("archive entry `image.tar` has an invalid size: {error}"),
            ))
        })?;
        if size == 0 {
            return Err(ImageArchiveAccessError::Issue(ValidationIssue::new(
                "image.empty_archive",
                "image.tar must not be empty",
            )));
        }

        let mut temp_file = NamedTempFile::new().map_err(ImageArchiveAccessError::Io)?;
        std::io::copy(&mut entry, &mut temp_file).map_err(ImageArchiveAccessError::Io)?;
        temp_file
            .as_file_mut()
            .flush()
            .map_err(ImageArchiveAccessError::Io)?;

        return Ok(temp_file);
    }

    Err(ImageArchiveAccessError::Issue(ValidationIssue::new(
        "archive.missing_image",
        "archive is missing required entry `image.tar`",
    )))
}

fn normalize_entry_path<R: Read>(
    entry: &tar::Entry<'_, R>,
) -> Result<String, ArchiveInspectionError> {
    let path = entry.path().map_err(|error| {
        ArchiveInspectionError::Issue(ValidationIssue::new(
            "archive.invalid_entry_path",
            format!("archive entry has an invalid path: {error}"),
        ))
    })?;

    let mut components = path.components();
    let first = components.next().ok_or_else(|| {
        ArchiveInspectionError::Issue(ValidationIssue::new(
            "archive.empty_entry_path",
            "archive entry path cannot be empty",
        ))
    })?;

    if components.next().is_some() {
        return Err(ArchiveInspectionError::Issue(ValidationIssue::new(
            "archive.nested_entry",
            format!(
                "archive entry `{}` must live at the package root",
                path.display()
            ),
        )));
    }

    match first {
        Component::Normal(component) => Ok(component.to_string_lossy().to_string()),
        _ => Err(ArchiveInspectionError::Issue(ValidationIssue::new(
            "archive.invalid_entry_path",
            format!(
                "archive entry `{}` uses an unsupported path form",
                path.display()
            ),
        ))),
    }
}

fn normalize_image_access_entry_path<R: Read>(
    entry: &tar::Entry<'_, R>,
) -> Result<String, ImageArchiveAccessError> {
    let path = entry.path().map_err(|error| {
        ImageArchiveAccessError::Issue(ValidationIssue::new(
            "archive.invalid_entry_path",
            format!("archive entry has an invalid path: {error}"),
        ))
    })?;

    let mut components = path.components();
    let first = components.next().ok_or_else(|| {
        ImageArchiveAccessError::Issue(ValidationIssue::new(
            "archive.empty_entry_path",
            "archive entry path cannot be empty",
        ))
    })?;

    if components.next().is_some() {
        return Err(ImageArchiveAccessError::Issue(ValidationIssue::new(
            "archive.nested_entry",
            format!(
                "archive entry `{}` must live at the package root",
                path.display()
            ),
        )));
    }

    match first {
        Component::Normal(component) => Ok(component.to_string_lossy().to_string()),
        _ => Err(ImageArchiveAccessError::Issue(ValidationIssue::new(
            "archive.invalid_entry_path",
            format!(
                "archive entry `{}` uses an unsupported path form",
                path.display()
            ),
        ))),
    }
}

fn drain_entry<R: Read>(entry: &mut tar::Entry<'_, R>) -> std::io::Result<()> {
    std::io::copy(entry, &mut std::io::sink()).map(|_| ())
}

struct ManifestParser;

impl ManifestParser {
    fn parse(bytes: &[u8]) -> Result<PackageManifest, ValidationIssue> {
        let raw = std::str::from_utf8(bytes).map_err(|error| {
            ValidationIssue::new(
                "manifest.invalid_utf8",
                format!("manifest.toml must be valid UTF-8: {error}"),
            )
        })?;
        let manifest = toml::from_str::<RawManifest>(raw).map_err(|error| {
            ValidationIssue::new(
                "manifest.parse_failed",
                format!("failed to parse manifest.toml: {error}"),
            )
        })?;

        if manifest.format_version != SUPPORTED_MANIFEST_FORMAT_VERSION {
            return Err(ValidationIssue::new(
                "manifest.unsupported_format_version",
                format!(
                    "unsupported format_version `{}`; expected `{SUPPORTED_MANIFEST_FORMAT_VERSION}`",
                    manifest.format_version
                ),
            ));
        }

        let created_at =
            OffsetDateTime::parse(&manifest.created_at, &Rfc3339).map_err(|error| {
                ValidationIssue::new(
                    "manifest.invalid_created_at",
                    format!("created_at must be RFC3339: {error}"),
                )
            })?;
        let package_version = Version::parse(&manifest.package_version).map_err(|error| {
            ValidationIssue::new(
                "manifest.invalid_package_version",
                format!("package_version must be valid semver: {error}"),
            )
        })?;
        let min_updater_version = manifest
            .compatibility
            .and_then(|compatibility| compatibility.min_updater_version)
            .map(|value| {
                Version::parse(&value).map_err(|error| {
                    ValidationIssue::new(
                        "manifest.invalid_min_updater_version",
                        format!("compatibility.min_updater_version must be valid semver: {error}"),
                    )
                })
            })
            .transpose()?;

        Ok(PackageManifest {
            format_version: manifest.format_version,
            package_type: manifest.package_type,
            package_version,
            created_at,
            target: PackageTarget {
                service: require_non_empty("target.service", manifest.target.service)?,
                service_revision: require_non_empty(
                    "target.service_revision",
                    manifest.target.service_revision,
                )?,
                platform: require_non_empty("target.platform", manifest.target.platform)?,
            },
            image: ImageSpec {
                reference: require_non_empty("image.reference", manifest.image.reference)?,
            },
            compatibility: CompatibilitySpec {
                min_updater_version,
            },
        })
    }
}

#[derive(Debug, Deserialize)]
struct RawManifest {
    #[serde(deserialize_with = "deserialize_format_version")]
    format_version: u64,
    package_type: PackageType,
    package_version: String,
    created_at: String,
    target: RawTarget,
    image: RawImage,
    compatibility: Option<RawCompatibility>,
}

#[derive(Debug, Deserialize)]
struct RawTarget {
    service: String,
    service_revision: String,
    platform: String,
}

#[derive(Debug, Deserialize)]
struct RawImage {
    reference: String,
}

#[derive(Debug, Deserialize)]
struct RawCompatibility {
    min_updater_version: Option<String>,
}

fn deserialize_format_version<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawFormatVersion {
        Integer(u64),
        String(String),
    }

    match RawFormatVersion::deserialize(deserializer)? {
        RawFormatVersion::Integer(value) => Ok(value),
        RawFormatVersion::String(value) => value.parse::<u64>().map_err(serde::de::Error::custom),
    }
}

fn require_non_empty(field: &str, value: String) -> Result<String, ValidationIssue> {
    if value.trim().is_empty() {
        return Err(ValidationIssue::new(
            "manifest.empty_field",
            format!("{field} cannot be empty"),
        ));
    }

    Ok(value)
}

fn validate_manifest(
    manifest: &PackageManifest,
    catalog: &ServiceCatalog,
    current_updater_version: &Version,
) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();
    let Some(service) = catalog.find_service(&manifest.target.service) else {
        issues.push(ValidationIssue::new(
            "package.unknown_service",
            format!(
                "package targets unknown service `{}`",
                manifest.target.service
            ),
        ));
        return issues;
    };

    if manifest.target.service_revision != service.service_revision {
        issues.push(ValidationIssue::new(
            "package.service_revision_mismatch",
            format!(
                "package service_revision `{}` does not match configured revision `{}` for service `{}`",
                manifest.target.service_revision, service.service_revision, service.name
            ),
        ));
    }

    if manifest.target.platform != service.platform {
        issues.push(ValidationIssue::new(
            "package.platform_mismatch",
            format!(
                "package platform `{}` does not match configured platform `{}` for service `{}`",
                manifest.target.platform, service.platform, service.name
            ),
        ));
    }
    #[allow(clippy::collapsible_if)]
    if let Some(min_version) = manifest.compatibility.min_updater_version.as_ref() {
        if current_updater_version < min_version {
            issues.push(ValidationIssue::new(
                "package.updater_too_old",
                format!(
                    "package requires updater version `{min_version}` or newer, but this build is `{current_updater_version}`"
                ),
            ));
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::ManifestParser;

    #[test]
    fn accepts_string_format_version() {
        let manifest = ManifestParser::parse(
            br#"
format_version = "1"
package_type = "service"
package_version = "1.2.3"
created_at = "2026-03-16T18:30:00Z"

[target]
service = "frontend"
service_revision = "frontend-v1"
platform = "linux/amd64"

[image]
reference = "example/frontend:1.2.3"
"#,
        )
        .expect("manifest should parse");

        assert_eq!(manifest.format_version, 1);
    }
}
