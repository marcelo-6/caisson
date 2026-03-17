# Operator CLI Guide

## Commands

### Inspect known services

```bash
cargo run -- service list --services path/to/services.toml --state-dir .caisson-state
cargo run -- service show frontend --services path/to/services.toml --state-dir .caisson-state
```

### Validate a package

```bash
cargo run -- package validate path/to/update.edgepkg --services path/to/services.toml --state-dir .caisson-state
```

This is read-only with respect to service updates. It validates the package and records the result locally.

### Load a package

```bash
cargo run -- package load path/to/update.edgepkg --services path/to/services.toml --state-dir .caisson-state
```

Add `--yes` to skip the confirmation prompt:

```bash
cargo run -- package load path/to/update.edgepkg --yes --services path/to/services.toml --state-dir .caisson-state
```

This flow validates the package, imports the image into Docker, applies the update to the target service, runs health checks, and rolls back automatically if the new version does not stay healthy.

### Read local history

```bash
cargo run -- history list --state-dir .caisson-state
cargo run -- history show <update-id> --state-dir .caisson-state
```

### Remove leftover package artifacts

```bash
cargo run -- package cleanup --state-dir .caisson-state
```

This only removes temporary package-workspace artifacts under the local state directory. It does not remove audit history, update records, or service state.

## Manual smoke test

```bash
./scripts/docker-test.sh
```

## Common operator errors

### Validation accepted but update failed

That usually means the package itself was fine, but the managed runtime state was not ready for the update. Check:

* the configured managed service exists
* Docker is reachable
* the service can stay running after replacement
* the history view for the recorded attempt
