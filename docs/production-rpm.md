# Production RPM deployment

Seiza Server ships as two native RPM targets:

| Target | Builder base | Output | Intended host |
| --- | --- | --- | --- |
| `al2023` | `public.ecr.aws/amazonlinux/amazonlinux:2023` | `dist/rpm/al2023/` | Amazon Linux 2023 |
| `f44` | `registry.fedoraproject.org/fedora:44` | `dist/rpm/f44/` | Fedora 44 |

They are intentionally separate artifacts. An RPM embeds distribution-specific
runtime requirements, and the Rust binary must be linked against the target
distribution's glibc. Build on the same CPU architecture as the eventual host.
The GitHub Actions workflow uses an `x86_64` runner; running the command on an
ARM host produces an `aarch64` RPM. Use a target-native builder or runner for
Graviton/ARM releases.

The package combines everything needed to run the service:

- `/usr/bin/seiza-server`, built with the `aws` feature for S3, SQS, and
  DynamoDB support;
- the compiled React UI at `/usr/libexec/seiza-server/frontend`;
- `seiza-server.service`, bound to `127.0.0.1:8080` by default;
- `/etc/seiza-server/seiza-server.env`, preserved across upgrades; and
- an nginx configuration example at
  `/usr/share/doc/seiza-server/nginx.conf.example`.

The Seiza star and optional object catalogs are not packaged. They are larger
and change on a different cadence than the server; install them on durable
storage such as `/srv/seiza/catalog/` and make them readable by the systemd
service. The packaged `SEIZA_CATALOG_DIR` prefers `stars-deep-gaia17.bin` and
discovers its matching `blind-gaia16.idx`, with `stars-gaia.bin` and the lite
catalog as fallbacks. It also discovers `objects.bin`,
`stars-lite-tycho2.ids.bin`, `transients.bin`, and `minor-bodies.bin` there
automatically; explicit per-catalog variables
override discovery.
This also keeps an application upgrade from silently changing solver results.

## Build

Podman is preferred; Docker also works. The build uses a target-native DNF
container and installs a current Rust toolchain inside it, so a macOS or other
non-RPM development host does not contaminate the resulting artifact.

```bash
make rpm-al2023
make rpm-f44
```

Set `CONTAINER_ENGINE=docker` to override the engine, or set
`SEIZA_CONTAINER_VOLUME_SUFFIX=:Z` on SELinux-enforcing Podman hosts. For a
reproducible release, override the default moving base image tag, for example:

```bash
SEIZA_AL2023_IMAGE=public.ecr.aws/amazonlinux/amazonlinux:2023.12.20260611.0 \
  make rpm-al2023
```

The package build runs the AWS-enabled Rust test suite and builds the frontend.
It produces binary, debuginfo, and source RPMs in the target artifact directory.
Cargo downloads, npm downloads, and compiled Rust targets are reused from
`.rpm-cache/<target>/` on subsequent local builds. GitHub Actions persists the
same directories between runs and separately caches the target-native builder
image, including its DNF packages and Rust toolchain.
Verify that the main package installs on a clean target container with:

```bash
make rpm-verify-al2023
make rpm-verify-f44
```

GitHub Actions runs both builds and clean-install checks for pull requests,
pushes to `main`, version tags, and manual dispatches. Its `x86_64` RPMs are
retained as workflow artifacts for 14 days on pull requests and 30 days on
`main`. Download them from the workflow run in GitHub, or with:

```bash
gh run download <run-id> --repo theatrus/seiza-server
```

Release workflow artifacts are also attached to the corresponding GitHub
release for durable retrieval.

## Install and configure

Copy the matching RPM to the host and install it with DNF:

```bash
sudo dnf install ./seiza-server-*.rpm
sudoedit /etc/seiza-server/seiza-server.env
sudo systemctl enable --now seiza-server
sudo systemctl status seiza-server
```

Download the prebuilt datasets with Seiza CLI 0.4.1 or newer before starting the
service:

```bash
sudo install -d -o root -g seiza-server -m 0750 /srv/seiza/catalog
sudo seiza download-data prebuilt --output /srv/seiza/catalog
sudo chgrp -R seiza-server /srv/seiza/catalog
sudo chmod -R g+rX /srv/seiza/catalog
```

The hosted prebuilt set supplies the star, stellar-identifier,
deep-sky/named-star, and transient catalogs. The v3 object files are
memory-mapped and contain their spatial and
name indices; downloading in place is safe because the CLI verifies a temporary
file and atomically renames it into place. The server notices that replacement
and reloads it without a restart. A minor-body catalog is built separately from
current orbital elements. The default configuration uses a SQLite database and local
uploaded-object storage
under `/var/lib/seiza-server/`; systemd creates and owns that state directory
for the restricted `seiza-server` system account. Keep that path on persistent
local storage. The environment file is packaged as `root:seiza-server` mode
`0640`, so it is the correct place for a worker token on a single host.

For a multi-host deployment, move originals to S3 and choose DynamoDB or a
PostgreSQL SQLx URL. The installed binary includes all of those adapters; only
the environment file changes. Keep the worker token in the protected local
environment file or in your secret manager; never commit it to the repository.

Originals are swept after 24 hours by default; add a matching S3 lifecycle rule
as defense-in-depth. Job records and WCS results do not expire with the image.

```ini
SEIZA_STORAGE_BACKEND=s3
SEIZA_S3_BUCKET=seiza-solves
SEIZA_JOB_BACKEND=dynamodb
SEIZA_DYNAMODB_TABLE=seiza-jobs
SEIZA_QUEUE_TRANSPORT=sqs
SEIZA_SQS_QUEUE_URL=https://sqs.us-west-2.amazonaws.com/123456789012/seiza-solves
SEIZA_EMBEDDED_WORKERS=false
SEIZA_WORKER_TOKEN=replace-with-a-secret-from-your-secret-manager
```

## nginx

Copy and tailor the installed nginx example, including its TLS certificate and
real hostname, then validate and reload nginx:

```bash
sudo cp /usr/share/doc/seiza-server/nginx.conf.example /etc/nginx/conf.d/seiza-server.conf
sudoedit /etc/nginx/conf.d/seiza-server.conf
sudo nginx -t
sudo systemctl reload nginx
```

The service deliberately listens only on loopback. nginx terminates TLS and
supplies the forwarding headers used by Seiza's client rate limiter. Restrict
the authenticated internal worker paths at the network layer if remote workers
do not need to reach them through the public proxy.

## Release policy

Publish the RPMs as release assets or from a signed repository, not as an
unversioned mutable download. Sign both the RPMs and repository metadata for
production use. The source RPM is emitted alongside the binary RPM so the
release can be rebuilt and audited later.

The Cargo manifest is the version source of truth for the server's git tag,
GitHub release, and RPMs. After merging a version bump to `main`, dispatch the
`Release` workflow with that exact version (without a `v` prefix). It rebuilds
and clean-installs both x86_64 RPM targets, creates the matching `v<version>`
tag and GitHub release, and attaches all binary, debuginfo, and source RPMs.

`seiza-server` is an application and is intentionally marked `publish = false`;
it is not published to crates.io. The reusable `seiza` library is versioned and
published independently from the `theatrus/seiza` repository.
