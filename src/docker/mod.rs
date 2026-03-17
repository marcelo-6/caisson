//! Docker image import and inspection support.
//!
//! The rest of the crate should not need to care whether Docker access happens
//! through `bollard`, a fake client in tests, or some other implementation
//! detail.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use bollard::Docker;
use bollard::body_full;
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, HealthStatusEnum, NetworkingConfig,
};
use bollard::query_parameters::ImportImageOptionsBuilder;
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, InspectContainerOptionsBuilder, ListContainersOptionsBuilder,
    RemoveContainerOptionsBuilder, RenameContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use bytes::Bytes;
use futures_util::StreamExt;
use thiserror::Error;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

use crate::audit;
use crate::domain::{
    CandidateReleaseRecord, ImageImportRecord, ImportedImageMetadata, ValidationIssue,
    ValidationRecord, ValidationStatus,
};
use crate::package::{ImageArchiveAccessError, extract_image_archive_to_temp_file};
use crate::persistence::{PersistenceError, StateStore};

pub trait DockerImageClient: std::fmt::Debug {
    fn load_image_archive(&self, archive_path: &Path) -> Result<(), DockerClientError>;
    fn inspect_image(
        &self,
        image_reference: &str,
    ) -> Result<ImportedImageMetadata, DockerClientError>;
}

/// Normalized view of a container for update and health logic.
#[derive(Debug, Clone)]
pub struct ObservedContainer {
    pub container_id: String,
    pub name: String,
    pub image_reference: Option<String>,
    pub labels: HashMap<String, String>,
    pub running: bool,
    pub health: Option<ContainerHealthState>,
    pub create_body: ContainerCreateBody,
}

/// Narrow health states used by the updater.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ContainerHealthState {
    Starting,
    Healthy,
    Unhealthy,
}

/// Docker operations used by the service update workflow.
pub trait DockerServiceClient: std::fmt::Debug {
    fn inspect_container(
        &self,
        container_name: &str,
    ) -> Result<ObservedContainer, DockerClientError>;
    fn stop_container(&self, container_name: &str) -> Result<(), DockerClientError>;
    fn rename_container(
        &self,
        container_name: &str,
        new_name: &str,
    ) -> Result<(), DockerClientError>;
    fn create_container_from(
        &self,
        container_name: &str,
        source: &ObservedContainer,
        image_reference: &str,
    ) -> Result<(), DockerClientError>;
    fn start_container(&self, container_name: &str) -> Result<(), DockerClientError>;
    fn remove_container(&self, container_name: &str, force: bool) -> Result<(), DockerClientError>;
    fn list_container_ids_by_labels(
        &self,
        labels: &[(&str, &str)],
    ) -> Result<Vec<String>, DockerClientError>;
}

/// `bollard` Docker image client for the local daemon.
#[derive(Debug, Clone)]
pub struct BollardDockerClient {
    docker: Docker,
    runtime: Arc<Runtime>,
}

impl BollardDockerClient {
    /// Connects to the local Docker daemon using `bollard` defaults.
    pub fn connect_local_defaults() -> Result<Self, DockerClientError> {
        let docker = Docker::connect_with_local_defaults().map_err(DockerClientError::Connect)?;
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(DockerClientError::Runtime)?;

        Ok(Self {
            docker,
            runtime: Arc::new(runtime),
        })
    }
}

impl DockerImageClient for BollardDockerClient {
    fn load_image_archive(&self, archive_path: &Path) -> Result<(), DockerClientError> {
        let bytes = std::fs::read(archive_path).map_err(|source| DockerClientError::ArchiveIo {
            path: archive_path.display().to_string(),
            source,
        })?;
        let bytes = Bytes::from(bytes);

        self.runtime.block_on(async {
            let mut output = self.docker.import_image(
                ImportImageOptionsBuilder::default().build(),
                body_full(bytes),
                None,
            );
            while let Some(progress) = output.next().await {
                match progress {
                    Ok(_) => {}
                    Err(bollard::errors::Error::DockerStreamError { error }) => {
                        return Err(DockerClientError::ImportFailed(error));
                    }
                    Err(error) => return Err(DockerClientError::Api(error)),
                }
            }

            Ok(())
        })
    }

