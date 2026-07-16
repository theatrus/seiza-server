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

- Native JSON API: resumable TUS uploads (with multipart fallback), job polling,
  explicit WCS/quality output, refreshable catalog annotations, downloadable
  FITS-style WCS headers, indexed object and stellar-identifier queries,
  optional Tycho/Bright Star/GCVS/WDS/IAU label overlays, a composite overlay
  endpoint, a 100 MB default file limit, structured errors, and CORS.
- Astrometry.net-compatible API subset: `POST /api/login`, `POST /api/upload`,
  `GET /api/submissions/:id`, `GET /api/jobs/:id`,
  `GET /api/jobs/:id/calibration`, and `GET /api/jobs/:id/info`.
- FITS (`.fit`, `.fits`, `.fts`), PNG, JPEG, TIFF, and WebP input. FITS files
  are decoded through `seiza-fits` and autostretched before source detection.
- Hinted solves when RA, Dec, and pixel scale are supplied; otherwise blind
  solving with Seiza 0.5.0. The maintained G<=16 index is memory-mapped once per
  worker and reused across jobs, including fine-scale fields down to 0.1"/px.
  Seiza 0.5 automatically uses compact detection for 8-bit uploads and its
  optimized hinted and blind solve paths.
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
- Every solve has a durable `/solutions/:public_id` web page. Its public ID is
  the same random UUID used throughout the durable job and worker APIs; it
  cannot be discovered by incrementing a queue sequence. Uploaded originals
  and derived visual previews expire after one day by default, while the job
  and its complete WCS and annotation
  metadata remain available. The React UI renders an interactive SVG layer over
  the retained image preview.

## Quick start

Install Seiza CLI 0.5.0 or newer, then get the prebuilt catalogs and maintained
blind index. The server automatically prefers the deep Gaia G<=17 catalog and
its matching G<=16 index when both are present. Seiza 0.5.0's prebuilt object
catalog is memory-mapped, includes the expanded LBN and Cederblad datasets, and
provides embedded spatial and designation indices. The prebuilt bundle also
includes `stars-lite-tycho2.ids.bin`; the server turns its proper,
Bayer/Flamsteed, variable, and double-star designations into a separate,
magnitude-limited overlay layer.

```bash
cargo install seiza-cli
seiza download-data prebuilt --output ../seiza-data

cp .env.example .env # copy the values into your shell or dotenv runner
export SEIZA_CATALOG_DIR="$PWD/../seiza-data"

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
this repository. `SEIZA_BLIND_INDEX` is strongly recommended for blind solving;
without it each worker builds and caches a shallower legacy index on its first
blind job. The health endpoint stays available without a star catalog and
reports `"degraded"`, while queued solves fail with a clear configuration
error.

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
SEIZA_STAR_DATA=/data/stars-deep-gaia17.bin \
SEIZA_BLIND_INDEX=/data/blind-gaia16.idx \
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

The health response includes the running `seiza-server` version and the exact
locked `seiza` crate version under `versions`.

The web client uploads through the TUS 1.0 endpoint at `/api/v1/uploads` using
5 MiB chunks, automatic retries, and offset-based resume. In-progress manifests
and chunks live in the configured local or S3 object store, so an API-process
restart does not discard progress. Once the declared length is complete, the
server assembles the object, creates exactly one queued solve, and exposes the
job from `GET /api/v1/uploads/:upload_id/result`. Any standard TUS client can
use the same creation, `HEAD`, `PATCH`, concatenation, and termination flow.
The browser sends up to three partial uploads concurrently for files of at
least 10 MiB, aligning partial boundaries to 5 MiB chunk boundaries so S3 can
complete the final object with native multipart copies. Local storage streams
those chunks into the final file without buffering the whole image in memory.

The original multipart endpoint remains available for small scripts and
Astrometry-compatible clients. Submit a blind solve with `options` as a JSON
form field:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/solves \
  -F 'file=@M31.fits' \
  -F 'options={"min_scale_arcsec_per_pixel":0.1,"max_scale_arcsec_per_pixel":15}'
```

