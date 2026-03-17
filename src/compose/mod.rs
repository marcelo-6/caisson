//! Compose integration for predefined services.
//!
//! Validate that the configured service exists,
//! figure out its current image when we need a rollback target, and hand Docker
//! Compose a one-service image override.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use tempfile::NamedTempFile;
use thiserror::Error;

use crate::domain::ComposeRuntime;

/// Compose operations the update workflow depends on.
pub trait ComposeClient: std::fmt::Debug {
    fn read_service_image(&self, runtime: &ComposeRuntime) -> Result<String, ComposeError>;
    fn apply_service_image(
        &self,
        runtime: &ComposeRuntime,
        image_reference: &str,
    ) -> Result<(), ComposeError>;
}

/// Shell-backed Compose client for local `docker compose` usage.
#[derive(Debug, Default, Clone, Copy)]
pub struct ShellComposeClient;

impl ComposeClient for ShellComposeClient {
    fn read_service_image(&self, runtime: &ComposeRuntime) -> Result<String, ComposeError> {
        let raw = read_compose_source(&runtime.file)?;
        find_service_image(&raw, &runtime.service)
    }

    fn apply_service_image(
        &self,
        runtime: &ComposeRuntime,
        image_reference: &str,
    ) -> Result<(), ComposeError> {
        let _ = self.read_service_image(runtime)?;

        let override_file = write_override_file(runtime, image_reference)?;
        run_compose_up(runtime, override_file.path())
    }
}

fn read_compose_source(path: &Path) -> Result<String, ComposeError> {
    fs::read_to_string(path).map_err(|source| ComposeError::Read {
        path: path.display().to_string(),
        source,
    })
}

fn find_service_image(raw: &str, service_name: &str) -> Result<String, ComposeError> {
    let mut in_services = false;
    let mut current_service: Option<&str> = None;

    for line in raw.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if !in_services {
            if trimmed == "services:" {
                in_services = true;
            }
            continue;
        }

        let indent = line
            .chars()
            .take_while(|character| *character == ' ')
            .count();
        if indent == 0 {
            break;
        }

        if indent == 2 && trimmed.ends_with(':') {
            current_service = Some(trimmed.trim_end_matches(':'));
            continue;
        }

        if current_service == Some(service_name) && indent >= 4 && trimmed.starts_with("image:") {
            let image = trimmed.trim_start_matches("image:").trim();
            if image.is_empty() {
                return Err(ComposeError::Validation(format!(
                    "compose service `{service_name}` is missing an image value"
                )));
            }

            return Ok(image.trim_matches('"').trim_matches('\'').to_string());
        }
    }

    if in_services {
        return Err(ComposeError::Validation(format!(
            "compose service `{service_name}` is missing an `image` field or does not exist"
        )));
    }

    Err(ComposeError::Validation(
        "compose file is missing a `services:` section".into(),
    ))
}

fn write_override_file(
    runtime: &ComposeRuntime,
    image_reference: &str,
) -> Result<NamedTempFile, ComposeError> {
    let escaped_image = image_reference.replace('\'', "''");
    let serialized = format!(
        "services:\n  {}:\n    image: '{}'\n",
        runtime.service, escaped_image
    );

    let mut file = NamedTempFile::new().map_err(ComposeError::CommandIo)?;
    file.write_all(serialized.as_bytes())
        .map_err(ComposeError::CommandIo)?;
    file.flush().map_err(ComposeError::CommandIo)?;

    Ok(file)
}

fn run_compose_up(runtime: &ComposeRuntime, override_path: &Path) -> Result<(), ComposeError> {
    let mut command = Command::new("docker");
    command
        .arg("compose")
        .arg("-p")
        .arg(&runtime.project)
        .arg("-f")
        .arg(&runtime.file)
        .arg("-f")
        .arg(override_path)
        .arg("up")
        .arg("-d")
        .arg(&runtime.service);

    if let Some(parent) = runtime.file.parent() {
        command.current_dir(parent);
    }

    let output = command.output().map_err(ComposeError::CommandIo)?;

    if output.status.success() {
        return Ok(());
    }

    Err(ComposeError::CommandFailed {
        command: format!(
            "docker compose -p {} -f {} -f {} up -d {}",
            runtime.project,
            runtime.file.display(),
            override_path.display(),
            runtime.service
        ),
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Errors returned by Compose.
#[derive(Debug, Error)]
pub enum ComposeError {
    #[error("failed to read compose file `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid compose configuration: {0}")]
    Validation(String),
    #[error("failed to run docker compose: {0}")]
    CommandIo(std::io::Error),
    #[error("docker compose command failed `{command}` (exit: {exit_code:?}): {stderr}")]
    CommandFailed {
        command: String,
        exit_code: Option<i32>,
        stderr: String,
    },
}
