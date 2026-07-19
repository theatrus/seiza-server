# Architecture

## Request lifecycle

```mermaid
flowchart LR
    Browser["React / Vite UI"] --> Upload["TUS resumable upload\n32 MiB chunks"]
    Client["Native or Astrometry-compatible client"] --> API
    Upload --> API["Axum API"]
    API --> Admission["Authentication + token bucket"]
    Admission --> Store["Object store\nlocal disk or S3"]
    Store --> Queue["Durable SQLx or DynamoDB\nweighted-LRU priority queue"]
    Queue --> Embedded["Embedded worker"]
    Queue --> WorkerAPI["Authenticated\nworker API"]
    Queue --> Outbox["Durable outbox"]
    Outbox --> SQS["Optional SQS adapter"]
    SQS --> Direct["Direct SQS worker"]
    WorkerAPI --> Remote["Remote HTTP worker"]
    Embedded --> Catalog["Read-only Seiza catalog"]
    Remote --> Catalog
    Direct --> Catalog
    API --> Annotations["Annotation engine\nDSO, stars, transients, minor bodies"]
    Annotations --> Catalog
```

The request path stops at enqueue. It does not invoke Seiza, build blind
indices, or hold the upload socket while detecting stars. That split is the
core behavior for a shared service: a client always receives an opaque public
solve ID quickly, and workers are the only place expensive CPU/memory work can
occur.

## Queue policy

Each entry has a client identity, a submission time, and a weight. At claim,
the scheduler selects the client with the greatest:

`time since the client was last served × client weight`

The default public, stub-key, and account-key weights are `1.0`, so this is
least-recently-served scheduling with FIFO ties. The weight supports
operator-managed priority keys and future account tiers rather than a
client-controlled priority field. A single client
cannot keep a backlog ahead of a client that has not recently received service.

The SQLx repository runs selection in a transaction (SQLite or PostgreSQL),
creates a random lease token, and updates `client_service.last_served_at` in
that transaction. The DynamoDB repository uses conditional item updates for
the same exclusive lease boundary. Workers must present that token to fetch
input, heartbeat, or complete. Expired leases return to `queued`; completion
updates only apply to the current token.

Embedded workers check the repository once at startup to recover durable queued
work, registering an in-process wakeup before that claim. Submissions and
new jobs created by re-solving signal the wakeup after their durable update.
Thereafter, an idle worker checks the repository only after one configured lease
period as a recovery path for cross-process writes and expired leases.

Admission is separate and uses a token bucket per client/IP. It returns HTTP
429 with `Retry-After` before the upload is persisted when the bucket is empty.

## Storage and durability

| Concern | Local baseline | AWS deployment | Horizontal production step |
| --- | --- | --- | --- |
| Original image | `SEIZA_DATA_DIR/objects` | S3 | Server sweep plus lifecycle defense-in-depth |
| Contributed validation image | protected object namespace | S3 prefix outside temporary lifecycle | explicit image grant plus durable contribution metadata |
| In-progress upload | object-store manifest + chunks | S3 manifest + chunks | Shared object store; resumable across API restarts |
| Catalog | local readonly path | EFS or immutable image layer | Versioned catalog release |
| Job record | SQLx SQLite file | DynamoDB or SQLx PostgreSQL | DynamoDB or SQLx PostgreSQL |
| Scheduler | SQLx transaction | job store + SQS notification outbox | durable job store plus queue outbox |
| Worker | Tokio tasks or HTTP worker | ECS/EC2 HTTP or direct SQS worker | dedicated worker service |
| Authentication | public/stub/accounts | public/stub/accounts | SQLx or DynamoDB identity store, scoped API keys, passkeys, revoke, tier/weight |

The account, verified-email, passkey, API-key, and pluggable email
architecture is specified in
[Accounts, passkeys, API keys, and email authentication](accounts-authentication.md).
It is implemented behind the opt-in `SEIZA_AUTH_MODE=accounts` runtime mode;
`public` and `stub-api-key` remain the defaults.

