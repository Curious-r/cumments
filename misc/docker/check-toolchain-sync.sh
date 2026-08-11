#!/usr/bin/env bash
set -euo pipefail

# Ensure the pinned Rust toolchain stays in sync between the canonical
# rust-toolchain.toml and the Docker base image (the only place a version
# still has to be written down explicitly).

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

toolchain_file="${1:-$repo_root/rust-toolchain.toml}"
dockerfile="${2:-$repo_root/misc/docker/Dockerfile}"

channel="$(sed -n 's/^channel = "\(.*\)"/\1/p' "$toolchain_file" | head -n1)"
docker_version="$(sed -n 's/^ARG RUST_VERSION=\(.*\)/\1/p' "$dockerfile" | head -n1)"

if [[ -z "$channel" || -z "$docker_version" ]]; then
    echo "check-toolchain-sync: could not read channel from $toolchain_file or RUST_VERSION from $dockerfile" >&2
    exit 1
fi

if [[ "$channel" != "$docker_version" ]]; then
    echo "check-toolchain-sync: rust-toolchain.toml channel '$channel' != Dockerfile RUST_VERSION '$docker_version'" >&2
    exit 1
fi

echo "check-toolchain-sync: ok ($channel)"
