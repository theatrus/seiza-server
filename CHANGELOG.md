# Changelog

All notable changes to Seiza Server are documented here. Versions follow
[Semantic Versioning](https://semver.org/).

## Unreleased

- Show the sky position under the pointer on a solved image: RA and Dec,
  and the image pixel, in a box beside the pointer. The conversion applies
  the solution's SIP terms, as Seiza does. In the expanded view a click
  pins the position and opens a card with the coordinates in sexagesimal
  and decimal degrees, a button that copies the degrees, and searches of
  SIMBAD, NED, VizieR, Aladin Lite and ESASky around it, in a new tab. The
  search radius is eight screen pixels of sky, from 3″ to 30′. Escape or
  the card's × clears the pin. The smaller view still expands on a click.

- Move to Seiza 0.40.0, seiza-fits 0.5.0, seiza-image-io 0.10.0,
  seiza-satellites 0.22.0 and seiza-xisf 0.6.0. Blind solves run faster and
  match more stars: two 12 MP phone photos at 71″/px matched 214 and 244
  stars, where Seiza 0.27.0 matched 36 and 42. Photo decoding and EXIF orientation now come from `seiza-image-io`, and
  satellite tracks take the WCS as a sky-to-pixel closure. The API is
  unchanged.
- Add PSF Guard to the home page's downloads: a full-width box under the
  three app cards, with a "Download PSF Guard" button that goes to
  psf-guard.com. On a phone it stacks like the other cards.
- Say that Seiza for Windows makes parallax tours too, as it has since
  0.8.0. Its download card says so, and the parallax card names its
  **More options → Parallax video…** command and links its releases. The
  integrations docs add a paragraph on the Windows tour editor and name
  parallax tours in its heading.
- Show a parallax video on the home page. `SEIZA_MEDIA_DIR` names a
  directory the server serves at `/media/`, outside the frontend build. When
  it holds `parallax-tour.mp4`, with `parallax-tour.jpg` as its poster, the
  tour plays in a muted loop behind the hero, under "Plate-solve an image"
  and a new "Download Seiza" button, so both sit above the fold. A line
  under the buttons says what solving does, and the intro names parallax
  videos. A card after the downloads covers `seiza parallax-video` and the
  macOS app's parallax tours. The tour pauses off screen, has a Pause
  button, and waits for a press when the visitor asks for reduced motion or
  less data. Without the video the hero shows the Seiza mark as before.
- Every "Solve an image" button now reads "Plate-solve an image". The solve
  page has a compact heading with the same wording and, when the media
  directory holds `solve-example.webp`, a small solved example beside it;
  without the file the heading stands alone. The notes on resumable uploads,
  the result link, deletion and ownership move under the upload row.
- Draw constellation stick figures. Annotations carry `constellations`
  (each with its IAU abbreviation, name, pixel polylines clipped to the
  image and a suggested name position) and the CC BY 4.0
  `constellation_attribution`, projected through the solution's WCS by
  Seiza's `constellations` module. `constellations=false` leaves them out.
  The web app adds a Constellations layer, on by default, through
  `@seiza/astro-overlay` 0.6.0, shows the credit below the image and draws
  it into exported PNGs, and lists the dataset on the Data Sources page.
  Straight figure lines arrive as their two endpoints. The server-drawn
  `overlay.svg` has no constellation layer, and solution metadata no longer
  projects figures it never draws.
- `overlay.svg` now accepts `true`/`false` options such as `deep_sky=false`,
  which its query parsing previously rejected with HTTP 400.
- Draw only what a field can show. Annotations rank deep-sky objects by
  apparent size, brightness and naming and return at most 200 (`max_deep_sky`),
  leaving out unnamed objects under 1.5 px (`deep_sky_min_size_px`); a phone
  frame of Cassiopeia went from 17,559 annotations, mostly sub-pixel PGC
  galaxies, to about 260. Named stars (`max_named_stars`, 60), transients
  (`max_transients`, 60, magnitude 13 or brighter in fields wider than 10°)
  and minor bodies (`max_minor_bodies`, 100) are capped the same way.
  `include_objects=M31,NGC 457` always returns the named objects, and the
  response's `totals` gives each layer's count before the limits. The web
  app shows "200 of 16,593" on a capped layer and passes `include_objects`
  from the page address.
- Accept numeric annotation options on the SVG overlay endpoint, whose
  query parsing previously rejected numbers.
- Upgrade to Seiza 0.27.0, seiza-fits 0.3.0, seiza-satellites 0.16.0 and
  seiza-xisf 0.4.0. FITS uploads now read tile-compressed images, image
  extensions, signed 16-bit and 64-bit integer data, BLANK and HIERARCH
  cards.

## 0.5.0 - Unreleased

- Solve photos upright. JPEG, PNG, TIFF and WebP uploads are decoded through
  Seiza's EXIF-oriented raster reader, so the WCS, footprint, overlays,
  previews, Astrometry.net-compatible width scales and satellite-trail pixels
  share one frame. Solutions report `pixel_coordinates` with the stored size
  and the orientation applied. Raster solutions saved by earlier versions keep
  the stored pixel frame for their previews and trail checks.
- Make `min_scale_arcsec_per_pixel` and `max_scale_arcsec_per_pixel`
  optional. A missing bound comes from a photo's EXIF 35 mm-equivalent focal
  length, searched first with a wide fallback in case the photo was cropped;
  without EXIF the range stays 0.1–20"/px. Bounds a client sends always hold.
  Solver statistics report the scale range that solved and an `exif` hint
  source when the focal length supplied it.
- Fill a photo's capture time from EXIF `DateTimeOriginal` and its offset, or
  from the GPS time, and the observer site from GPS latitude and longitude,
  when the request leaves them out. Record the EXIF tags used in
  `satellite_metadata_keywords` with the new `exif` source. EXIF exposure time
  and GPS altitude are never used.
- Upgrade to Seiza 0.21.0, seiza-satellites 0.10.0 and seiza-fits 0.2.5.

## 0.4.0 - 2026-09-29

- Replace the browser date picker for the acquisition time with a typed field
  that accepts `YYYY-MM-DD HH:MM[:SS]` or an ISO timestamp with an offset, shows
  how the entry will be read while typing, and no longer rejects a time whose
  seconds or AM/PM segment was left blank.
- Upgrade to Seiza 0.18.16, seiza-fits 0.2.2, seiza-satellites 0.7.0, and
  seiza-xisf 0.3.0. XISF uploads now follow XISF 1.0 Revision 1, including
  embedded, inline and normal-storage images and compression subblocks.

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
