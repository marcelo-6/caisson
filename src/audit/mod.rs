//! Helpers for local audit events.

use std::path::Path;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::{
    AuditEvent, AuditEventKind, HealthCheckOutcome, ImageImportRecord, ImageImportStatus,
    UpdateAttemptRecord, UpdateAttemptStatus, ValidationRecord, ValidationStatus,
};

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

/// Builds the audit event emitted when Docker image import begins.
#[must_use]
pub fn image_import_started(
    attempt_id: Uuid,
    image_reference: &str,
    occurred_at: OffsetDateTime,
) -> AuditEvent {
    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id,
        occurred_at,
        kind: AuditEventKind::ImageImportStarted,
        message: format!("started importing image `{image_reference}`"),
    }
}

/// Builds the audit event emitted when Docker image import finishes.
#[must_use]
pub fn image_import_finished(
    record: &ImageImportRecord,
    occurred_at: OffsetDateTime,
) -> AuditEvent {
    let (kind, message) = match record.status {
        ImageImportStatus::Imported => (
            AuditEventKind::ImageImportSucceeded,
            format!(
                "imported candidate release for service `{}` using image `{}`",
                record.service_name, record.image_reference
            ),
        ),
        ImageImportStatus::Failed => (
            AuditEventKind::ImageImportFailed,
            format!(
                "failed to import image `{}` for service `{}`: {}",
                record.image_reference,
                record.service_name,
                record
                    .issues
                    .first()
                    .map(|issue| issue.message.as_str())
                    .unwrap_or("image import failed")
            ),
        ),
    };

    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.validation_attempt_id,
        occurred_at,
        kind,
        message,
    }
}

/// Builds the audit event emitted when a service update starts.
#[must_use]
pub fn update_started(record: &UpdateAttemptRecord, occurred_at: OffsetDateTime) -> AuditEvent {
    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.update_id,
        occurred_at,
        kind: AuditEventKind::UpdateStarted,
        message: format!(
            "started applying image `{}` to service `{}`",
            record.target_image_reference, record.service_name
        ),
    }
}

/// Builds the audit event emitted when health checks start.
#[must_use]
pub fn health_check_started(
    record: &UpdateAttemptRecord,
    occurred_at: OffsetDateTime,
) -> AuditEvent {
    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.update_id,
        occurred_at,
        kind: AuditEventKind::HealthCheckStarted,
        message: format!(
            "started health checks for service `{}`",
            record.service_name
        ),
    }
}

/// Builds the audit event emitted when health checks finish.
#[must_use]
pub fn health_check_finished(
    record: &UpdateAttemptRecord,
    occurred_at: OffsetDateTime,
) -> AuditEvent {
    let report = record
        .health_check
        .as_ref()
        .expect("health_check_finished requires a report");
    let kind = match report.outcome {
        HealthCheckOutcome::Passed => AuditEventKind::HealthCheckPassed,
        HealthCheckOutcome::Failed | HealthCheckOutcome::TimedOut => {
            AuditEventKind::HealthCheckFailed
        }
    };

    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.update_id,
        occurred_at,
        kind,
        message: format!(
            "health check for service `{}` {}: {}",
            record.service_name,
            match report.outcome {
                HealthCheckOutcome::Passed => "passed",
                HealthCheckOutcome::Failed => "failed",
                HealthCheckOutcome::TimedOut => "timed out",
            },
            report.message
        ),
    }
}

/// Builds the audit event emitted when rollback starts.
#[must_use]
pub fn rollback_started(record: &UpdateAttemptRecord, occurred_at: OffsetDateTime) -> AuditEvent {
    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.update_id,
        occurred_at,
        kind: AuditEventKind::RollbackStarted,
        message: format!("started rollback for service `{}`", record.service_name),
    }
}

/// Builds the audit event emitted when rollback finishes.
#[must_use]
pub fn rollback_finished(record: &UpdateAttemptRecord, occurred_at: OffsetDateTime) -> AuditEvent {
    let (kind, message) = match record.status {
        UpdateAttemptStatus::RolledBack => (
            AuditEventKind::RollbackSucceeded,
            format!(
                "rolled back service `{}` to the previous image",
                record.service_name
            ),
        ),
        UpdateAttemptStatus::RollbackFailed => (
            AuditEventKind::RollbackFailed,
            format!(
                "rollback failed for service `{}`: {}",
                record.service_name,
                record
                    .issues
                    .last()
                    .map(|issue| issue.message.as_str())
                    .unwrap_or("rollback failed")
            ),
        ),
        _ => (
            AuditEventKind::RollbackFailed,
            format!(
                "rollback did not finish cleanly for service `{}`",
                record.service_name
            ),
        ),
    };

    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.update_id,
        occurred_at,
        kind,
        message,
    }
}

/// Builds the audit event emitted when an update is committed.
#[must_use]
pub fn update_finished(record: &UpdateAttemptRecord, occurred_at: OffsetDateTime) -> AuditEvent {
    let message = match record.status {
        UpdateAttemptStatus::Succeeded => format!(
            "committed image `{}` for service `{}`",
            record.target_image_reference, record.service_name
        ),
        _ => format!(
            "update for service `{}` finished with status `{:?}`",
            record.service_name, record.status
        ),
    };

    AuditEvent {
        event_id: Uuid::new_v4(),
        attempt_id: record.update_id,
        occurred_at,
        kind: AuditEventKind::UpdateCommitted,
        message,
    }
}
