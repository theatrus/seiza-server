#!/usr/bin/env bash
set -euo pipefail

target=${1:?usage: build-rpm.sh <al2023|f44>}
repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
version=$(awk -F '"' '/^version = / { print $2; exit }' "${repo_root}/Cargo.toml")
release=${SEIZA_RPM_RELEASE:-1}
# Keep rpmbuild's mutable BUILD tree on the container filesystem. RPM 6's
# cleanup pass recursively changes permissions, which is not supported by all
# macOS container bind-mount implementations. Only finished artifacts leave
# the container.
topdir="${SEIZA_RPM_TOPDIR:-/tmp/seiza-rpmbuild/${target}}"
source_dir="${topdir}/SOURCES"
artifact_dir="${repo_root}/dist/rpm/${target}"

if [[ -z ${version} ]]; then
  echo "could not read package version from Cargo.toml" >&2
  exit 1
fi

rm -rf "${topdir}" "${artifact_dir}"
mkdir -p "${source_dir}" "${artifact_dir}"

# Build from the checked-out source set, including uncommitted packaging edits,
# while honoring .gitignore for target/, data/, node_modules/, and artifacts.
git -c "safe.directory=${repo_root}" -C "${repo_root}" \
    ls-files -co --exclude-standard -z \
  | tar --create --gzip \
      --file "${source_dir}/seiza-server-${version}.tar.gz" \
      --directory "${repo_root}" \
      --null --files-from=- \
      --transform "s,^,seiza-server-${version}/,"

# The builder supplies current Rust through rustup rather than an RPM from the
# target distro, so satisfy the spec's BuildRequires through the image and let
# rpmbuild focus on producing the binary and source packages.
rpmbuild -ba --nodeps \
  --define "_topdir ${topdir}" \
  --define "package_version ${version}" \
  --define "package_release ${release}" \
  "${repo_root}/packaging/rpm/seiza-server.spec"

find "${topdir}/RPMS" "${topdir}/SRPMS" -type f -name '*.rpm' -exec cp --target-directory "${artifact_dir}" {} +
printf 'RPM artifacts written to %s\n' "${artifact_dir}"