The response is `202 Accepted` with an opaque ID and artifact URLs. Poll it until
`status` becomes `succeeded` or `failed`:

```bash
PUBLIC_ID='550e8400-e29b-41d4-a716-446655440000'
curl "http://127.0.0.1:8080/api/v1/solves/$PUBLIC_ID"
```

Completed jobs report end-to-end `solve_time_ms` from worker claim through
durable completion, including failed attempts. Successful `solution` objects
also retain a `statistics` block with the solver strategy, total pipeline time,
decode/detection/search timings, detected-star count, catalog size, and blind
index pattern count when applicable. These diagnostics remain available after
the temporary uploaded image expires.

Ordinary uploads remain the user’s property. Seiza does not claim ownership
and stores the original only temporarily to process and present the solve.
After either success or failure, the user may explicitly contribute the still-
retained image to the long-term validation set with an optional comment. The
historical `validation-donation` route name remains for API compatibility:

```bash
curl -X POST "http://127.0.0.1:8080/api/v1/solves/$PUBLIC_ID/validation-donation" \
  -H 'Content-Type: application/json' \
  -d '{"comment":"Sparse field that failed blind solving","solve_is_invalid":true,"license_agreed":true}'
```

Set the optional `solve_is_invalid` flag for an incorrect WCS, a false
positive, or a failed solve that should have succeeded. It defaults to
`false` and is stored with the validation record.

The submitter attests that they own the image or have authority to contribute
it. The affirmative permission is recorded as
`seiza-validation-image-grant-v2`. The contributor retains ownership and gives
Seiza and its maintainers permission to retain, copy, and process the image as
part of Seiza's validation set, only to test, validate, debug, and improve the
Seiza plate solver, including training and evaluating solver-related models.
Seiza will not make the validation set public, sell the image, or use it for
unrelated purposes.

While the input is retained, a failed job can be requeued at the same opaque
URL with new solve hints and no second upload:

```bash
curl -X POST "http://127.0.0.1:8080/api/v1/solves/$PUBLIC_ID/retry" \
  -H 'Content-Type: application/json' \
  -d '{"center_ra_deg":202.47,"center_dec_deg":47.2,"scale_arcsec_per_pixel":1.35,"radius_deg":3}'
```

Successful jobs expose an on-demand PNG preview while the uploaded image is
retained, plus persistent annotations and a FITS-compatible WCS header:

```bash
curl "http://127.0.0.1:8080/api/v1/solves/$PUBLIC_ID/annotations?field_stars=true&historical_transients=true"
curl -OJ "http://127.0.0.1:8080/api/v1/solves/$PUBLIC_ID/wcs"
```

The grid is projected through the solved TAN WCS rather than drawn in image
coordinates, so its meridians and parallels reflect field curvature, rotation,
parity, and RA wraparound. The solution page draws that grid and catalog
markers as a transparent React SVG over the preview, with independent controls
for deep-sky objects, named stars, Tycho-sidecar star identifiers, field stars,
transients, minor bodies, and historical transients. **Download rendered PNG** fetches the retained image at
full resolution and composites the currently selected layers into a PNG in the
browser. The exported image carries a small Seiza logo, “Solved with Seiza,”
and `seiza.fyi` mark; it does not download an SVG.

`GET /api/v1/solves/:public_id/overlay.svg` remains as an optional self-contained
image output for API clients. Its query supports `objects`, `grid`,
`deep_sky`, `named_stars`, `star_identifiers`, `field_stars`, `transients`,
`minor_bodies`, `historical_transients`, `star_identifier_mag_limit`,
`max_star_identifiers`, `field_star_mag_limit`, and `max_field_stars`.