SQLite survives process restarts and supports multiple worker processes that
claim through one API process. It is intentionally a single-host durable queue:
do not place its database file on object storage or use it as a multi-AZ
database. SQLx also supports a PostgreSQL URL, and the DynamoDB repository is
a single table with a string `pk` partition key plus a
`job-status-created-at` GSI. For direct cloud delivery, the API publishes only
job IDs to SQS after the durable job write; the API remains the lease authority
and protects worker operations with a shared token. Each job has one
random UUIDv4 used by the repository, public result URL, worker API, upload
object path, and SQS message, so neither backend needs a centralized sequence
generator. A direct worker claims that UUID with one primary-key read and
renews SQS visibility with its database heartbeat. SQS therefore redelivers a
crashed worker's message after its lease expires without a database sweep.

The GSI is the finite-time safety net, not a second queue. After one lease
period, the API queries `status = queued` ordered by `created_at` for records
whose initial publish was never acknowledged and republishes them. Recovery of
expired leases and embedded-worker scheduling also query the small active
`queued` or `solving` partitions. Historical completed jobs are never scanned
during queue operation; table `Scan` remains only for store migration and one
legacy object-key compatibility lookup.

The AWS-enabled `migrate-store` command copies the complete logical job-store
snapshot between SQLx and DynamoDB while the service is quiesced. It preserves
job UUIDs, legacy and Astrometry.net aliases, fairness state, leases, attempts,
results, and outbox delivery timestamps. It includes validation-contribution
metadata, rebuilds DynamoDB object and compatibility indexes, converts old
numeric records using the UUID already present in their upload object keys, and
verifies the destination snapshot before a deployment changes backends.
Contribution metadata includes the invalid-solve classification; uploaded image
objects remain a separate object-store migration concern.

Uploaded objects have a deliberately short lifecycle independent of job
durability. The API reports `input_expires_at`, denies preview/overlay access
after the configured retention window, and periodically deletes old objects
by filesystem modification time or S3 `LastModified`. The default is 24 hours
with an hourly sweep. Job rows, calibration JSON, capture time, footprints,
and downloadable WCS headers remain in the selected durable job store.
Catalog annotations are regenerated from that WCS, so catalog upgrades do not
require a new solve. No schema-specific expiration process is required, so the
same policy works with SQLite, PostgreSQL, and DynamoDB. Production S3 buckets
should also use a matching lifecycle rule to cover interrupted cleanup.

Ordinary image submission does not transfer ownership and does not opt the
image into long-term storage. Once a job reaches `succeeded` or `failed`, its
opaque result page can submit an optional comment plus an affirmative,
versioned image grant. The object-store abstraction copies the original into
`SEIZA_VALIDATION_PREFIX`; local and S3 cleanup both exclude that namespace.
SQLx records the durable object key, comment, invalid-solve flag, grant
version, and acceptance time in `validation_donations`; DynamoDB stores the
same fields on the job item. Subsequent preview and re-solve reads prefer the
durable copy, while the temporary original remains eligible for normal
cleanup. Re-solving copies the retained source through the object-store
adapter, creates a new durable job UUID, and leaves the prior result unchanged.
S3 deployments should keep the validation prefix outside the temporary-upload
lifecycle rule.

The browser uses Uppy’s TUS client with 32 MiB chunks and retry delays. Files of
at least 64 MiB are split into up to three concurrent TUS partial uploads. The
partial boundaries stay aligned to chunk boundaries, leaving only the final
chunk smaller than S3’s 5 MiB multipart minimum. A random upload-session URL
identifies each manifest stored beside its chunks in the selected object store.
`HEAD` returns the durable byte offset, so a client can resume after a browser,
network, or API-process interruption. Local finalization streams chunks into
the destination file; S3 uses multipart `UploadPartCopy` and completion without
downloading and re-uploading the image through the API process. Non-aligned TUS
clients retain a compatible buffered S3 fallback. Finalization is idempotent:
the jobs table enforces one row per private object key, and a lost completion
response reuses that row rather than queueing the image twice. Partial sessions
follow the same retention sweep as completed originals.

