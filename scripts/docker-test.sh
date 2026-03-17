#!/usr/bin/env bash
set -euo pipefail

# One-shot local smoke test for the full package load flow.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

service_name="frontend"
services_file="tests/fixtures/services.valid.toml"
manifest_file="tests/fixtures/manifests/valid-frontend.toml"
current_base_image="alpine:3.19"
target_base_image="alpine:3.20"
current_tag="example/frontend:current"
target_tag="example/frontend:1.2.3"
keep_temp=0
keep_container=0
tmpdir=""

usage() {
    cat <<'EOF'
Usage: ./scripts/docker-test.sh [--keep-temp] [--keep-container]

Runs a local end-to-end smoke test for `caisson package load`.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --keep-temp)
            keep_temp=1
            shift
            ;;
        --keep-container)
            keep_container=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

need_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 1
    fi
}

cleanup() {
    local exit_code=$?

    if [[ $keep_container -eq 0 ]]; then
        docker rm -f "$service_name" >/dev/null 2>&1 || true
    else
        echo "kept test container: $service_name"
    fi

    if [[ -n "$tmpdir" ]]; then
        if [[ $keep_temp -eq 1 || $exit_code -ne 0 ]]; then
            echo "kept temp directory: $tmpdir"
        else
            rm -rf "$tmpdir"
        fi
    fi
}

trap cleanup EXIT

need_command cargo
need_command docker
need_command tar
need_command mktemp
need_command cp

tmpdir="$(mktemp -d)"
state_dir="$tmpdir/state"
package_path="$tmpdir/frontend.edgepkg"

echo "==> pulling lightweight images"
docker pull "$current_base_image"
docker pull "$target_base_image"

echo "==> tagging local test images"
docker tag "$current_base_image" "$current_tag"
docker tag "$target_base_image" "$target_tag"

echo "==> building test package in $tmpdir"
docker save "$target_tag" -o "$tmpdir/image.tar"
cp "$manifest_file" "$tmpdir/manifest.toml"
tar -cf "$package_path" -C "$tmpdir" manifest.toml image.tar

echo "==> starting managed test container"
docker rm -f "$service_name" >/dev/null 2>&1 || true
docker run -d --name "$service_name" "$current_tag" sh -c 'while true; do sleep 3600; done'

echo "==> running package load"
cargo run -- package load "$package_path" \
    --services "$services_file" \
    --state-dir "$state_dir" \
    --yes

echo
echo "==> service status"
cargo run -- service show "$service_name" \
    --services "$services_file" \
    --state-dir "$state_dir"

echo
echo "==> update history"
cargo run -- history list \
    --state-dir "$state_dir"
