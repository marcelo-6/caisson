#!/usr/bin/env bash
set -euo pipefail

# Builds a lightweight demo `.edgepkg` using the tracked frontend manifest.
# Good for `package validate` demos and for the `package load` smoke path when
# paired with the matching managed test container setup.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

output_dir="${1:-dist/demo}"
manifest_file="tests/fixtures/manifests/valid-frontend.toml"
target_base_image="alpine:3.20"
target_tag="example/frontend:1.2.3"
package_name="frontend.edgepkg"

need_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required command: $1" >&2
        exit 1
    fi
}

for cmd in docker tar cp mkdir mktemp; do
    need_command "$cmd"
done

mkdir -p "$output_dir"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "==> pulling lightweight demo image"
docker pull "$target_base_image"

echo "==> tagging demo image as $target_tag"
docker tag "$target_base_image" "$target_tag"

echo "==> building demo package"
docker save "$target_tag" -o "$tmpdir/image.tar"
cp "$manifest_file" "$tmpdir/manifest.toml"
tar -cf "$output_dir/$package_name" -C "$tmpdir" manifest.toml image.tar

echo "created demo package: $output_dir/$package_name"