A successful or failed job can be re-solved with replacement options while its
input is retained. The object-store adapter copies the source into a fresh
retention scope and the repository creates a new queued job with a new UUID and
outbox entry. The original job, result URL, options, and artifacts remain
unchanged. The historical `/retry` route is an alias for this immutable
`/resolve` behavior; it no longer transitions the existing record back to
`queued`.

Preview PNGs are generated on demand rather than stored as additional durable
objects. The web client renders the preview as the base image and places a
responsive React SVG over it, independently toggling catalog categories and a
true WCS-projected RA/Dec graticule. PNG export fetches a full-resolution base
image and rasterizes the currently selected React overlay into one browser-side
PNG, then adds the Seiza logo and `seiza.fyi` attribution plaque; users never
need to download an SVG. The optional composite SVG API is generated on demand
for machine clients. Once the original expires,
image-backed visual artifacts return HTTP 410 while annotation JSON and the
standards-facing WCS download remain available.

## Calibration and annotation boundary

Workers persist only solve calibration: dimensions, linear or TAN-SIP WCS,
footprint, matched stars, and RMS. SIP coefficient records carry explicit
exponents so persisted SQLx and DynamoDB JSON does not depend on Seiza's
internal coefficient ordering. They do not need object, transient, or
minor-body catalogs.
The API-side annotation engine projects those catalogs through the durable WCS
when a solution or `/annotations` endpoint is read. Deep-sky and transient
catalog files are checked for replacement and reloaded without a server
restart. Minor bodies are propagated to the capture time; FITS `DATE-OBS` is
captured during submission and non-FITS clients can provide it explicitly.

The optional `SEIZASI1` stellar-identifier sidecar is likewise memory-mapped
and hot-reloaded. At open time the server chooses one preferred human-facing
designation per stable stellar ID and builds a small sky-bin index. Annotation
requests project only the matching bins through the WCS, while
`/api/v1/catalog/stars/search` retains exact TYC/HIP and indexed textual lookup.

Seiza 0.8.1 object catalogs are read-only memory maps with embedded spatial and
designation indices. Overlay projection and `/api/v1/catalog/objects` cone
queries therefore materialize only matching records, while
`/api/v1/catalog/objects/search` uses the exact/prefix name index. Legacy v1
catalogs remain compatible but are decoded eagerly and do not carry stable IDs,
aliases, hierarchy, or source provenance. Blind-index startup likewise defers
the exhaustive validation pass, keeping API and worker startup independent of
the full catalog size.

This boundary keeps HTTP and cloud-queue workers interchangeable while catalog
updates immediately improve old solution pages. Named stars come from the
object catalog; the independent Star identifiers layer adds proper,
Bayer/Flamsteed, variable, and double-star labels from the Tycho sidecar; and
an optional field-star layer projects the solve tile catalog with a magnitude
threshold and result cap.

Native result URLs use the job's random UUIDv4 capability, indexed directly by
both repositories. The same UUID identifies the job to internal workers and
queue transports, so DynamoDB and distributed submitters need no shared counter.
Queue ordering comes from timestamps and weighted least-recently-used client
state, never from the UUID. The Astrometry.net compatibility API retains a
numeric alias because that protocol specifies numbers; it is not an ordering
sequence. Migration preserves old numeric aliases and continues to resolve
legacy `<sequence>-<UUID>` result URLs.

## API compatibility boundary

The native `/api/v1` API is the canonical service contract. The `/api/*`
endpoints mirror the useful polling path from Astrometry.net, including its
`request-json` form field and its calibration fields. This gives existing
clients a low-friction route without making Seiza's richer WCS result depend on
Astrometry.net's historic naming.

The compatibility API intentionally does not fetch `url_upload` sources. A
server-side URL fetch would need strict egress controls, DNS rebinding defenses,
size streaming, and an allowlist policy; client-side upload is the safe default.
