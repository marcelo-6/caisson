# Caisson

`Caisson` is an offline Docker service updater for edge devices.

## Status

> [!NOTE]
> This project is in early alpha and not ready for production use.

The goal of `Caisson` is to provide a update workflow for predefined Docker services on air-gapped or offline devices. Operators should only need to upload a package apply the update. The backend handles validation, image import, service restart, health checks, rollback, and local audit history.

## Why the name

A `caisson` is a controlled chamber used to move work safely through difficult or isolated environments. That is the idea behind this project: a controlled path for getting updates onto an edge device and applying them safely.

## What the project is about

`Caisson` is focused on:

- accept an offline update package
- validate it
- load the Docker image
- update a predefined service
- run health checks
- roll back automatically if the update fails
- keep a local history of what happened

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

More advanced package security can come later such as [encrypion and signatures](https://docs.securosys.com/docker_encryption/Concepts/).

But next release would still focus on a method to update the `updater` service too
