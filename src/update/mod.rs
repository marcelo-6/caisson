//! Service update orchestration.
//!
//! Where the imported image becomes a service change.
//! Try to persist state, apply one service,
//! check health, and roll back when things go sideways.

use std::thread::sleep;
use std::time::{Duration, Instant};

use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::audit;
use crate::compose::{ComposeClient, ComposeError};
use crate::docker::{
    ContainerHealthState, DockerClientError, DockerServiceClient, ObservedContainer,
};
use crate::domain::{
    CandidateReleaseRecord, ComposeRuntime, DockerRuntime, HealthCheckKind, HealthCheckOutcome,
    HealthCheckReport, ManagedService, RuntimeMode, ServiceCatalog, ServiceStateRecord,
    UpdateAttemptRecord, ValidationIssue,
};
use crate::persistence::{PersistenceError, StateStore};

/// Backend workflow that applies candidate releases to managed services.
#[derive(Debug)]
pub struct UpdateService<D, C, S> {
    catalog: ServiceCatalog,
    docker: D,
    compose: C,
    store: S,
}

impl<D, C, S> UpdateService<D, C, S>
where
    D: DockerServiceClient,
    C: ComposeClient,
    S: StateStore,
{
    /// Creates a new update service.
    pub fn new(catalog: ServiceCatalog, docker: D, compose: C, store: S) -> Self {
        Self {
            catalog,
            docker,
            compose,
            store,
        }
    }

    /// Applies a persisted candidate release to its target service.
    pub fn apply_candidate_release(
        &self,
        candidate_release_id: Uuid,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        let candidate_release = self
            .store
            .load_candidate_release(candidate_release_id)?
            .ok_or_else(|| {
                UpdateError::Precondition(format!(
                    "candidate release `{candidate_release_id}` was not found"
                ))
            })?;
        let service = self
            .catalog
            .find_service(&candidate_release.service_name)
            .cloned()
            .ok_or_else(|| {
                UpdateError::Precondition(format!(
                    "service `{}` is not present in the current catalog",
                    candidate_release.service_name
                ))
            })?;
        let previous_state = self.store.load_service_state(&service.name)?;
        let started_at = OffsetDateTime::now_utc();
        let attempt = UpdateAttemptRecord::new(
            Uuid::new_v4(),
            candidate_release.candidate_release_id,
            candidate_release.validation_attempt_id,
            service.name.clone(),
            service.runtime.kind(),
            candidate_release.image_reference.clone(),
            previous_state
                .as_ref()
                .and_then(|state| state.active_candidate_release_id),
            previous_state
                .as_ref()
                .map(|state| state.active_image_reference.clone()),
            started_at,
        );

        self.persist_attempt(&attempt)?;
        self.store
            .append_audit_event(&audit::update_started(&attempt, started_at))?;

        match &service.runtime {
            RuntimeMode::Docker(runtime) => self.apply_docker_service(
                &service,
                runtime,
                &candidate_release,
                previous_state,
                attempt,
            ),
            RuntimeMode::Compose(runtime) => self.apply_compose_service(
                &service,
                runtime,
                &candidate_release,
                previous_state,
                attempt,
            ),
        }
    }

    fn apply_docker_service(
        &self,
        service: &ManagedService,
        runtime: &DockerRuntime,
        candidate_release: &CandidateReleaseRecord,
        previous_state: Option<ServiceStateRecord>,
        mut attempt: UpdateAttemptRecord,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        let current_container = match self.docker.inspect_container(&runtime.container_name) {
            Ok(container) => container,
            Err(error) => {
                return self.finish_failed_attempt(
                    attempt,
                    issue_from_docker_error(
                        "docker.service_inspect_failed",
                        "failed to inspect the managed container",
                        &error,
                    ),
                );
            }
        };

        if attempt.previous_image_reference.is_none() {
            attempt.previous_image_reference = current_container
                .image_reference
                .clone()
                .or_else(|| Some(runtime.image_reference.clone()));
            self.persist_attempt(&attempt)?;
        }

        let previous_image = match attempt.previous_image_reference.clone() {
            Some(image) => image,
            None => {
                return self.finish_failed_attempt(
                    attempt,
                    ValidationIssue::new(
                        "service.previous_image_unknown",
                        "could not determine the previous image for rollback",
                    ),
                );
            }
        };

        let backup_name = rollback_container_name(&runtime.container_name, attempt.update_id);
        attempt.rollback_container_name = Some(backup_name.clone());
        self.persist_attempt(&attempt)?;

        if let Err(error) = self.best_effort_stop(&runtime.container_name) {
            return self.finish_failed_attempt(
                attempt,
                issue_from_docker_error(
                    "docker.stop_failed",
                    "failed to stop the managed container before update",
                    &error,
                ),
            );
        }

        if let Err(error) = self
            .docker
            .rename_container(&runtime.container_name, &backup_name)
        {
            return self.finish_failed_attempt(
                attempt,
                issue_from_docker_error(
                    "docker.rename_failed",
                    "failed to move the old container into rollback position",
                    &error,
                ),
            );
        }

        let mut new_container_created = false;
        if let Err(error) = self.docker.create_container_from(
            &runtime.container_name,
            &current_container,
            &candidate_release.image_reference,
        ) {
            attempt.add_issue(issue_from_docker_error(
                "docker.create_failed",
                "failed to create the replacement container",
                &error,
            ));
            return self.rollback_docker_service(
                service,
                runtime,
                attempt,
                previous_state,
                &backup_name,
                &previous_image,
                new_container_created,
            );
        }
        new_container_created = true;

        if let Err(error) = self.docker.start_container(&runtime.container_name) {
            attempt.add_issue(issue_from_docker_error(
                "docker.start_failed",
                "failed to start the replacement container",
                &error,
            ));
            return self.rollback_docker_service(
                service,
                runtime,
                attempt,
                previous_state,
                &backup_name,
                &previous_image,
                new_container_created,
            );
        }

        attempt.mark_health_checking();
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::health_check_started(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        let report =
            self.wait_for_named_container_health(&runtime.container_name, &service.health_check);
        attempt.health_check = Some(report);
        self.persist_attempt(&attempt)?;
        self.store
            .append_audit_event(&audit::health_check_finished(
                &attempt,
                OffsetDateTime::now_utc(),
            ))?;

        match attempt.health_check.as_ref().map(|report| report.outcome) {
            Some(HealthCheckOutcome::Passed) => {
                if let Err(error) = self.docker.remove_container(&backup_name, true) {
                    attempt.add_issue(issue_from_docker_error(
                        "docker.cleanup_failed",
                        "failed to remove the rollback container after a good update",
                        &error,
                    ));
                }

                let service_state = committed_service_state(
                    service,
                    candidate_release,
                    previous_state.as_ref(),
                    &previous_image,
                    OffsetDateTime::now_utc(),
                );
                attempt.mark_succeeded(OffsetDateTime::now_utc());
                self.store.save_service_state(&service_state)?;
                self.persist_attempt(&attempt)?;
                self.store.append_audit_event(&audit::update_finished(
                    &attempt,
                    OffsetDateTime::now_utc(),
                ))?;

                Ok(attempt)
            }
            Some(HealthCheckOutcome::Failed | HealthCheckOutcome::TimedOut) => {
                attempt.add_issue(issue_from_health_report(
                    attempt
                        .health_check
                        .as_ref()
                        .expect("health report should exist"),
                ));
                self.rollback_docker_service(
                    service,
                    runtime,
                    attempt,
                    previous_state,
                    &backup_name,
                    &previous_image,
                    true,
                )
            }
            None => self.finish_failed_attempt(
                attempt,
                ValidationIssue::new(
                    "health_check.report_missing",
                    "health checks finished without a report",
                ),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rollback_docker_service(
        &self,
        service: &ManagedService,
        runtime: &DockerRuntime,
        mut attempt: UpdateAttemptRecord,
        previous_state: Option<ServiceStateRecord>,
        backup_name: &str,
        previous_image: &str,
        created_new_container: bool,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        attempt.mark_rollback_started();
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::rollback_started(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        if created_new_container {
            if let Err(error) = self.best_effort_stop(&runtime.container_name) {
                attempt.add_issue(issue_from_docker_error(
                    "rollback.stop_failed",
                    "failed to stop the replacement container during rollback",
                    &error,
                ));
            }
            if let Err(error) = self.best_effort_remove(&runtime.container_name) {
                attempt.add_issue(issue_from_docker_error(
                    "rollback.remove_failed",
                    "failed to remove the replacement container during rollback",
                    &error,
                ));
            }
        }

        if let Err(error) = self
            .docker
            .rename_container(backup_name, &runtime.container_name)
        {
            return self.finish_rollback_failed(
                attempt,
                issue_from_docker_error(
                    "rollback.rename_failed",
                    "failed to restore the previous container name",
                    &error,
                ),
            );
        }

        if let Err(error) = self.docker.start_container(&runtime.container_name) {
            return self.finish_rollback_failed(
                attempt,
                issue_from_docker_error(
                    "rollback.start_failed",
                    "failed to restart the previous container",
                    &error,
                ),
            );
        }

        let restored_state = restored_service_state(
            service,
            previous_state.as_ref(),
            previous_image,
            OffsetDateTime::now_utc(),
        );
        attempt.mark_rolled_back(OffsetDateTime::now_utc());
        self.store.save_service_state(&restored_state)?;
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::rollback_finished(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        Ok(attempt)
    }

    fn apply_compose_service(
        &self,
        service: &ManagedService,
        runtime: &ComposeRuntime,
        candidate_release: &CandidateReleaseRecord,
        previous_state: Option<ServiceStateRecord>,
        mut attempt: UpdateAttemptRecord,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        if attempt.previous_image_reference.is_none() {
            attempt.previous_image_reference = match self.compose.read_service_image(runtime) {
                Ok(image) => Some(
                    previous_state
                        .as_ref()
                        .map(|state| state.active_image_reference.clone())
                        .unwrap_or(image),
                ),
                Err(error) => {
                    return self.finish_failed_attempt(
                        attempt,
                        issue_from_compose_error(
                            "compose.image_lookup_failed",
                            "failed to resolve the current compose image",
                            &error,
                        ),
                    );
                }
            };
            self.persist_attempt(&attempt)?;
        }

        let previous_image = match attempt.previous_image_reference.clone() {
            Some(image) => image,
            None => {
                return self.finish_failed_attempt(
                    attempt,
                    ValidationIssue::new(
                        "compose.previous_image_unknown",
                        "could not determine the previous compose image for rollback",
                    ),
                );
            }
        };

        if let Err(error) = self
            .compose
            .apply_service_image(runtime, &candidate_release.image_reference)
        {
            attempt.add_issue(issue_from_compose_error(
                "compose.apply_failed",
                "failed to apply the compose image override",
                &error,
            ));
            return self.rollback_compose_service(
                service,
                runtime,
                attempt,
                previous_state,
                &previous_image,
            );
        }

        attempt.mark_health_checking();
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::health_check_started(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        let report = self.wait_for_compose_service_health(
            &runtime.project,
            &runtime.service,
            &service.health_check,
        );
        attempt.health_check = Some(report);
        self.persist_attempt(&attempt)?;
        self.store
            .append_audit_event(&audit::health_check_finished(
                &attempt,
                OffsetDateTime::now_utc(),
            ))?;

        match attempt.health_check.as_ref().map(|report| report.outcome) {
            Some(HealthCheckOutcome::Passed) => {
                let service_state = committed_service_state(
                    service,
                    candidate_release,
                    previous_state.as_ref(),
                    &previous_image,
                    OffsetDateTime::now_utc(),
                );
                attempt.mark_succeeded(OffsetDateTime::now_utc());
                self.store.save_service_state(&service_state)?;
                self.persist_attempt(&attempt)?;
                self.store.append_audit_event(&audit::update_finished(
                    &attempt,
                    OffsetDateTime::now_utc(),
                ))?;

                Ok(attempt)
            }
            Some(HealthCheckOutcome::Failed | HealthCheckOutcome::TimedOut) => {
                attempt.add_issue(issue_from_health_report(
                    attempt
                        .health_check
                        .as_ref()
                        .expect("health report should exist"),
                ));
                self.rollback_compose_service(
                    service,
                    runtime,
                    attempt,
                    previous_state,
                    &previous_image,
                )
            }
            None => self.finish_failed_attempt(
                attempt,
                ValidationIssue::new(
                    "health_check.report_missing",
                    "health checks finished without a report",
                ),
            ),
        }
    }

    fn rollback_compose_service(
        &self,
        service: &ManagedService,
        runtime: &ComposeRuntime,
        mut attempt: UpdateAttemptRecord,
        previous_state: Option<ServiceStateRecord>,
        previous_image: &str,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        attempt.mark_rollback_started();
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::rollback_started(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        if let Err(error) = self.compose.apply_service_image(runtime, previous_image) {
            return self.finish_rollback_failed(
                attempt,
                issue_from_compose_error(
                    "rollback.compose_apply_failed",
                    "failed to restore the previous compose image",
                    &error,
                ),
            );
        }

        let restored_state = restored_service_state(
            service,
            previous_state.as_ref(),
            previous_image,
            OffsetDateTime::now_utc(),
        );
        attempt.mark_rolled_back(OffsetDateTime::now_utc());
        self.store.save_service_state(&restored_state)?;
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::rollback_finished(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        Ok(attempt)
    }

    fn wait_for_named_container_health(
        &self,
        container_name: &str,
        spec: &crate::domain::HealthCheckSpec,
    ) -> HealthCheckReport {
        self.wait_for_containers(spec, || {
            self.docker
                .inspect_container(container_name)
                .map(|container| vec![container])
        })
    }

    fn wait_for_compose_service_health(
        &self,
        project: &str,
        service: &str,
        spec: &crate::domain::HealthCheckSpec,
    ) -> HealthCheckReport {
        self.wait_for_containers(spec, || {
            let container_ids = self.docker.list_container_ids_by_labels(&[
                ("com.docker.compose.project", project),
                ("com.docker.compose.service", service),
            ])?;
            let mut containers = Vec::with_capacity(container_ids.len());
            for container_id in container_ids {
                containers.push(self.docker.inspect_container(&container_id)?);
            }
            Ok(containers)
        })
    }

    fn wait_for_containers<F>(
        &self,
        spec: &crate::domain::HealthCheckSpec,
        mut fetch_containers: F,
    ) -> HealthCheckReport
    where
        F: FnMut() -> Result<Vec<ObservedContainer>, DockerClientError>,
    {
        let deadline = Instant::now() + Duration::from_secs(spec.timeout_secs);
        let interval = Duration::from_secs(spec.poll_interval_secs);

        loop {
            let pending_message = match fetch_containers() {
                Ok(containers) => match evaluate_containers(&containers, spec.kind) {
                    PollState::Passed(message) => {
                        return HealthCheckReport {
                            kind: spec.kind,
                            outcome: HealthCheckOutcome::Passed,
                            message,
                            checked_at: OffsetDateTime::now_utc(),
                        };
                    }
                    PollState::Failed(message) => {
                        return HealthCheckReport {
                            kind: spec.kind,
                            outcome: HealthCheckOutcome::Failed,
                            message,
                            checked_at: OffsetDateTime::now_utc(),
                        };
                    }
                    PollState::Pending(message) => message,
                },
                Err(error) => format!("waiting for runtime state: {error}"),
            };

            if Instant::now() >= deadline {
                return HealthCheckReport {
                    kind: spec.kind,
                    outcome: HealthCheckOutcome::TimedOut,
                    message: pending_message,
                    checked_at: OffsetDateTime::now_utc(),
                };
            }

            sleep(interval);
        }
    }

    fn finish_failed_attempt(
        &self,
        mut attempt: UpdateAttemptRecord,
        issue: ValidationIssue,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        attempt.add_issue(issue);
        attempt.mark_failed(OffsetDateTime::now_utc());
        self.persist_attempt(&attempt)?;

        Ok(attempt)
    }

    fn finish_rollback_failed(
        &self,
        mut attempt: UpdateAttemptRecord,
        issue: ValidationIssue,
    ) -> Result<UpdateAttemptRecord, UpdateError> {
        attempt.add_issue(issue);
        attempt.mark_rollback_failed(OffsetDateTime::now_utc());
        self.persist_attempt(&attempt)?;
        self.store.append_audit_event(&audit::rollback_finished(
            &attempt,
            OffsetDateTime::now_utc(),
        ))?;

        Ok(attempt)
    }

    fn persist_attempt(&self, attempt: &UpdateAttemptRecord) -> Result<(), UpdateError> {
        self.store
            .save_update_attempt(attempt)
            .map_err(UpdateError::Persistence)
    }

    fn best_effort_stop(&self, container_name: &str) -> Result<(), DockerClientError> {
        match self.docker.stop_container(container_name) {
            Ok(()) => Ok(()),
            Err(error) if is_missing_or_not_modified(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn best_effort_remove(&self, container_name: &str) -> Result<(), DockerClientError> {
        match self.docker.remove_container(container_name, true) {
            Ok(()) => Ok(()),
            Err(error) if is_missing_or_not_modified(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug)]
enum PollState {
    Passed(String),
    Failed(String),
    Pending(String),
}

fn evaluate_containers(containers: &[ObservedContainer], kind: HealthCheckKind) -> PollState {
    if containers.is_empty() {
        return PollState::Pending("waiting for containers to appear".into());
    }

    match kind {
        HealthCheckKind::Running => {
            if containers.iter().all(|container| container.running) {
                PollState::Passed(format!("{} container(s) are running", containers.len()))
            } else {
                PollState::Pending(format!(
                    "waiting for running containers: {}",
                    container_status_line(containers)
                ))
            }
        }
        HealthCheckKind::ContainerHealth => {
            if containers.iter().any(|container| !container.running) {
                return PollState::Pending(format!(
                    "waiting for running containers before checking health: {}",
                    container_status_line(containers)
                ));
            }

            let mut unhealthy = Vec::new();
            let mut missing_health = Vec::new();
            let mut starting = Vec::new();

            for container in containers {
                match container.health {
                    Some(ContainerHealthState::Healthy) => {}
                    Some(ContainerHealthState::Unhealthy) => unhealthy.push(container.name.clone()),
                    Some(ContainerHealthState::Starting) => starting.push(container.name.clone()),
                    None => missing_health.push(container.name.clone()),
                }
            }

            if !unhealthy.is_empty() {
                return PollState::Failed(format!(
                    "containers reported unhealthy: {}",
                    unhealthy.join(", ")
                ));
            }

            if !missing_health.is_empty() {
                return PollState::Failed(format!(
                    "containers are missing Docker health status: {}",
                    missing_health.join(", ")
                ));
            }

            if !starting.is_empty() {
                return PollState::Pending(format!(
                    "waiting for healthy containers: {}",
                    starting.join(", ")
                ));
            }

            PollState::Passed(format!(
                "{} container(s) reported healthy",
                containers.len()
            ))
        }
    }
}

fn container_status_line(containers: &[ObservedContainer]) -> String {
    containers
        .iter()
        .map(|container| {
            let state = if container.running {
                "running"
            } else {
                "not-running"
            };
            match container.health {
                Some(ContainerHealthState::Starting) => {
                    format!("{} ({state}, health=starting)", container.name)
                }
                Some(ContainerHealthState::Healthy) => {
                    format!("{} ({state}, health=healthy)", container.name)
                }
                Some(ContainerHealthState::Unhealthy) => {
                    format!("{} ({state}, health=unhealthy)", container.name)
                }
                None => format!("{} ({state})", container.name),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn committed_service_state(
    service: &ManagedService,
    candidate_release: &CandidateReleaseRecord,
    previous_state: Option<&ServiceStateRecord>,
    previous_image: &str,
    updated_at: OffsetDateTime,
) -> ServiceStateRecord {
    ServiceStateRecord::new(
        service.name.clone(),
        Some(candidate_release.candidate_release_id),
        candidate_release.image_reference.clone(),
        previous_state.and_then(|state| state.active_candidate_release_id),
        Some(previous_image.to_string()),
        updated_at,
    )
}

fn restored_service_state(
    service: &ManagedService,
    previous_state: Option<&ServiceStateRecord>,
    previous_image: &str,
    updated_at: OffsetDateTime,
) -> ServiceStateRecord {
    match previous_state {
        Some(state) => {
            let mut restored = state.clone();
            restored.updated_at = updated_at;
            restored
        }
        None => ServiceStateRecord::new(
            service.name.clone(),
            None,
            previous_image.to_string(),
            None,
            Some(previous_image.to_string()),
            updated_at,
        ),
    }
}

fn rollback_container_name(container_name: &str, update_id: Uuid) -> String {
    let suffix = update_id.simple().to_string();
    format!("{container_name}-caisson-rollback-{}", &suffix[..8])
}

fn issue_from_docker_error(code: &str, prefix: &str, error: &DockerClientError) -> ValidationIssue {
    ValidationIssue::new(code, format!("{prefix}: {error}"))
}

fn issue_from_compose_error(code: &str, prefix: &str, error: &ComposeError) -> ValidationIssue {
    ValidationIssue::new(code, format!("{prefix}: {error}"))
}

fn issue_from_health_report(report: &HealthCheckReport) -> ValidationIssue {
    ValidationIssue::new(
        match report.outcome {
            HealthCheckOutcome::Passed => "health_check.passed",
            HealthCheckOutcome::Failed => "health_check.failed",
            HealthCheckOutcome::TimedOut => "health_check.timed_out",
        },
        report.message.clone(),
    )
}

fn is_missing_or_not_modified(error: &DockerClientError) -> bool {
    matches!(
        error,
        DockerClientError::Api(bollard::errors::Error::DockerResponseServerError {
            status_code: 404 | 304,
            ..
        })
    )
}

/// Errors that should stop the update workflow entirely.
#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("update precondition failed: {0}")]
    Precondition(String),
    #[error("failed to persist update state: {0}")]
    Persistence(#[from] PersistenceError),
}
