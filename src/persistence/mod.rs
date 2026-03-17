//! Filesystem persistence for validation records, import records, and
//! audit events.
//!
//! Might use sqlite later, not final yet.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

use crate::domain::{AuditEvent, CandidateReleaseRecord, ImageImportRecord, ValidationRecord};

/// Storage operations the application.
pub trait StateStore {
    fn save_validation_record(&self, record: &ValidationRecord) -> Result<(), PersistenceError>;
    fn save_image_import_record(&self, record: &ImageImportRecord) -> Result<(), PersistenceError>;
    fn save_candidate_release(
        &self,
        record: &CandidateReleaseRecord,
    ) -> Result<(), PersistenceError>;
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
