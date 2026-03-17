mod common;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bollard::models::ContainerCreateBody;
use caisson::app::{ApplyCandidateReleaseRequest, UpdateApp};
use caisson::compose::{ComposeClient, ComposeError};
use caisson::config::load_service_catalog;
use caisson::docker::{
    ContainerHealthState, DockerClientError, DockerServiceClient, ObservedContainer,
};
use caisson::domain::{
    CandidateReleaseRecord, HealthCheckKind, ImportedImageMetadata, UpdateAttemptStatus,
};
use caisson::persistence::{FilesystemStore, StateStore};
use semver::Version;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
struct FakeDockerClient {
    state: Arc<Mutex<FakeDockerState>>,
}

#[derive(Debug)]
struct FakeDockerState {
    containers: HashMap<String, FakeContainer>,
    created_container_health: Option<ContainerHealthState>,
    created_container_running: bool,
    fail_create: bool,
    fail_start: bool,
    fail_rename: bool,
    operations: Vec<String>,
}

#[derive(Debug, Clone)]
struct FakeContainer {
    container_id: String,
    name: String,
    image_reference: String,
    running: bool,
    health: Option<ContainerHealthState>,
    labels: HashMap<String, String>,
    create_body: ContainerCreateBody,
}

impl FakeDockerClient {
    fn with_containers(containers: Vec<FakeContainer>) -> Self {
        let containers = containers
            .into_iter()
            .map(|container| (container.name.clone(), container))
            .collect();

        Self {
            state: Arc::new(Mutex::new(FakeDockerState {
                containers,
                created_container_health: None,
                created_container_running: true,
                fail_create: false,
                fail_start: false,
                fail_rename: false,
                operations: Vec::new(),
            })),
        }
    }

    fn operations(&self) -> Vec<String> {
        self.state.lock().expect("lock").operations.clone()
    }
}

impl DockerServiceClient for FakeDockerClient {
    fn inspect_container(
        &self,
        container_name: &str,
    ) -> Result<ObservedContainer, DockerClientError> {
        let state = self.state.lock().expect("lock");
        let container = state
            .containers
            .get(container_name)
            .or_else(|| {
                state
                    .containers
                    .values()
                    .find(|container| container.container_id == container_name)
            })
            .ok_or_else(|| {
                DockerClientError::ImportFailed(format!("missing container `{container_name}`"))
            })?;

        Ok(ObservedContainer {
            container_id: container.container_id.clone(),
            name: container.name.clone(),
            image_reference: Some(container.image_reference.clone()),
            labels: container.labels.clone(),
            running: container.running,
            health: container.health,
            create_body: container.create_body.clone(),
        })
    }

    fn stop_container(&self, container_name: &str) -> Result<(), DockerClientError> {
        let mut state = self.state.lock().expect("lock");
        state.operations.push(format!("stop:{container_name}"));
        if let Some(container) = state.containers.get_mut(container_name) {
            container.running = false;
            Ok(())
        } else {
            Err(DockerClientError::ImportFailed(format!(
                "missing container `{container_name}`"
            )))
        }
    }

    fn rename_container(
        &self,
        container_name: &str,
        new_name: &str,
    ) -> Result<(), DockerClientError> {
        let mut state = self.state.lock().expect("lock");
        state
            .operations
            .push(format!("rename:{container_name}->{new_name}"));
        if state.fail_rename {
            return Err(DockerClientError::ImportFailed("rename failed".into()));
        }

        let mut container = state.containers.remove(container_name).ok_or_else(|| {
            DockerClientError::ImportFailed(format!("missing container `{container_name}`"))
        })?;
        container.name = new_name.to_string();
        state.containers.insert(new_name.to_string(), container);

        Ok(())
    }

    fn create_container_from(
        &self,
        container_name: &str,
        source: &ObservedContainer,
        image_reference: &str,
    ) -> Result<(), DockerClientError> {
        let mut state = self.state.lock().expect("lock");
        state
            .operations
            .push(format!("create:{container_name}:{image_reference}"));
        if state.fail_create {
            return Err(DockerClientError::ImportFailed("create failed".into()));
        }

        let mut create_body = source.create_body.clone();
        create_body.image = Some(image_reference.to_string());
        let created_container_running = state.created_container_running;
        let created_container_health = state.created_container_health;
        state.containers.insert(
            container_name.to_string(),
            FakeContainer {
                container_id: format!("{container_name}-new-id"),
                name: container_name.to_string(),
                image_reference: image_reference.to_string(),
                running: created_container_running,
                health: created_container_health,
                labels: source.labels.clone(),
                create_body,
            },
        );

        Ok(())
    }

    fn start_container(&self, container_name: &str) -> Result<(), DockerClientError> {
        let mut state = self.state.lock().expect("lock");
        state.operations.push(format!("start:{container_name}"));
        if state.fail_start {
            return Err(DockerClientError::ImportFailed("start failed".into()));
        }

        let container = state.containers.get_mut(container_name).ok_or_else(|| {
            DockerClientError::ImportFailed(format!("missing container `{container_name}`"))
        })?;
        container.running = true;

        Ok(())
    }