    fn inspect_image(
        &self,
        image_reference: &str,
    ) -> Result<ImportedImageMetadata, DockerClientError> {
        self.runtime.block_on(async {
            let details = self
                .docker
                .inspect_image(image_reference)
                .await
                .map_err(DockerClientError::Api)?;

            Ok(ImportedImageMetadata {
                image_id: details.id.unwrap_or_else(|| image_reference.to_string()),
                repo_tags: details.repo_tags.unwrap_or_default(),
                repo_digests: details.repo_digests.unwrap_or_default(),
                architecture: details.architecture,
                os: details.os,
            })
        })
    }
}

impl DockerServiceClient for BollardDockerClient {
    fn inspect_container(
        &self,
        container_name: &str,
    ) -> Result<ObservedContainer, DockerClientError> {
        self.runtime.block_on(async {
            let details = self
                .docker
                .inspect_container(
                    container_name,
                    Some(
                        InspectContainerOptionsBuilder::default()
                            .size(false)
                            .build(),
                    ),
                )
                .await
                .map_err(DockerClientError::Api)?;

            observed_container_from_inspect(details)
        })
    }

    fn stop_container(&self, container_name: &str) -> Result<(), DockerClientError> {
        self.runtime.block_on(async {
            self.docker
                .stop_container(
                    container_name,
                    Some(StopContainerOptionsBuilder::default().t(10).build()),
                )
                .await
                .map_err(DockerClientError::Api)
        })
    }

    fn rename_container(
        &self,
        container_name: &str,
        new_name: &str,
    ) -> Result<(), DockerClientError> {
        self.runtime.block_on(async {
            self.docker
                .rename_container(
                    container_name,
                    RenameContainerOptionsBuilder::default()
                        .name(new_name)
                        .build(),
                )
                .await
                .map_err(DockerClientError::Api)
        })
    }

    fn create_container_from(
        &self,
        container_name: &str,
        source: &ObservedContainer,
        image_reference: &str,
    ) -> Result<(), DockerClientError> {
        let mut body = source.create_body.clone();
        body.image = Some(image_reference.to_string());

        self.runtime.block_on(async {
            self.docker
                .create_container(
                    Some(
                        CreateContainerOptionsBuilder::default()
                            .name(container_name)
                            .build(),
                    ),
                    body,
                )
                .await
                .map(|_| ())
                .map_err(DockerClientError::Api)
        })
    }

    fn start_container(&self, container_name: &str) -> Result<(), DockerClientError> {
        self.runtime.block_on(async {
            self.docker
                .start_container(
                    container_name,
                    None::<bollard::query_parameters::StartContainerOptions>,
                )
                .await
                .map_err(DockerClientError::Api)
        })
    }

    fn remove_container(&self, container_name: &str, force: bool) -> Result<(), DockerClientError> {
        self.runtime.block_on(async {
            self.docker
                .remove_container(
                    container_name,
                    Some(
                        RemoveContainerOptionsBuilder::default()
                            .force(force)
                            .build(),
                    ),
                )
                .await
                .map_err(DockerClientError::Api)
        })
    }

    fn list_container_ids_by_labels(
        &self,
        labels: &[(&str, &str)],
    ) -> Result<Vec<String>, DockerClientError> {
        let mut filters = HashMap::new();
        filters.insert(
            "label".to_string(),
            labels
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect::<Vec<_>>(),
        );

        self.runtime.block_on(async {
            self.docker
                .list_containers(Some(
                    ListContainersOptionsBuilder::default()
                        .all(true)
                        .filters(&filters)
                        .build(),
                ))
                .await
                .map_err(DockerClientError::Api)
                .map(|containers| {
                    containers
                        .into_iter()
                        .filter_map(|container| container.id)
                        .collect()
                })
        })
    }
}

