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
    fn list_service_states(&self) -> Result<Vec<ServiceStateRecord>, PersistenceError>;
    fn save_update_attempt(&self, record: &UpdateAttemptRecord) -> Result<(), PersistenceError>;
    fn load_update_attempt(
        &self,
        update_id: Uuid,
    ) -> Result<Option<UpdateAttemptRecord>, PersistenceError>;
    fn list_update_attempts(&self) -> Result<Vec<UpdateAttemptRecord>, PersistenceError>;
    fn list_audit_events(&self) -> Result<Vec<AuditEvent>, PersistenceError>;
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

    fn list_service_states(&self) -> Result<Vec<ServiceStateRecord>, PersistenceError> {
        self.ensure_layout()?;
        let mut records = read_json_dir::<ServiceStateRecord>(&self.root.join("service-states"))?;
        records.sort_by(|left, right| left.service_name.cmp(&right.service_name));
        Ok(records)
    }

    fn save_update_attempt(&self, record: &UpdateAttemptRecord) -> Result<(), PersistenceError> {
        self.ensure_layout()?;
        write_json_atomic(&self.update_attempt_path(record.update_id), record)
    }

    fn load_update_attempt(
        &self,
        update_id: Uuid,
    ) -> Result<Option<UpdateAttemptRecord>, PersistenceError> {
        self.ensure_layout()?;
        read_json_optional(&self.update_attempt_path(update_id))
    }

    fn list_update_attempts(&self) -> Result<Vec<UpdateAttemptRecord>, PersistenceError> {
        self.ensure_layout()?;
        let mut records = read_json_dir::<UpdateAttemptRecord>(&self.root.join("update-attempts"))?;
        records.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| left.update_id.cmp(&right.update_id))
        });
        Ok(records)
    }

    fn list_audit_events(&self) -> Result<Vec<AuditEvent>, PersistenceError> {
        self.ensure_layout()?;

        let path = self.audit_log_path();
        let contents = match fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(PersistenceError::OpenFile {
                    path: path.display().to_string(),
                    source,
                });
            }
        };

        let mut events: Vec<AuditEvent> = Vec::new();
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            events.push(serde_json::from_str(line).map_err(PersistenceError::Serialize)?);
        }

        events.sort_by(|left, right| {
            left.occurred_at
                .cmp(&right.occurred_at)
                .then_with(|| left.event_id.cmp(&right.event_id))
        });

        Ok(events)
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

fn read_json_dir<T>(path: &Path) -> Result<Vec<T>, PersistenceError>
where
    T: DeserializeOwned,
{
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path).map_err(|source| PersistenceError::OpenFile {
        path: path.display().to_string(),
        source,
    })?;

    for entry in read_dir {
        let entry = entry.map_err(|source| PersistenceError::OpenFile {
            path: path.display().to_string(),
            source,
        })?;
        let file_type = entry
            .file_type()
            .map_err(|source| PersistenceError::OpenFile {
                path: entry.path().display().to_string(),
                source,
            })?;

        if file_type.is_file() {
            entries.push(entry.path());
        }
    }

    entries.sort();

    let mut values = Vec::with_capacity(entries.len());
    for entry_path in entries {
        let bytes = fs::read(&entry_path).map_err(|source| PersistenceError::OpenFile {
            path: entry_path.display().to_string(),
            source,
        })?;
        values.push(serde_json::from_slice(&bytes).map_err(PersistenceError::Serialize)?);
    }

    Ok(values)
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use time::OffsetDateTime;
    use uuid::Uuid;

    use super::{FilesystemStore, StateStore};
    use crate::domain::{
        AuditEvent, AuditEventKind, RuntimeModeKind, ServiceStateRecord, UpdateAttemptRecord,
    };

    #[test]
    fn lists_service_states_in_service_name_order() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(temp_dir.path());

        store
            .save_service_state(&ServiceStateRecord::new(
                "frontend".into(),
                None,
                "example/frontend:2.0.0".into(),
                None,
                None,
                parse_time("2026-03-17T12:00:00Z"),
            ))
            .expect("save state");
        store
            .save_service_state(&ServiceStateRecord::new(
                "backend".into(),
                None,
                "example/backend:2.0.0".into(),
                None,
                None,
                parse_time("2026-03-17T11:00:00Z"),
            ))
            .expect("save state");

        let states = store.list_service_states().expect("list states");
        let names = states
            .into_iter()
            .map(|record| record.service_name)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["backend", "frontend"]);
    }

    #[test]
    fn lists_update_attempts_newest_first_and_loads_one_by_id() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(temp_dir.path());
        let older_id = Uuid::new_v4();
        let newer_id = Uuid::new_v4();

        store
            .save_update_attempt(&UpdateAttemptRecord::new(
                older_id,
                Uuid::new_v4(),
                Uuid::new_v4(),
                "frontend".into(),
                RuntimeModeKind::Docker,
                "example/frontend:1.0.0".into(),
                None,
                None,
                parse_time("2026-03-17T10:00:00Z"),
            ))
            .expect("save older attempt");
        store
            .save_update_attempt(&UpdateAttemptRecord::new(
                newer_id,
                Uuid::new_v4(),
                Uuid::new_v4(),
                "frontend".into(),
                RuntimeModeKind::Docker,
                "example/frontend:2.0.0".into(),
                None,
                None,
                parse_time("2026-03-17T11:00:00Z"),
            ))
            .expect("save newer attempt");

        let attempts = store.list_update_attempts().expect("list attempts");
        let ids = attempts
            .iter()
            .map(|record| record.update_id)
            .collect::<Vec<_>>();

        assert_eq!(ids, vec![newer_id, older_id]);
        assert_eq!(
            store
                .load_update_attempt(newer_id)
                .expect("load attempt")
                .expect("attempt should exist")
                .target_image_reference,
            "example/frontend:2.0.0"
        );
    }

    #[test]
    fn lists_audit_events_in_time_order() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(temp_dir.path());
        let attempt_id = Uuid::new_v4();

        store
            .append_audit_event(&AuditEvent {
                event_id: Uuid::from_str("00000000-0000-0000-0000-000000000002").expect("uuid"),
                attempt_id,
                occurred_at: parse_time("2026-03-17T11:00:00Z"),
                kind: AuditEventKind::UpdateCommitted,
                message: "second".into(),
            })
            .expect("append event");
        store
            .append_audit_event(&AuditEvent {
                event_id: Uuid::from_str("00000000-0000-0000-0000-000000000001").expect("uuid"),
                attempt_id,
                occurred_at: parse_time("2026-03-17T10:00:00Z"),
                kind: AuditEventKind::ValidationStarted,
                message: "first".into(),
            })
            .expect("append event");

        let events = store.list_audit_events().expect("list events");
        let messages = events
            .into_iter()
            .map(|event| event.message)
            .collect::<Vec<_>>();

        assert_eq!(messages, vec!["first", "second"]);
    }

    fn parse_time(value: &str) -> OffsetDateTime {
        OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .expect("valid timestamp")
    }
}