    fn remove_container(
        &self,
        container_name: &str,
        _force: bool,
    ) -> Result<(), DockerClientError> {
        let mut state = self.state.lock().expect("lock");
        state.operations.push(format!("remove:{container_name}"));
        state.containers.remove(container_name).ok_or_else(|| {
            DockerClientError::ImportFailed(format!("missing container `{container_name}`"))
        })?;

        Ok(())
    }

    fn list_container_ids_by_labels(
        &self,
        labels: &[(&str, &str)],
    ) -> Result<Vec<String>, DockerClientError> {
        let state = self.state.lock().expect("lock");
        Ok(state
            .containers
            .values()
            .filter(|container| {
                labels.iter().all(|(key, value)| {
                    container
                        .labels
                        .get(*key)
                        .map(|candidate| candidate == value)
                        .unwrap_or(false)
                })
            })
            .map(|container| container.container_id.clone())
            .collect())
    }
}

#[derive(Debug, Clone)]
struct FakeComposeClient {
    state: Arc<Mutex<FakeComposeState>>,
}

#[derive(Debug)]
struct FakeComposeState {
    current_image: String,
    applied_images: Vec<String>,
    fail_images: HashSet<String>,
}

impl FakeComposeClient {
    fn new(current_image: &str) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeComposeState {
                current_image: current_image.to_string(),
                applied_images: Vec::new(),
                fail_images: HashSet::new(),
            })),
        }
    }

    fn applied_images(&self) -> Vec<String> {
        self.state.lock().expect("lock").applied_images.clone()
    }
}

impl ComposeClient for FakeComposeClient {
    fn read_service_image(
        &self,
        _runtime: &caisson::domain::ComposeRuntime,
    ) -> Result<String, ComposeError> {
        Ok(self.state.lock().expect("lock").current_image.clone())
    }

    fn apply_service_image(
        &self,
        _runtime: &caisson::domain::ComposeRuntime,
        image_reference: &str,
    ) -> Result<(), ComposeError> {
        let mut state = self.state.lock().expect("lock");
        state.applied_images.push(image_reference.to_string());
        if state.fail_images.contains(image_reference) {
            return Err(ComposeError::Validation("apply failed".into()));
        }
        state.current_image = image_reference.to_string();

        Ok(())
    }
}

#[test]
fn applies_a_docker_service_and_commits_service_state() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let store = FilesystemStore::new(temp_dir.path().join("state"));
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let candidate_release = write_candidate_release(&store, "frontend", "example/frontend:1.2.3");
    let docker = FakeDockerClient::with_containers(vec![docker_container(
        "frontend",
        "example/frontend:current",
        true,
        None,
        HashMap::new(),
    )]);
    let compose = FakeComposeClient::new("example/backend:current");
    let app = UpdateApp::new(catalog, docker.clone(), compose, store.clone());

    let update = app
        .apply_candidate_release(ApplyCandidateReleaseRequest {
            candidate_release_id: candidate_release.candidate_release_id,
        })
        .expect("update should complete");

    assert_eq!(update.status, UpdateAttemptStatus::Succeeded);
    let state = store
        .load_service_state("frontend")
        .expect("state load should work")
        .expect("service state should exist");
    assert_eq!(
        state.active_candidate_release_id,
        Some(candidate_release.candidate_release_id)
    );
    assert_eq!(state.active_image_reference, "example/frontend:1.2.3");
    assert_eq!(
        state.previous_known_good_image_reference.as_deref(),
        Some("example/frontend:current")
    );

    let audit_log = std::fs::read_to_string(store.root().join("audit").join("events.jsonl"))
        .expect("audit log");
    assert!(audit_log.contains("update_started"));
    assert!(audit_log.contains("health_check_passed"));
    assert!(audit_log.contains("update_committed"));

    let operations = docker.operations();
    assert!(
        operations
            .iter()
            .any(|operation| operation.starts_with("rename:frontend->"))
    );
    assert!(
        operations
            .iter()
            .any(|operation| operation.starts_with("remove:frontend-caisson-rollback-"))
    );
}