fn observed_container_from_inspect(
    details: ContainerInspectResponse,
) -> Result<ObservedContainer, DockerClientError> {
    let create_body = create_body_from_inspect(&details)?;
    let state = details.state.as_ref();
    let running = state.and_then(|state| state.running).unwrap_or(false);
    let health = state
        .and_then(|state| state.health.as_ref())
        .and_then(|health| match health.status {
            Some(HealthStatusEnum::STARTING) => Some(ContainerHealthState::Starting),
            Some(HealthStatusEnum::HEALTHY) => Some(ContainerHealthState::Healthy),
            Some(HealthStatusEnum::UNHEALTHY) => Some(ContainerHealthState::Unhealthy),
            _ => None,
        });
    let labels = create_body.labels.clone().unwrap_or_default();

    Ok(ObservedContainer {
        container_id: details.id.unwrap_or_default(),
        name: details
            .name
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string(),
        image_reference: create_body.image.clone(),
        labels,
        running,
        health,
        create_body,
    })
}

fn create_body_from_inspect(
    details: &ContainerInspectResponse,
) -> Result<ContainerCreateBody, DockerClientError> {
    let config = details.config.clone().ok_or_else(|| {
        DockerClientError::InvalidContainerConfig(
            "inspected container is missing its creation config".into(),
        )
    })?;

    let networking_config = details
        .network_settings
        .as_ref()
        .and_then(|settings| settings.networks.clone())
        .map(|endpoints_config| NetworkingConfig {
            endpoints_config: Some(endpoints_config),
        });

    Ok(ContainerCreateBody {
        hostname: config.hostname,
        domainname: config.domainname,
        user: config.user,
        attach_stdin: config.attach_stdin,
        attach_stdout: config.attach_stdout,
        attach_stderr: config.attach_stderr,
        exposed_ports: config.exposed_ports,
        tty: config.tty,
        open_stdin: config.open_stdin,
        stdin_once: config.stdin_once,
        env: config.env,
        cmd: config.cmd,
        healthcheck: config.healthcheck,
        args_escaped: config.args_escaped,
        image: config.image,
        volumes: config.volumes,
        working_dir: config.working_dir,
        entrypoint: config.entrypoint,
        network_disabled: config.network_disabled,
        on_build: config.on_build,
        labels: config.labels,
        stop_signal: config.stop_signal,
        stop_timeout: config.stop_timeout,
        shell: config.shell,
        host_config: details.host_config.clone(),
        networking_config,
    })
}

/// Backend service that imports staged package images into Docker.
#[derive(Debug)]
pub struct ImageImportService<D, S> {
    docker: D,
    store: S,
}

