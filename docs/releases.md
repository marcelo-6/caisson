# Release Plan

This is the release packaging plan.

Releases use `release-plz`.

## Release workflow

The intended release flow is:

1. merge normal work into `main`
2. let the `release-plz` workflow open or update the release PR
3. review the release PR version bump and changelog changes
4. merge the release PR
5. let `release-plz` publish the crate, create the tag, and create the GitHub release

Repository setup notes:

* `CARGO_REGISTRY_TOKEN` is required for crates.io publishing
* `RELEASE_PLZ_TOKEN` is recommended for the GitHub side if you want release-plz-created PRs and releases to trigger downstream workflows normally
* if `RELEASE_PLZ_TOKEN` is not configured, the workflow falls back to `GITHUB_TOKEN`

## What the release produces

For now, the actual release output is whatever `release-plz` manages directly:

* the published crate on crates.io
* the git tag
* the GitHub release

## GitHub/CD expectations

The current CD workflow is expected to:

* open or update the release PR on pushes to `main`
* publish the crate after the release PR is merged
* create the git tag and GitHub release through `release-plz`
