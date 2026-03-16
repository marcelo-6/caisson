<!-- markdownlint-disable MD033 -->
<!-- markdownlint-disable MD041 -->
<div align="center">

<h4>Offline Docker service updater for airgapped edge devices.</h4>

<a href="https://github.com/marcelo-6/caisson/relseases"><img src="https://img.shields.io/github/v/release/marcelo-6/caisson?logo=github" alt="GitHub Release"></a>
<a href="https://crates.io/crates/caisson/"><img src="https://img.shields.io/crates/v/caisson?logo=Rust" alt="Crate Release"></a>
<a href="https://codecov.io/gh/marcelo-6/caisson"><img src="https://codecov.io/gh/marcelo-6/caisson/graph/badge.svg?token=TPJMXTJ5ZQ&amp;logo=Codecov&amp;logoColor=white" alt="Coverage"></a>
<br>
<a href="https://github.com/marcelo-6/caisson/actions?query=workflow%3A%22CI%22"><img src="https://img.shields.io/github/actions/workflow/status/marcelo-6/caisson/ci.yml?branch=main&amp;logo=GitHub%20Actions&amp;logoColor=white&amp;label=CI" alt="Continuous Integration"></a>
<a href="https://github.com/marcelo-6/caisson/actions?query=workflow%3A%22CD%22"><img src="https://img.shields.io/github/actions/workflow/status/marcelo-6/caisson/cd.yml?logo=GitHub%20Actions&amp;logoColor=white&amp;label=CD" alt="Continuous Deployment"></a>
<a href="https://docs.rs/caisson/"><img src="https://img.shields.io/docsrs/caisson?logo=Rust&amp;logoColor=white" alt="Documentation"></a>

<img alt="Crates.io Total Downloads" src="https://img.shields.io/crates/d/caisson?logo=Rust">
<img alt="GitHub Downloads (all assets, all releases)" src="https://img.shields.io/github/downloads/marcelo-6/caisson/total?logo=GitHub">
<br>
<img alt="GitHub commits since latest release" src="https://img.shields.io/github/commits-since/marcelo-6/caisson/latest">

</div>

## Status

> [!NOTE]
> This project is in early alpha and not ready for production use.

The goal of `Caisson` is to provide an update workflow for predefined Docker services on air-gapped or offline devices. In `0.1.0`, operators should only need to point the CLI at a local package file and apply the update. The backend handles validation, image import, service restart, health checks, rollback, and local audit history.

## Why the name

A `caisson` is a controlled chamber used to move work safely through difficult or isolated environments. That is the idea behind this project: a controlled path for getting updates onto an edge device and applying them safely.

## What the project is about

`Caisson` is focused on:

- accept an offline update package
- work through a CLI-first operator flow in `0.1.0`
- validate it
- load the Docker image
- update a predefined service
- run health checks
- roll back automatically if the update fails
- keep a local history of what happened
- support predefined services that use either direct Docker control or developer-provided Docker Compose definitions

## What it is not

`Caisson` is not:

- a fleet manager
- a registry
- a cloud update system
- a general Docker dashboard

## Alternatives

[Hauler](https://docs.hauler.dev/docs/airgap-workflow) describes itself as an “Airgap Swiss Army Knife” for fetching, storing, packaging, and distributing artifacts across disconnected environments.

[Mender](https://docs.mender.io/) supports standalone deployments for devices without network connectivity, including updates triggered manually or through external storage, and it also has Docker Compose update support.

[Portainer](https://www.portainer.io/) is a broader container management UI for Docker and other environments, positioned as a toolset for building and managing containers.

`Caisson` exists because I wanted something smaller and more opinionated than those options: a local updater focused specifically on offline Docker service updates on one device, with a simple operator experience. It is not always that the human closest to the end device has the technical knowlege to safely update its services.

## Current direction

The first release is focused on getting the baseline workflow working:

- offline package intake
- validation
- Docker image import
- service update
- health checks
- rollback
- local audit/history

> [!NOTE]
> Current roadmap:
>
> - `0.1.0`: cli for offline updater
> - `0.2.0`: minimal GUI on top of the same application logic
> - `0.3.0`: self-updater support
> - `0.4.0`: encryption and signatures

## Current CLI

```bash
cargo run -- validate path/to/update.edgepkg --services path/to/services.toml --state-dir .caisson-state
```

That command:

- stages the package locally
- validates the `.edgepkg` tar structure
- parses `manifest.toml`
- checks target service compatibility against `services.toml`
- persists validation records and audit events under the chosen state directory
