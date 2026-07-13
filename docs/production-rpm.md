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

The Seiza star catalog is not packaged. It is much larger and changes on a
different cadence than the server; install it on durable storage such as
`/srv/seiza/catalog/`, point `SEIZA_STAR_DATA` at it, and make it readable by
the systemd service. This also keeps an application upgrade from silently
changing solver results.

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
Verify that the main package installs on a clean target container with:

```bash
make rpm-verify-al2023
make rpm-verify-f44
```

GitHub Actions runs both builds and clean-install checks for pull requests,
pushes to `main`, version tags, and manual dispatches. Its `x86_64` RPMs are
retained as workflow artifacts for 14 days.

## Install and configure

Copy the matching RPM to the host and install it with DNF:

```bash
sudo dnf install ./seiza-server-*.rpm
sudoedit /etc/seiza-server/seiza-server.env
sudo systemctl enable --now seiza-server
sudo systemctl status seiza-server
```

Set `SEIZA_STAR_DATA` before starting the service. The default configuration
uses a SQLite database and local uploaded-object storage under
`/var/lib/seiza-server/`; systemd creates and owns that state directory for the
restricted `seiza-server` system account. Keep that path on persistent local
storage. The environment file is packaged as `root:seiza-server` mode `0640`,
so it is the correct place for a worker token on a single host.

For a multi-host deployment, move originals to S3 and choose DynamoDB or a
PostgreSQL SQLx URL. The installed binary includes all of those adapters; only
the environment file changes. Keep the worker token in the protected local
environment file or in your secret manager; never commit it to the repository.

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

The Cargo manifest is the version source of truth for the crate, git tag,
GitHub release, and RPMs. After merging a version bump to `main`, dispatch the
`Release` workflow with that exact version (without a `v` prefix). It performs
a crates.io dry run, rebuilds and clean-installs both x86_64 RPM targets,
publishes the crate, creates the matching `v<version>` tag and GitHub release,
and attaches all binary, debuginfo, and source RPMs.

The first `seiza-server` version must be bootstrapped manually because
crates.io only allows trusted publishers after a crate exists:

```bash
git switch main
git pull --ff-only
cargo publish --locked
```

Then configure the crate's crates.io trusted publisher with repository owner
`theatrus`, repository `seiza-server`, workflow `release.yml`, and environment
`release`. Dispatch the release workflow for the already-published first
version; it detects the existing crate and still creates the synchronized tag,
GitHub release, and RPM assets. Later versions publish through short-lived OIDC
credentials and require no stored crates.io token.