impl<D, S> ImageImportService<D, S>
where
    D: DockerImageClient,
    S: StateStore,
{
    /// Creates a new image-import service.
    pub fn new(docker: D, store: S) -> Self {
        Self { docker, store }
    }

    /// Imports a Docker image from a previously accepted validation record.
    ///
    /// Docker failures are captured into the returned `ImageImportRecord`
    pub fn import_validated_package(
        &self,
        validation_record: &ValidationRecord,
    ) -> Result<ImageImportRecord, ImageImportError> {
        if validation_record.status != ValidationStatus::Accepted {
            return Err(ImageImportError::Precondition(
                "image import requires an accepted validation record".into(),
            ));
        }

        let staged_path = validation_record.staged_path.as_ref().ok_or_else(|| {
            ImageImportError::Precondition(
                "accepted validation record is missing staged package path".into(),
            )
        })?;
        let manifest = validation_record.manifest.as_ref().ok_or_else(|| {
            ImageImportError::Precondition(
                "accepted validation record is missing manifest metadata".into(),
            )
        })?;

        let now = time::OffsetDateTime::now_utc();
        let mut import_record = ImageImportRecord::new(
            Uuid::new_v4(),
            validation_record.attempt_id,
            manifest.target.service.clone(),
            manifest.image.reference.clone(),
            manifest.package_version.clone(),
            now,
        );

        self.store
            .append_audit_event(&audit::image_import_started(
                validation_record.attempt_id,
                &manifest.image.reference,
                now,
            ))
            .map_err(ImageImportError::Persistence)?;

        let extracted_archive = match extract_image_archive_to_temp_file(staged_path) {
            Ok(archive) => archive,
            Err(error) => {
                import_record.fail_with(issue_from_archive_access_error(&error));
                self.persist_import_failure(&import_record)?;
                return Ok(import_record);
            }
        };

        if let Err(error) = self.docker.load_image_archive(extracted_archive.path()) {
            import_record.fail_with(issue_from_docker_error(
                "docker.load_failed",
                "failed to load image archive into Docker",
                &error,
            ));
            self.persist_import_failure(&import_record)?;
            return Ok(import_record);
        }

        let imported_image = match self.docker.inspect_image(&manifest.image.reference) {
            Ok(imported_image) => imported_image,
            Err(error) => {
                import_record.fail_with(issue_from_docker_error(
                    "docker.inspect_failed",
                    "failed to inspect imported image",
                    &error,
                ));
                self.persist_import_failure(&import_record)?;
                return Ok(import_record);
            }
        };

        let candidate_release = CandidateReleaseRecord::new(
            Uuid::new_v4(),
            import_record.import_id,
            validation_record.attempt_id,
            manifest.target.service.clone(),
            manifest.image.reference.clone(),
            manifest.package_version.clone(),
            time::OffsetDateTime::now_utc(),
            imported_image.clone(),
        );
        import_record.mark_imported(imported_image, candidate_release.candidate_release_id);

        self.store
            .save_candidate_release(&candidate_release)
            .map_err(ImageImportError::Persistence)?;
        self.store
            .save_image_import_record(&import_record)
            .map_err(ImageImportError::Persistence)?;
        self.store
            .append_audit_event(&audit::image_import_finished(
                &import_record,
                time::OffsetDateTime::now_utc(),
            ))
            .map_err(ImageImportError::Persistence)?;

        Ok(import_record)
    }

    fn persist_import_failure(
        &self,
        import_record: &ImageImportRecord,
    ) -> Result<(), ImageImportError> {
        self.store
            .save_image_import_record(import_record)
            .map_err(ImageImportError::Persistence)?;
        self.store
            .append_audit_event(&audit::image_import_finished(
                import_record,
                time::OffsetDateTime::now_utc(),
            ))
            .map_err(ImageImportError::Persistence)?;

        Ok(())
    }
}

fn issue_from_archive_access_error(error: &ImageArchiveAccessError) -> ValidationIssue {
    ValidationIssue::new(
        "package.image_archive_access_failed",
        format!("failed to access staged image.tar: {error}"),
    )
}

fn issue_from_docker_error(code: &str, prefix: &str, error: &DockerClientError) -> ValidationIssue {
    ValidationIssue::new(code, format!("{prefix}: {error}"))
}

/// Errors used by the Docker adapter.
#[derive(Debug, Error)]
pub enum DockerClientError {
    #[error("failed to connect to the local Docker daemon: {0}")]
    Connect(bollard::errors::Error),
    #[error("failed to create Tokio runtime for Docker operations: {0}")]
    Runtime(std::io::Error),
    #[error("inspected container could not be reused: {0}")]
    InvalidContainerConfig(String),
    #[error("failed to open image archive `{path}`: {source}")]
    ArchiveIo {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Docker API error: {0}")]
    Api(bollard::errors::Error),
    #[error("Docker reported image import failure: {0}")]
    ImportFailed(String),
}

/// Errors that should stop the image-import workflow entirely.
#[derive(Debug, Error)]
pub enum ImageImportError {
    #[error("image import precondition failed: {0}")]
    Precondition(String),
    #[error("failed to persist image import state: {0}")]
    Persistence(#[from] PersistenceError),
}
