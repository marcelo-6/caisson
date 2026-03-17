# Get version from Cargo.toml/Cargo.lock
#
# Alternative command:
# `cargo metadata --format-version=1 | jq '.packages[]|select(.name=="rust-template").version'`
version := `cargo pkgid | sed -rn s'/^.*#(.*)$/\1/p'`

# coverage threshold to fail (CI)
coverage_threshold := "60"

# semver tag pattern
semver_tag_pattern := "^v?[0-9]+\\.[0-9]+\\.[0-9]+$"

# show available commands
[group('project-agnostic')]
default:
    @just --list

# evaluate and print all just variables
[group('project-agnostic')]
just-vars:
    @just --evaluate

# print system information such as OS and architecture
[group('project-agnostic')]
system-info:
    @echo "architecture: {{arch()}}"
    @echo "os: {{os()}}"
    @echo "os family: {{os_family()}}"

# lint the sources
[group('development')]
lint:
    cargo fmt --all --check
    cargo clippy -- --deny warnings

# build the program
[group('development')]
build:
    cargo build

# analyze the current package and report errors, but don't build object files (faster than 'build')
[group('development')]
check:
    cargo check

# remove generated artifacts
[group('development')]
clean:
    cargo clean

# show test coverage (requires https://lib.rs/crates/cargo-llvm-cov)
[group('development')]
coverage threshold=coverage_threshold:
    cargo llvm-cov --fail-under-lines {{threshold}} --show-missing-lines --quiet
alias cov := coverage

# run ci workflow (lint, check, test, cov) (requires https://lib.rs/crates/cargo-llvm-cov)
[group('ci')]
ci: lint check
    cargo llvm-cov --all-features --workspace --lcov --output-path lcov.info --fail-under-lines {{coverage_threshold}} --quiet

# generate the full changelog into CHANGELOG.md.
[group('cd')]
changelog:
    git-cliff --config cliff.toml --tag-pattern '{{semver_tag_pattern}}' --output CHANGELOG.md

# dry run changelog generation.
[group('development')]
changelog-dry-run:
    next="$(git-cliff --config cliff.toml --bumped-version --unreleased --tag-pattern '{{semver_tag_pattern}}')"; \
    echo "Project version {{version}} -> next ${next}"
    git-cliff --config cliff.toml --unreleased --tag "${next}" --tag-pattern '{{semver_tag_pattern}}'

# dry run changelog generation (offline).
[group('development')]
changelog-dry-run-offline:
    git-cliff --config cliff.toml --offline

# show dependencies of this project
[group('development')]
dependencies:
    cargo tree --depth 1

# generate the documentation of this project
[group('development')]
docs:
    cargo doc --open

# build and install the binary locally
[group('development')]
install: build test
    cargo install --path .

# show version of this project
[group('development')]
version:
    @echo "Project {{version}}"
    @rustc --version
    @cargo --version

# run tests
[group('development')]
test: lint
    cargo test

# check, test, lint
[group('development')]
pre-release: check test lint

# dry runs the publish crate
[group('development')]
publish-dry-run: pre-release
    cargo publish --dry-run

# build release executable
[group('production')]
release: pre-release
    cargo build --release

# publish crate
[group('production')]
publish: pre-release
    cargo publish

# publish crate in CD (no local pre-release checks, assumes CI already passed).
[group('cd')]
publish-release:
    @printf '\033[1;34m[info]\033[0m publishing crate to crates.io...\n'
    cargo publish

# build and run
[group('production')]
run:
    cargo run

# build a lightweight demo package for local validation/load tests
[group('development')]
demo-package out_dir="dist/demo":
    bash scripts/make-demo-edgepkg.sh "{{out_dir}}"

# run the maintained local Docker smoke test
[group('development')]
smoke-test:
    bash scripts/docker-test.sh

# print the full release plan
[group('cd')]
release-plan:
    @printf '\033[1;34m[info]\033[0m Recommended release flow:\n'; \
    printf '  \033[1;33m1)\033[0m just ci\n'; \
    printf '  \033[1;33m2)\033[0m merge normal work into main\n'; \
    printf '  \033[1;33m3)\033[0m let release-plz open or update the release PR\n'; \
    printf '  \033[1;33m4)\033[0m review the release PR version bump and changelog\n'; \
    printf '  \033[1;33m5)\033[0m merge the release PR\n'; \
    printf '  \033[1;33m6)\033[0m monitor release-plz publish, tag creation, and GitHub release\n'
