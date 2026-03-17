//! Filesystem persistence for validation records, import records, and
//! audit events.
//!
//! Might use sqlite later, not final yet.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{
    AuditEvent, CandidateReleaseRecord, ImageImportRecord, ServiceStateRecord, UpdateAttemptRecord,
    ValidationRecord,
};

/// Storage operations the application.
pub trait StateStore {
    fn save_validation_record(&self, record: &ValidationRecord) -> Result<(), PersistenceError>;
    fn save_image_import_record(&self, record: &ImageImportRecord) -> Result<(), PersistenceError>;
    fn save_candidate_release(
        &self,
        record: &CandidateReleaseRecord,
    ) -> Result<(), PersistenceError>;
    fn load_candidate_release(
        &self,
        candidate_release_id: Uuid,
    ) -> Result<Option<CandidateReleaseRecord>, PersistenceError>;
    fn save_service_state(&self, record: &ServiceStateRecord) -> Result<(), PersistenceError>;
    fn load_service_state(
        &self,
        service_name: &str,
    ) -> Result<Option<ServiceStateRecord>, PersistenceError>;
    fn save_update_attempt(&self, record: &UpdateAttemptRecord) -> Result<(), PersistenceError>;
    fn append_audit_event(&self, event: &AuditEvent) -> Result<(), PersistenceError>;
}

/// Filesystem local state store.
#[derive(Debug, Clone)]
pub struct FilesystemStore {
    root: PathBuf,
}

impl FilesystemStore {
    /// Creates a store rooted at the provided directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Returns the configured store root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Returns the path used for staged packages.
    #[must_use]
    pub fn staging_dir_for(&self, attempt_id: Uuid) -> PathBuf {
        self.root.join("staging").join(attempt_id.to_string())
    }

    /// Returns the JSON path for a persisted validation record.
    #[must_use]
    pub fn validation_record_path(&self, attempt_id: Uuid) -> PathBuf {
        self.root
            .join("validation-records")
            .join(format!("{attempt_id}.json"))
    }

    /// Returns the JSON path for a persisted image-import record.
    #[must_use]
    pub fn image_import_record_path(&self, import_id: Uuid) -> PathBuf {
        self.root
            .join("image-import-records")
            .join(format!("{import_id}.json"))
    }

    /// Returns the JSON path for a persisted candidate release record.
    #[must_use]
    pub fn candidate_release_path(&self, candidate_release_id: Uuid) -> PathBuf {
        self.root
            .join("candidate-releases")
            .join(format!("{candidate_release_id}.json"))
    }

    /// Returns the JSON path for a persisted service state record.
    #[must_use]
    pub fn service_state_path(&self, service_name: &str) -> PathBuf {
        self.root
            .join("service-states")
            .join(format!("{}.json", sanitize_path_component(service_name)))
    }

    /// Returns the JSON path for a persisted update attempt.
    #[must_use]
    pub fn update_attempt_path(&self, update_id: Uuid) -> PathBuf {
        self.root
            .join("update-attempts")
            .join(format!("{update_id}.json"))
    }

    fn audit_log_path(&self) -> PathBuf {
        self.root.join("audit").join("events.jsonl")
    }

    fn ensure_layout(&self) -> Result<(), PersistenceError> {
        for path in [
            self.root.clone(),
            self.root.join("staging"),
            self.root.join("validation-records"),
            self.root.join("image-import-records"),
            self.root.join("candidate-releases"),
            self.root.join("service-states"),
            self.root.join("update-attempts"),
            self.root.join("audit"),
        ] {
            fs::create_dir_all(&path).map_err(|source| PersistenceError::CreateDir {
                path: path.display().to_string(),
                source,
            })?;
        }

        Ok(())
    }
}

impl StateStore for FilesystemStore {
    fn save_validation_record(&self, record: &ValidationRecord) -> Result<(), PersistenceError> {
        self.ensure_layout()?;
        write_json_atomic(&self.validation_record_path(record.attempt_id), record)
    }

    fn save_image_import_record(&self, record: &ImageImportRecord) -> Result<(), PersistenceError> {
        self.ensure_layout()?;
        write_json_atomic(&self.image_import_record_path(record.import_id), record)
    }

    fn save_candidate_release(
        &self,
        record: &CandidateReleaseRecord,
    ) -> Result<(), PersistenceError> {
        self.ensure_layout()?;
        write_json_atomic(
            &self.candidate_release_path(record.candidate_release_id),
            record,
        )
    }

    fn load_candidate_release(
        &self,
        candidate_release_id: Uuid,
    ) -> Result<Option<CandidateReleaseRecord>, PersistenceError> {
        self.ensure_layout()?;
        read_json_optional(&self.candidate_release_path(candidate_release_id))
    }

    fn save_service_state(&self, record: &ServiceStateRecord) -> Result<(), PersistenceError> {
        self.ensure_layout()?;
        write_json_atomic(&self.service_state_path(&record.service_name), record)
    }

    fn load_service_state(
        &self,
        service_name: &str,
    ) -> Result<Option<ServiceStateRecord>, PersistenceError> {
        self.ensure_layout()?;
        read_json_optional(&self.service_state_path(service_name))
    }

    fn save_update_attempt(&self, record: &UpdateAttemptRecord) -> Result<(), PersistenceError> {
        self.ensure_layout()?;
        write_json_atomic(&self.update_attempt_path(record.update_id), record)
    }

    fn append_audit_event(&self, event: &AuditEvent) -> Result<(), PersistenceError> {
        self.ensure_layout()?;

        let path = self.audit_log_path();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| PersistenceError::OpenFile {
                path: path.display().to_string(),
                source,
            })?;
        let line = serde_json::to_string(event).map_err(PersistenceError::Serialize)?;
        writeln!(file, "{line}").map_err(|source| PersistenceError::WriteFile {
            path: path.display().to_string(),
            source,
        })?;

        Ok(())
    }
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), PersistenceError> {
    let temp_path = path.with_extension("json.tmp");
    let serialized = serde_json::to_vec_pretty(value).map_err(PersistenceError::Serialize)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| PersistenceError::CreateDir {
            path: parent.display().to_string(),
            source,
        })?;
    }

    let mut file = File::create(&temp_path).map_err(|source| PersistenceError::OpenFile {
        path: temp_path.display().to_string(),
        source,
    })?;
    file.write_all(&serialized)
        .map_err(|source| PersistenceError::WriteFile {
            path: temp_path.display().to_string(),
            source,
        })?;
    file.sync_all()
        .map_err(|source| PersistenceError::WriteFile {
            path: temp_path.display().to_string(),
            source,
        })?;
    fs::rename(&temp_path, path).map_err(|source| PersistenceError::WriteFile {
        path: path.display().to_string(),
        source,
    })?;

    Ok(())
}

fn read_json_optional<T>(path: &Path) -> Result<Option<T>, PersistenceError>
where
    T: DeserializeOwned,
{
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(PersistenceError::Serialize),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PersistenceError::OpenFile {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => character,
            _ => '_',
        })
        .collect()
}

/// Errors produced by filesystem persistence.
#[derive(Debug, Error)]
pub enum PersistenceError {
    #[error("failed to create directory `{path}`: {source}")]
    CreateDir {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to open file `{path}`: {source}")]
    OpenFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write file `{path}`: {source}")]
    WriteFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize persistent state: {0}")]
    Serialize(#[from] serde_json::Error),
}
