# Seiza Server

Seiza Server is a queued web service for plate solving. It uses the published
[`seiza`](https://crates.io/crates/seiza) and
[`seiza-fits`](https://crates.io/crates/seiza-fits) Rust crates directly—not a
CLI subprocess—and includes a TypeScript/React frontend.

The job queue is durable: local deployments use SQLx with SQLite on disk, and
AWS deployments can use DynamoDB. SQLx also accepts PostgreSQL, so it is the
same relational implementation from laptop to a multi-host deployment. It
stores job state, weighted-LRU scheduling state, leases, and the notification
outbox. A queued request is accepted immediately, then a bounded solver worker
reads it later. A slow blind solve therefore never ties up the HTTP handler or
disappears on a process restart.

## What is implemented

- Native JSON API: multipart uploads, job polling, explicit WCS/quality output,
  downloadable FITS-style WCS headers, annotated SVG overlays, a 100 MB
  default body limit, structured errors, and CORS.
- Astrometry.net-compatible API subset: `POST /api/login`, `POST /api/upload`,
  `GET /api/submissions/:id`, `GET /api/jobs/:id`,
  `GET /api/jobs/:id/calibration`, and `GET /api/jobs/:id/info`.
- FITS (`.fit`, `.fits`, `.fts`), PNG, JPEG, TIFF, and WebP input. FITS files
  are decoded through `seiza-fits` and autostretched before source detection.
- Hinted solves when RA, Dec, and pixel scale are supplied; otherwise blind
  solving using Seiza's catalog index.
- Per-client token-bucket admission limiting plus a durable weighted-LRU
  priority queue. An unseen/least-recently served client goes first; higher
  future API tiers can use a larger queue weight without changing the
  scheduler. Conditional leasing makes duplicate delivery safe across workers.
- `public` and `stub-api-key` authentication modes. The latter requires a
  nonempty key/session but deliberately does not validate it against a key
  database yet.
- Separate-process workers can poll an authenticated internal API, while an
  SQS adapter can deliver jobs directly to cloud workers. Local object storage
  is the default; S3 and SQS are opt-in through the `aws` Cargo feature.
- Every solve has a durable `/solutions/:id` web page. Uploaded originals and
  derived visual previews expire after one day by default, while the job and
  its complete WCS metadata remain available.

## Quick start

Install or build the Seiza CLI, then get a catalog. The lite Tycho-2 catalog is
a small starting point; Gaia is better for narrow/deep fields.

```bash
cargo install seiza-cli
seiza download-data prebuilt --output ../seiza-data

cp .env.example .env # copy the values into your shell or dotenv runner
export SEIZA_STAR_DATA="$PWD/../seiza-data/stars-lite-tycho2.bin"

cargo run
```

In a second terminal, run the web client:

```bash
cd frontend
npm install
npm run dev
```

Open Vite's local URL (normally `http://localhost:5173`). To have Axum serve
the built UI instead, run `npm run build` and then `cargo run`; the default
`SEIZA_FRONTEND_DIR` is `frontend/dist`.

`SEIZA_STAR_DATA` is intentionally required for usable solves and is not in
this repository. The health endpoint stays available without it and reports
`"degraded"`, while queued solves fail with a clear configuration error.

## Worker processes and durable queue

By default the API process starts `SEIZA_WORKER_COUNT` embedded workers. The
default queue is an SQLx SQLite database at `SEIZA_QUEUE_DATABASE` (default
`data/jobs.sqlite3`) and preserves queued jobs after a restart. Set a
`SEIZA_SQL_DATABASE_URL` for a SQLite URL or PostgreSQL connection instead.
To separate API and CPU work, set a shared worker token and disable embedded
workers on the API process:

```bash
export SEIZA_EMBEDDED_WORKERS=false
export SEIZA_WORKER_TOKEN="$(openssl rand -hex 32)"
cargo run -- serve
```

Start one or more workers wherever the Seiza catalog is available. They do not
need access to the job-store database or local object directory: they claim a lease,
download the original, heartbeat while solving, and complete through the
authenticated internal worker API.

```bash
SEIZA_STAR_DATA=/data/stars-gaia.bin \
SEIZA_WORKER_TOKEN="$SEIZA_WORKER_TOKEN" \
cargo run -- worker --server http://api-host:8080
```

Each lease expires after `SEIZA_LEASE_SECONDS` (15 minutes by default). A
crashed worker's job is automatically requeued; a stale worker cannot fetch an
input or overwrite a retried result. The worker endpoints are disabled until
`SEIZA_WORKER_TOKEN` is configured.

## Native API

Health and queue state:

```bash
curl http://127.0.0.1:8080/api/v1/health
```

Submit a blind solve. `options` is a JSON form field, making the file upload
endpoint straightforward for browsers and API clients alike.

```bash
curl -X POST http://127.0.0.1:8080/api/v1/solves \
  -F 'file=@M31.fits' \
  -F 'options={"min_scale_arcsec_per_pixel":0.5,"max_scale_arcsec_per_pixel":15}'
```

The response is `202 Accepted` with an ID and artifact URLs. Poll it until
`status` becomes `succeeded` or `failed`:

```bash
curl http://127.0.0.1:8080/api/v1/solves/1
```

Successful jobs expose an on-demand PNG preview and annotated SVG while the
uploaded image is retained, plus a persistent FITS-compatible WCS header:

```bash
curl -o overlay.svg http://127.0.0.1:8080/api/v1/solves/1/overlay.svg
curl -OJ http://127.0.0.1:8080/api/v1/solves/1/wcs
```

The JSON solution includes the full TAN/ICRS WCS (`CTYPE`, `CUNIT`, `CRVAL`,
zero-indexed internal `CRPIX`, CD matrix, `RADESYS`, and `EQUINOX`), the four
ICRS footprint corners, and projected catalog objects when
`SEIZA_OBJECT_DATA` is configured. The downloadable `.wcs` converts `CRPIX`
to FITS' one-indexed convention.

A position hint avoids the whole-sky path:

```json
{
  "center_ra_deg": 10.6847,
  "center_dec_deg": 41.2690,
  "radius_deg": 2,
  "scale_arcsec_per_pixel": 1.24,
  "scale_tolerance": 0.2
}
```

All solver work happens after the `202`; upload handlers only validate, store,
rate-limit, and enqueue.

## Astrometry.net compatibility

The compatibility endpoints follow the Astrometry.net convention of carrying
JSON as a `request-json` form field. File submissions use multipart with a
`request-json` text part and a `file` part. The service supports the common
`scale_type` values `ul` and `ev`, and `scale_units` values `degwidth`,
`arcminwidth`, and `arcsecperpix`.

```bash
session=$(curl -sS -X POST http://127.0.0.1:8080/api/login \
  --data-urlencode 'request-json={"apikey":"development-key"}' \
  | jq -r .session)

curl -sS -X POST http://127.0.0.1:8080/api/upload \
  -F "request-json={\"session\":\"$session\",\"scale_units\":\"arcsecperpix\",\"scale_type\":\"ul\",\"scale_lower\":0.5,\"scale_upper\":15}" \
  -F 'file=@M31.fits;type=application/octet-stream'
```

This is deliberately a focused interoperability surface, not a clone of every
Astrometry.net endpoint. URL uploads are not exposed, avoiding an SSRF-capable
server fetch path. Tags, generated FITS images, durable sessions, and API-key
verification are future additions. The canonical native API already provides
annotations and a downloadable WCS header.

The endpoint shapes and multipart encoding follow the
[Astrometry.net API documentation](https://astrometry.net/doc/net/api.html).

## Configuration

Copy [.env.example](.env.example) as a reference. These environment variables
are currently supported:

| Variable | Default | Meaning |
| --- | --- | --- |
| `SEIZA_BIND_ADDR` | `127.0.0.1:8080` | Axum listen address |
| `SEIZA_STAR_DATA` | unset | Seiza tile catalog path |
| `SEIZA_OBJECT_DATA` | unset | Optional Seiza object catalog for named overlay annotations |
| `SEIZA_FRONTEND_DIR` | `frontend/dist` | Production static UI directory |
| `SEIZA_DATA_DIR` | `data` | Local object storage root |
| `SEIZA_JOB_BACKEND` | `sqlx` | `sqlx` (SQLite or PostgreSQL URL) or `dynamodb` |
| `SEIZA_QUEUE_DATABASE` | `data/jobs.sqlite3` | Default SQLite path when no SQL URL is set |
| `SEIZA_SQL_DATABASE_URL` | SQLite URL for `SEIZA_QUEUE_DATABASE` | SQLx URL, e.g. `sqlite://…?mode=rwc` or `postgres://…` |
| `SEIZA_DYNAMODB_TABLE` | unset | Required with `SEIZA_JOB_BACKEND=dynamodb`; table partition key is string `pk` |
| `SEIZA_QUEUE_TRANSPORT` | `local` | `local` polling or `sqs` direct-notification transport; it is independent of the job store (`SEIZA_QUEUE_BACKEND` remains an alias) |
| `SEIZA_WORKER_COUNT` | `1` | Embedded worker count when enabled |
| `SEIZA_EMBEDDED_WORKERS` | `true` | Run workers inside the API process |
| `SEIZA_WORKER_TOKEN` | unset | Required shared secret for separate workers |
| `SEIZA_LEASE_SECONDS` | `900` | Exclusive worker-lease duration |
| `SEIZA_MAX_UPLOAD_BYTES` | `104857600` | Request/file size ceiling |
| `SEIZA_UPLOAD_RETENTION_SECONDS` | `86400` | Age after which uploaded image objects and visual previews are unavailable |
| `SEIZA_UPLOAD_CLEANUP_INTERVAL_SECONDS` | `3600` | Local/S3 expired-object sweep interval |
| `SEIZA_RATE_LIMIT_PER_MINUTE` | `6` | Per-client submission refill rate |
| `SEIZA_RATE_LIMIT_BURST` | `3` | Per-client initial burst size |
| `SEIZA_AUTH_MODE` | `public` | `public` or `stub-api-key` |
| `SEIZA_STORAGE_BACKEND` | `local` | `local` or `s3` |
| `SEIZA_S3_BUCKET` | unset | Required when storage is `s3` |
| `SEIZA_S3_PREFIX` | `uploads` | S3 object-key prefix |
| `SEIZA_SQS_QUEUE_URL` | unset | Required when queue transport is `sqs` |

`X-Forwarded-For`/`X-Real-IP` are used for anonymous fairness and rate limits.
Only accept those headers from a trusted reverse proxy in production.

## Deployment paths

### Local / single VM

Use a persistent local `SEIZA_DATA_DIR`, mount the star catalog read-only, and
run one process. The supplied [Dockerfile](Dockerfile) produces an image with
the compiled UI served by Axum. The lightweight [docker-compose.yml](docker-compose.yml)
is the local reference setup.

### Native RPM service packages

The repository can build target-native RPMs for Amazon Linux 2023 and Fedora
44. Each RPM includes the Rust server (with S3/SQS/DynamoDB support), built
React UI, systemd unit, default loopback-only configuration, and an nginx
example. It deliberately excludes the star catalog, which should be installed
and versioned separately on the host.

```bash
make rpm-al2023
make rpm-f44
```

Artifacts are written to `dist/rpm/al2023/` and `dist/rpm/f44/`. See
[the production RPM guide](docs/production-rpm.md) for installation, nginx,
catalog, and worker guidance.

### AWS / SQS workers

Build with AWS support and use a task role that can only `GetObject`,
`PutObject`, and `DeleteObject` under the chosen S3 prefix plus `ListBucket`
restricted to that prefix; `SendMessage`, `ReceiveMessage`, and `DeleteMessage`
on the configured SQS queue; plus `GetItem`, `PutItem`,
`UpdateItem`, `Scan`, and `TransactWriteItems` on the DynamoDB table when that
job backend is selected. The AWS client is explicitly configured to use the
AWS-LC-RS Rustls crypto provider rather than the legacy Ring connector:

```bash
cargo build --release --features aws
```

Run the API with `SEIZA_STORAGE_BACKEND=s3` and optionally
`SEIZA_QUEUE_TRANSPORT=sqs`. Choose one durable job store:

- `SEIZA_JOB_BACKEND=sqlx` with a PostgreSQL URL for a relational deployment.
- `SEIZA_JOB_BACKEND=dynamodb` and `SEIZA_DYNAMODB_TABLE=seiza-jobs` for a
  managed AWS job store. The supplied
  [DynamoDB template](infra/aws/seiza-jobs-dynamodb.yaml) creates the required
  `pk` string partition key.

SQS is a cross-process delivery adapter, not the authoritative scheduler: it
carries only job IDs. The selected durable job store retains priority selection,
leases, results, and the notification-outbox state, and retries failed SQS
publishes after a restart.

Run direct SQS workers with the same worker token and catalog:

```bash
cargo run --features aws -- worker --mode sqs --server http://api-host:8080
```

The worker receives a job ID from SQS, claims that job from the API's selected
job store, then deletes the SQS message only after completion is accepted.
Duplicate messages and expired leases are safe by design. The AWS SDK uses the
standard credential provider chain, so ECS task roles work without application
secrets.
Put `SEIZA_STAR_DATA` and optional `SEIZA_OBJECT_DATA` on a read-only EFS mount
or bake/version them into a dedicated solver image. The server sweeps expired
S3 uploads itself; configure a matching bucket lifecycle rule as
defense-in-depth.

For the supplied container build, enable the adapter explicitly:

```bash
docker build --build-arg CARGO_FEATURES="--features aws" -t seiza-server:aws .
```

For multi-AZ API replicas, select DynamoDB or a PostgreSQL SQLx URL while
keeping the `QueueTransport` and worker-lease protocol. A single SQLite file
is durable and supports multiple local processes, but it is not a multi-host
database. The adapter boundary is documented in
[docs/architecture.md](docs/architecture.md).

## Verification

```bash
cargo fmt --check
cargo check
cargo test
(cd frontend && npm run build && npm run lint)
```
