# Changelog

All notable changes to Seiza Server are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## Unreleased

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
