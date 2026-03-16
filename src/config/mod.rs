//! Service catalog loading and validation.
//!
//! The config loader is strict right now.
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::domain::{
    ComposeRuntime, DockerRuntime, ManagedService, RuntimeMode, SUPPORTED_SERVICE_CATALOG_VERSION,
    ServiceCatalog,
};

/// Errors produced while loading `services.toml`.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read service catalog `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse service catalog `{path}`: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid service catalog: {0}")]
    Validation(String),
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    catalog_version: u64,
    services: Vec<RawService>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawRuntimeMode {
    Docker,
    Compose,
}

#[derive(Debug, Deserialize)]
struct RawService {
    name: String,
    service_revision: String,
    platform: String,
    runtime_mode: RawRuntimeMode,
    docker: Option<RawDockerRuntime>,
    compose: Option<RawComposeRuntime>,
}

#[derive(Debug, Deserialize)]
struct RawDockerRuntime {
    container_name: String,
    image_reference: String,
}

#[derive(Debug, Deserialize)]
struct RawComposeRuntime {
    project: String,
    file: std::path::PathBuf,
    service: String,
}

/// Loads and validates a `services.toml` file into the domain model.
pub fn load_service_catalog(path: impl AsRef<Path>) -> Result<ServiceCatalog, ConfigError> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let catalog = toml::from_str::<RawCatalog>(&raw).map_err(|source| ConfigError::Parse {
        path: path.display().to_string(),
        source,
    })?;

    validate_catalog(catalog)
}

fn validate_catalog(raw: RawCatalog) -> Result<ServiceCatalog, ConfigError> {
    if raw.catalog_version != SUPPORTED_SERVICE_CATALOG_VERSION {
        return Err(ConfigError::Validation(format!(
            "unsupported catalog_version `{}`; expected `{SUPPORTED_SERVICE_CATALOG_VERSION}`",
            raw.catalog_version
        )));
    }

    let mut seen_names = BTreeSet::new();
    let mut services = Vec::with_capacity(raw.services.len());

    for service in raw.services {
        let name = require_non_empty("service.name", service.name)?;
        let service_revision = require_non_empty(
            &format!("service `{name}` service_revision"),
            service.service_revision,
        )?;
        let platform = require_non_empty(&format!("service `{name}` platform"), service.platform)?;

        if !seen_names.insert(name.clone()) {
            return Err(ConfigError::Validation(format!(
                "duplicate service name `{name}` in catalog"
            )));
        }

        let runtime = match service.runtime_mode {
            RawRuntimeMode::Docker => {
                let docker = service.docker.ok_or_else(|| {
                    ConfigError::Validation(format!(
                        "service `{name}` uses runtime_mode `docker` but is missing [services.docker]"
                    ))
                })?;

                if service.compose.is_some() {
                    return Err(ConfigError::Validation(format!(
                        "service `{name}` mixes docker and compose runtime blocks"
                    )));
                }

                RuntimeMode::Docker(DockerRuntime {
                    container_name: require_non_empty(
                        &format!("service `{name}` docker.container_name"),
                        docker.container_name,
                    )?,
                    image_reference: require_non_empty(
                        &format!("service `{name}` docker.image_reference"),
                        docker.image_reference,
                    )?,
                })
            }
            RawRuntimeMode::Compose => {
                let compose = service.compose.ok_or_else(|| {
                    ConfigError::Validation(format!(
                        "service `{name}` uses runtime_mode `compose` but is missing [services.compose]"
                    ))
                })?;

                if service.docker.is_some() {
                    return Err(ConfigError::Validation(format!(
                        "service `{name}` mixes docker and compose runtime blocks"
                    )));
                }

                let compose_file = compose.file;
                if compose_file.as_os_str().is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "service `{name}` compose.file cannot be empty"
                    )));
                }

                RuntimeMode::Compose(ComposeRuntime {
                    project: require_non_empty(
                        &format!("service `{name}` compose.project"),
                        compose.project,
                    )?,
                    file: compose_file,
                    service: require_non_empty(
                        &format!("service `{name}` compose.service"),
                        compose.service,
                    )?,
                })
            }
        };

        services.push(ManagedService {
            name,
            service_revision,
            platform,
            runtime,
        });
    }

    Ok(ServiceCatalog {
        catalog_version: raw.catalog_version,
        services,
    })
}

fn require_non_empty(field: &str, value: String) -> Result<String, ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Validation(format!("{field} cannot be empty")));
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::load_service_catalog;

    #[test]
    fn rejects_duplicate_service_names() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let path = temp_dir.path().join("services.toml");

        std::fs::write(
            &path,
            r#"
catalog_version = 1

[[services]]
name = "frontend"
service_revision = "frontend-v1"
platform = "linux/amd64"
runtime_mode = "docker"

[services.docker]
container_name = "frontend"
image_reference = "example/frontend:current"

[[services]]
name = "frontend"
service_revision = "frontend-v2"
platform = "linux/amd64"
runtime_mode = "docker"

[services.docker]
container_name = "frontend-v2"
image_reference = "example/frontend:v2"
"#,
        )
        .expect("write catalog");

        let error = load_service_catalog(&path).expect_err("catalog should fail");
        assert!(
            error.to_string().contains("duplicate service name"),
            "unexpected error: {error}"
        );
    }
}
