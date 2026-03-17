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
cargo run -- service list --services path/to/services.toml --state-dir .caisson-state
```

That command shows the predefined services from `services.toml` together with the locally known image state and the last recorded update result.

Validate a package without changing anything:

```bash
cargo run -- package validate path/to/update.edgepkg --services path/to/services.toml --state-dir .caisson-state
```

That command:

- stages the package locally
- validates the `.edgepkg` tar structure
- parses `manifest.toml`
- checks target service compatibility against `services.toml`
- persists validation records and audit events under the chosen state directory

Load a package:

```bash
cargo run -- package load path/to/update.edgepkg --services path/to/services.toml --state-dir .caisson-state
```

That command validates the package first, asks for confirmation, imports the staged `image.tar` into Docker, applies the update to the target service, runs health checks, and rolls back automatically if the update does not stay healthy.

If you want the same flow without the confirmation prompt:

```bash
cargo run -- package load path/to/update.edgepkg --yes --services path/to/services.toml --state-dir .caisson-state
```

To inspect local update history after a run:

```bash
cargo run -- history list --state-dir .caisson-state
cargo run -- history show <update-id> --state-dir .caisson-state
```

Quick way to build a local package for manual testing is:

```bash
tmpdir=$(mktemp -d)
docker pull alpine:3.19
docker pull alpine:3.20
docker tag alpine:3.19 example/frontend:current
docker tag alpine:3.20 example/frontend:1.2.3
docker save example/frontend:1.2.3 -o "$tmpdir/image.tar"
cp tests/fixtures/manifests/valid-frontend.toml "$tmpdir/manifest.toml"
tar -cf "$tmpdir/frontend.edgepkg" -C "$tmpdir" manifest.toml image.tar
```

If you want to test the full `package load` path, start the managed service first so there is something to replace:

```bash
docker rm -f frontend 2>/dev/null || true
docker run -d --name frontend example/frontend:current sh -c 'sleep infinity'
```

Then run:

```bash
cargo run -- package load "$tmpdir/frontend.edgepkg" \
  --services tests/fixtures/services.valid.toml \
  --state-dir "$tmpdir/state"
```

`hello-world` is still fine for the earlier image-import smoke test, but it exits immediately, so it is not a good fit for the full apply + health check.

For a one-shot local smoke test, run:

```bash
./scripts/docker-test.sh
```

Test fixtures live under `tests/fixtures/`.