The JSON solution includes the full TAN/ICRS WCS (`CTYPE`, `CUNIT`, `CRVAL`,
zero-indexed internal `CRPIX`, CD matrix, `RADESYS`, and `EQUINOX`), the four
ICRS footprint corners, and current projected catalog objects when annotation
catalogs are configured. Static catalog changes are detected and reprojected
through the stored WCS without rerunning the solver. The downloadable `.wcs`
converts `CRPIX` to FITS' one-indexed convention.

The native catalog API can query the configured deep-sky catalog without
submitting an image. Cone queries support kind, magnitude, angular-size,
common-name, extent-overlap, result-limit, and sort controls:

```bash
curl "http://127.0.0.1:8080/api/v1/catalog/objects?ra=10.6848&dec=41.2691&radius=3&kinds=galaxy,nebula&max_mag=14&sort=prominence&limit=100"
curl "http://127.0.0.1:8080/api/v1/catalog/objects/search?q=M31"
curl "http://127.0.0.1:8080/api/v1/catalog/objects/search?q=ced&prefix=true&limit=20"
curl "http://127.0.0.1:8080/api/v1/catalog/stars/search?q=TYC%205949-2777-1"
curl "http://127.0.0.1:8080/api/v1/catalog/stars/search?q=RR%20L&prefix=true&limit=20"
```

`sort` accepts `prominence`, `size`, `magnitude`, `distance`, or `name`.
Responses include stable IDs, aliases, hierarchy, and source provenance when
the v3 catalog supplies them. Legacy v1 object catalogs remain readable, but
their identity/provenance fields are empty and their name lookups require an
in-memory scan.

FITS `DATE-OBS` is captured automatically. When explicit solve hints are
absent, the server also promotes a complete FITS position and pixel scale from
common `RA`/`DEC`, `OBJCTRA`/`OBJCTDEC`, WCS, `PIXSCALE`, or camera-geometry
headers. Non-FITS API clients can provide a RFC 3339 `capture_time` in the
options JSON. Capture time scopes transient events and propagates comets and
asteroids to the acquisition instant.

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

All solver work happens after enqueue; upload handlers only validate, store,
rate-limit, and enqueue. `SEIZA_MAX_UPLOAD_BYTES` limits the complete image,
not an individual TUS chunk.

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
| `SEIZA_CATALOG_DIR` | automatic | Directory searched for canonically named prebuilt Seiza datasets; defaults to `SEIZA_DATA_DIR/catalog` and its sibling `catalog` directory |
| `SEIZA_STAR_DATA` | unset | Seiza tile catalog path; automatic discovery prefers `stars-deep-gaia17.bin` |
| `SEIZA_BLIND_INDEX` | unset | Seiza persisted blind-index path; automatic discovery uses `blind-gaia16.idx` |
| `SEIZA_OBJECT_DATA` | unset | Optional Seiza object catalog for named overlay annotations |
| `SEIZA_STAR_IDENTIFIER_DATA` | unset | Optional Tycho/Bright Star/GCVS/WDS/IAU identifier sidecar; automatic discovery uses `stars-lite-tycho2.ids.bin` |
| `SEIZA_TRANSIENT_DATA` | unset | Optional reloadable Seiza object catalog containing transient events |
| `SEIZA_MINOR_BODY_DATA` | unset | Optional reloadable Seiza minor-body orbital-elements catalog |
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
| `SEIZA_MAX_UPLOAD_BYTES` | `104857600` | Complete image size ceiling; resumable requests contain smaller chunks |
| `SEIZA_UPLOAD_RETENTION_SECONDS` | `86400` | Age after which uploaded image objects and visual previews are unavailable |
| `SEIZA_UPLOAD_CLEANUP_INTERVAL_SECONDS` | `3600` | Local/S3 expired-object sweep interval |
| `SEIZA_RATE_LIMIT_PER_MINUTE` | `6` | Per-client submission refill rate |
| `SEIZA_RATE_LIMIT_BURST` | `3` | Per-client initial burst size |
| `SEIZA_AUTH_MODE` | `public` | `public` or `stub-api-key` |
| `SEIZA_STORAGE_BACKEND` | `local` | `local` or `s3` |
| `SEIZA_S3_BUCKET` | unset | Required when storage is `s3` |
| `SEIZA_S3_PREFIX` | `uploads` | S3 object-key prefix |
| `SEIZA_VALIDATION_PREFIX` | `validation` | Object-key prefix protected from temporary-upload cleanup for contributed validation images |
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

