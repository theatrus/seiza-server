#!/usr/bin/env bash
set -euo pipefail

target=${1:?usage: build-in-container.sh <al2023|f44>}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

case "${target}" in
  al2023)
    base_image=${SEIZA_AL2023_IMAGE:-public.ecr.aws/amazonlinux/amazonlinux:2023}
    ;;
  f44)
    base_image=${SEIZA_F44_IMAGE:-registry.fedoraproject.org/fedora:44}
    ;;
  *)
    echo "unknown RPM target: ${target}; use al2023 or f44" >&2
    exit 2
    ;;
esac

if [[ -n ${CONTAINER_ENGINE:-} ]]; then
  engine=${CONTAINER_ENGINE}
elif command -v podman >/dev/null 2>&1; then
  engine=podman
elif command -v docker >/dev/null 2>&1; then
  engine=docker
else
  echo "install podman or docker, or set CONTAINER_ENGINE" >&2
  exit 1
fi

tag="seiza-server-rpm-builder:${target}"
if [[ ${SEIZA_SKIP_CONTAINER_BUILD:-0} == 1 ]]; then
  if ! "${engine}" image inspect "${tag}" >/dev/null 2>&1; then
    echo "cached builder image ${tag} is not loaded" >&2
    exit 1
  fi
else
  "${engine}" build \
    --build-arg "BASE=${base_image}" \
    --build-arg "TARGET=${target}" \
    --tag "${tag}" \
    --file "${repo_root}/packaging/rpm/Containerfile" \
    "${repo_root}"
fi

# `:Z` is useful on enforcing-SELinux hosts. Leave it configurable so Docker
# Desktop and non-SELinux Podman installations work without special casing.
volume_suffix=${SEIZA_CONTAINER_VOLUME_SUFFIX:-}
cache_dir="${repo_root}/.rpm-cache/${target}"
mkdir -p "${cache_dir}/cargo" "${cache_dir}/npm" "${cache_dir}/target"

"${engine}" run --rm \
  --user "$(id -u):$(id -g)" \
  --env HOME=/tmp \
  --env CARGO_HOME=/tmp/cargo \
  --env CARGO_TARGET_DIR=/tmp/target \
  --env npm_config_cache=/tmp/npm \
  --env RUSTUP_HOME=/opt/rustup \
  --volume "${repo_root}:/src${volume_suffix}" \
  --volume "${cache_dir}/cargo:/tmp/cargo${volume_suffix}" \
  --volume "${cache_dir}/npm:/tmp/npm${volume_suffix}" \
  --volume "${cache_dir}/target:/tmp/target${volume_suffix}" \
  --workdir /src \
  "${tag}" \
  /src/packaging/rpm/build-rpm.sh "${target}"
