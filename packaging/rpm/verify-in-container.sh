#!/usr/bin/env bash
set -euo pipefail

target=${1:?usage: verify-in-container.sh <al2023|f44>}
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

volume_suffix=${SEIZA_CONTAINER_VOLUME_SUFFIX:-}
packages=()
while IFS= read -r package; do
  packages+=("${package}")
done < <(
  find "${repo_root}/dist/rpm/${target}" -maxdepth 1 -type f \
    \( -name '*.x86_64.rpm' -o -name '*.aarch64.rpm' \) \
    ! -name '*-debuginfo-*' \
    ! -name '*-debugsource-*'
)

if [[ ${#packages[@]} -ne 1 ]]; then
  echo "expected exactly one installable ${target} RPM, found ${#packages[@]}" >&2
  printf '%s\n' "${packages[@]}" >&2
  exit 1
fi

package_path=${packages[0]#"${repo_root}/"}
"${engine}" run --rm \
  --volume "${repo_root}:/src${volume_suffix}" \
  "${base_image}" \
  bash -c 'dnf install -y "/src/$1" && /usr/bin/seiza-server --help >/dev/null' \
  bash "${package_path}"

printf 'Verified clean installation of %s\n' "${packages[0]}"