Both backends use one UUIDv4 for each job: it is the durable primary key, public
result locator, worker handle, object-path identity, and SQS message body.
DynamoDB therefore needs no counter item. SQL deployments automatically copy
records from the older numeric schema into the UUID schema, using each upload's
existing public UUID, and retain mappings for legacy and Astrometry-compatible
numeric URLs.

SQS is a cross-process delivery adapter, not the authoritative scheduler: it
carries only the job UUID. The selected durable job store retains priority
selection, leases, results, and the notification-outbox state, and retries
failed SQS publishes after a restart.

Run direct SQS workers with the same worker token and catalog:

```bash
cargo run --features aws -- worker --mode sqs --server http://api-host:8080
```

The worker receives a job ID from SQS, claims that job from the API's selected
job store, then deletes the SQS message only after completion is accepted.
Duplicate messages and expired leases are safe by design. The AWS SDK uses the
standard credential provider chain, so ECS task roles work without application
secrets.
Put `SEIZA_STAR_DATA`, `SEIZA_BLIND_INDEX`, and optional annotation catalogs on
a read-only EFS mount or bake/version them into a dedicated server image.
Workers need the star catalog and blind index; annotation catalogs are loaded
by the API server. The server sweeps expired
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

## Migrating the job store

An AWS-enabled build includes a bidirectional `migrate-store` command. It
preserves job UUIDs and state, legacy and Astrometry.net numeric aliases, solve
options and results, active lease metadata, retry attempts, weighted-LRU client
timestamps, and durable outbox delivery state. Validation-contribution metadata,
including the invalid-solve classification, is included when present, and
DynamoDB object-key and compatibility-index items are rebuilt from the
authoritative job records. Legacy numeric stores are converted by reusing the
UUID already embedded in each upload's unguessable object key.

Stop every API server and worker that can access either store before taking the
snapshot, and keep them stopped until the destination has been verified and the
deployment is switched over. Back up the SQL database or enable DynamoDB
point-in-time recovery before the cutover. The command migrates the job store
only; it does not copy local/S3 image objects, so the new deployment must retain
access to the upload and validation object keys referenced by the jobs.

Check a SQLx-to-DynamoDB migration without copying records:

```bash
cargo run --release --features aws -- migrate-store \
  --from sqlx \
  --to dynamodb \
  --sqlx-url 'postgres://seiza@db/seiza' \
  --dynamodb-table seiza-jobs \
  --dry-run
```

Remove `--dry-run` to perform the migration. The destination must be empty by
default, and the command re-reads it after the copy and requires an exact
logical match before reporting success. If a DynamoDB write was interrupted,
rerun with `--resume`; every existing destination record must be an exact
subset of the current source snapshot or the command refuses to write.
The reader also fails closed on unknown SQL columns or DynamoDB attributes so
a newer persisted schema cannot be silently truncated by an older binary.

Reverse the direction to move back to SQLite or PostgreSQL:

```bash
cargo run --release --features aws -- migrate-store \
  --from dynamodb \
  --to sqlx \
  --dynamodb-table seiza-jobs \
  --sqlx-url 'sqlite:///var/lib/seiza-server/jobs.sqlite3?mode=rwc'
```

`SEIZA_SQL_DATABASE_URL` and `SEIZA_DYNAMODB_TABLE` can replace the matching
flags. DynamoDB connections use the standard AWS SDK credential, region, and
endpoint environment variables. A dry run connects to both stores and may
initialize the SQLx schema, but it does not copy queue records.

## Verification

```bash
cargo fmt --check
cargo check
cargo test
(cd frontend && npm run build && npm run lint)
```
