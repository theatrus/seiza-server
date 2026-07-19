# Changelog

All notable changes to Seiza Server are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## Unreleased

## 0.3.0 - 2026-07-18

- Add verified-email accounts with passkey-first sign-in, multi-session browser
  authentication, scoped and revocable API keys, SQLx/DynamoDB identity stores,
  SES and authenticated SMTP delivery, and hardened email abuse controls.
- Keep public browser and API solves independently configurable in accounts
  mode, route anonymous jobs through the normal queue, and list recent solves
  on the submitting account.
- Upgrade to Seiza 0.8.1 with SIP polynomial orders 2–5, velocity-aware moving
  bodies, improved data-directory discovery, and the corrected library data
  path behavior.
- Make re-solves immutable new jobs, preserve original results, and add ETag and
  conditional HTTP caching for solve responses.
- Replace idle embedded-worker database polling with in-process wakeups and a
  long recovery fallback; replace DynamoDB recovery scans with a bounded GSI.
- Add SQS fair-queue groups and a separate weighted priority queue while
  preserving durable job-store ownership and recovery semantics.
- Increase TUS upload chunks to 32 MiB and retain independent public UI/API
  admission controls.
- Adopt the published overlay package's suggested catalog palette, motion
  vectors, and semantic catalog layers.
- Document guided Seiza catalog setup plus N.I.N.A. ASTAP-compatible and Siril
  solve-field-compatible integrations.

## 0.2.0 - 2026-07-17

- Upgrade to Seiza 0.6.0 and `@seiza/astro-overlay` 0.2.0 with object-catalog
  v4 provenance, aliases, hierarchy, source records, and projected outlines.
- Add searchable deep-sky and stellar-identifier catalogs, per-catalog overlay
  controls, RA/Dec grids, moving-body direction tails, and branded PNG export.
- Add durable, unguessable solution pages with complete WCS metadata, solve
  timing, fit statistics, and retained catalog metadata after image expiry.
- Add parallel resumable TUS uploads, FITS-derived hints, retry-with-hints
  without re-uploading, and durable image contribution workflows.
- Add SQLx/DynamoDB store migration, unified UUID job identity, and improved
  cached Amazon Linux 2023 and Fedora 44 RPM builds.
- Add API, N.I.N.A./ASTAP, data-source attribution, sitemap, and production
  deployment documentation.
- Auto-discover canonically named solver and annotation catalogs from a shared
  catalog directory, including the production sibling-directory layout.
- Report unavailable overlay data explicitly and render catalog markers with
  Safari-safe SVG color attributes.
- Exercise overlay display and rendered PNG downloads in Chromium and WebKit.

## 0.1.0 - 2026-07-13

- Add the Axum JSON and Astrometry.net-compatible plate-solving APIs.
- Add the React/Vite web interface and FITS plus common raster image uploads.
- Add durable weighted-LRU SQLx and DynamoDB job stores.
- Add authenticated remote workers, leases, local polling, and SQS transport.
- Add local and S3 object storage with AWS-LC-backed TLS.
- Add production Amazon Linux 2023 and Fedora 44 RPM packaging.
