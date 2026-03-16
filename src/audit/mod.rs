//! Helpers for local audit events.

use std::path::Path;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{AuditEvent, AuditEventKind, ValidationRecord, ValidationStatus};

/// Builds the audit event emitted when package validation begins.
#[must_use]
pub fn validation_started(
    attempt_id: Uuid,
    source_path: &Path,
    occurred_at: OffsetDateTime,
) -> AuditEvent {
    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id,
        occurred_at,
        kind: AuditEventKind::ValidationStarted,
        message: format!("started validating package `{}`", source_path.display()),
    }
}

/// Builds the audit event emitted when package validation finishes.
#[must_use]
pub fn validation_finished(record: &ValidationRecord, occurred_at: OffsetDateTime) -> AuditEvent {
    let (kind, message) = match record.status {
        ValidationStatus::Accepted => (
            AuditEventKind::ValidationAccepted,
            format!(
                "accepted package for service `{}`",
                record
                    .manifest
                    .as_ref()
                    .map(|manifest| manifest.target.service.as_str())
                    .unwrap_or("unknown")
            ),
        ),
        ValidationStatus::Rejected => (
            AuditEventKind::ValidationRejected,
            format!(
                "rejected package: {}",
                record
                    .issues
                    .first()
                    .map(|issue| issue.message.as_str())
                    .unwrap_or("validation failed")
            ),
        ),
    };

    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.attempt_id,
        occurred_at,
        kind,
        message,
    }
}