#[test]
fn rolls_back_a_docker_service_when_health_checks_fail() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let store = FilesystemStore::new(temp_dir.path().join("state"));
    let mut catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let frontend = catalog
        .services
        .iter_mut()
        .find(|service| service.name == "frontend")
        .expect("frontend service should exist");
    frontend.health_check.kind = HealthCheckKind::ContainerHealth;
    frontend.health_check.timeout_secs = 1;
    frontend.health_check.poll_interval_secs = 1;

    let candidate_release = write_candidate_release(&store, "frontend", "example/frontend:1.2.3");
    let docker = FakeDockerClient::with_containers(vec![docker_container(
        "frontend",
        "example/frontend:current",
        true,
        Some(ContainerHealthState::Healthy),
        HashMap::new(),
    )]);
    {
        let mut state = docker.state.lock().expect("lock");
        state.created_container_health = Some(ContainerHealthState::Unhealthy);
    }
    let compose = FakeComposeClient::new("example/backend:current");
    let app = UpdateApp::new(catalog, docker.clone(), compose, store.clone());

    let update = app
        .apply_candidate_release(ApplyCandidateReleaseRequest {
            candidate_release_id: candidate_release.candidate_release_id,
        })
        .expect("update should complete");

    assert_eq!(update.status, UpdateAttemptStatus::RolledBack);
    let state = store
        .load_service_state("frontend")
        .expect("state load should work")
        .expect("service state should exist");
    assert_eq!(state.active_image_reference, "example/frontend:current");

    let audit_log = std::fs::read_to_string(store.root().join("audit").join("events.jsonl"))
        .expect("audit log");
    assert!(audit_log.contains("health_check_failed"));
    assert!(audit_log.contains("rollback_succeeded"));
}

#[test]
fn applies_a_compose_service_and_persists_the_candidate() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let store = FilesystemStore::new(temp_dir.path().join("state"));
    let candidate_release = write_candidate_release(&store, "backend", "example/backend:2.0.0");
    let docker = FakeDockerClient::with_containers(vec![docker_container(
        "backend-1",
        "example/backend:2.0.0",
        true,
        None,
        compose_labels("caisson-stack", "backend"),
    )]);
    let compose = FakeComposeClient::new("example/backend:current");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = UpdateApp::new(catalog, docker, compose.clone(), store.clone());

    let update = app
        .apply_candidate_release(ApplyCandidateReleaseRequest {
            candidate_release_id: candidate_release.candidate_release_id,
        })
        .expect("update should complete");

    assert_eq!(update.status, UpdateAttemptStatus::Succeeded);
    assert_eq!(compose.applied_images(), vec!["example/backend:2.0.0"]);

    let state = store
        .load_service_state("backend")
        .expect("state load should work")
        .expect("service state should exist");
    assert_eq!(state.active_image_reference, "example/backend:2.0.0");
}

#[test]
fn rolls_back_a_compose_service_when_health_checks_fail() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let store = FilesystemStore::new(temp_dir.path().join("state"));
    let mut catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let backend = catalog
        .services
        .iter_mut()
        .find(|service| service.name == "backend")
        .expect("backend service should exist");
    backend.health_check.kind = HealthCheckKind::ContainerHealth;
    backend.health_check.timeout_secs = 1;
    backend.health_check.poll_interval_secs = 1;

    let candidate_release = write_candidate_release(&store, "backend", "example/backend:2.0.0");
    let docker = FakeDockerClient::with_containers(vec![docker_container(
        "backend-1",
        "example/backend:2.0.0",
        true,
        Some(ContainerHealthState::Unhealthy),
        compose_labels("caisson-stack", "backend"),
    )]);
    let compose = FakeComposeClient::new("example/backend:current");
    let app = UpdateApp::new(catalog, docker, compose.clone(), store.clone());

    let update = app
        .apply_candidate_release(ApplyCandidateReleaseRequest {
            candidate_release_id: candidate_release.candidate_release_id,
        })
        .expect("update should complete");

    assert_eq!(update.status, UpdateAttemptStatus::RolledBack);
    assert_eq!(
        compose.applied_images(),
        vec!["example/backend:2.0.0", "example/backend:current"]
    );

    let audit_log = std::fs::read_to_string(store.root().join("audit").join("events.jsonl"))
        .expect("audit log");
    assert!(audit_log.contains("rollback_succeeded"));
}

fn write_candidate_release(
    store: &FilesystemStore,
    service_name: &str,
    image_reference: &str,
) -> CandidateReleaseRecord {
    let candidate_release = CandidateReleaseRecord::new(
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        service_name.to_string(),
        image_reference.to_string(),
        Version::parse("1.2.3").expect("version"),
        OffsetDateTime::now_utc(),
        ImportedImageMetadata {
            image_id: format!("sha256:{service_name}"),
            repo_tags: vec![image_reference.to_string()],
            repo_digests: vec![format!("{image_reference}@sha256:{service_name}")],
            architecture: Some("amd64".into()),
            os: Some("linux".into()),
        },
    );
    store
        .save_candidate_release(&candidate_release)
        .expect("candidate release should save");

    candidate_release
}

fn docker_container(
    name: &str,
    image_reference: &str,
    running: bool,
    health: Option<ContainerHealthState>,
    labels: HashMap<String, String>,
) -> FakeContainer {
    FakeContainer {
        container_id: format!("{name}-id"),
        name: name.to_string(),
        image_reference: image_reference.to_string(),
        running,
        health,
        labels: labels.clone(),
        create_body: ContainerCreateBody {
            image: Some(image_reference.to_string()),
            labels: Some(labels),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
            ..Default::default()
        },
    }
}

fn compose_labels(project: &str, service: &str) -> HashMap<String, String> {
    HashMap::from([
        ("com.docker.compose.project".into(), project.into()),
        ("com.docker.compose.service".into(), service.into()),
    ])
}
