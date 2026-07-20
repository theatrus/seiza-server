import { ReactNode, useRef, useState } from 'react'

const multipartExample = `curl -X POST https://seiza.fyi/api/v1/solves \\
  -F 'file=@M31.fits' \\
  -F 'options={"min_scale_arcsec_per_pixel":0.1,"max_scale_arcsec_per_pixel":20}'`

const pollExample = `PUBLIC_ID='550e8400-e29b-41d4-a716-446655440000'
curl "https://seiza.fyi/api/v1/solves/$PUBLIC_ID"`

const retryExample = `# A completed solve can create a new solve from its retained image.
curl -X POST "https://seiza.fyi/api/v1/solves/$PUBLIC_ID/resolve" \\
  -H 'Content-Type: application/json' \\
  -d '{"center_ra_deg":202.47,"center_dec_deg":47.2,"scale_arcsec_per_pixel":1.35,"radius_deg":3}'`

const contributionExample = `# Available after either a successful or failed solve, while the image exists.
curl -X POST "https://seiza.fyi/api/v1/solves/$PUBLIC_ID/validation-donation" \\
  -H 'Content-Type: application/json' \\
  -d '{"comment":"Sparse field that failed blind solving","solve_is_invalid":true,"license_agreed":true}'`

const catalogExample = `# Objects in a three-degree cone around M31
curl 'https://seiza.fyi/api/v1/catalog/objects?ra=10.6848&dec=41.2691&radius=3&kinds=galaxy,nebula&max_mag=14&sort=prominence&limit=100'

# Exact designation or stable-ID lookup
curl 'https://seiza.fyi/api/v1/catalog/objects/search?q=M31'

# Source records, selections, relations, geometries, outlines, and provenance
curl 'https://seiza.fyi/api/v1/catalog/objects/details/openngc:NGC224'

# Prefix search across names, aliases, and stable IDs
curl 'https://seiza.fyi/api/v1/catalog/objects/search?q=ced&prefix=true&limit=20'

# Exact TYC/HIP lookup or stellar-name prefix completion
curl 'https://seiza.fyi/api/v1/catalog/stars/search?q=TYC%205949-2777-1'
curl 'https://seiza.fyi/api/v1/catalog/stars/search?q=RR%20L&prefix=true&limit=20'`

const tusExample = `# Create an upload. Upload-Metadata values use base64.
curl -i -X POST https://seiza.fyi/api/v1/uploads \\
  -H 'Tus-Resumable: 1.0.0' \\
  -H 'Upload-Length: 12582912' \\
  -H 'Upload-Metadata: filename TTUxLmZpdHM=,filetype YXBwbGljYXRpb24vZml0cw=='

# Resume from the offset returned by HEAD, then read the queued job.
curl -I -H 'Tus-Resumable: 1.0.0' "$UPLOAD_URL"
curl -X PATCH "$UPLOAD_URL" \\
  -H 'Tus-Resumable: 1.0.0' \\
  -H 'Upload-Offset: 0' \\
  -H 'Content-Type: application/offset+octet-stream' \\
  --data-binary @chunk.bin
curl "$UPLOAD_URL/result"`

const astrometryExample = `curl -X POST https://seiza.fyi/api/login \\
  --data-urlencode 'request-json={"apikey":"your-key"}'

curl -X POST https://seiza.fyi/api/upload \\
  -F 'request-json={"session":"seiza-…","scale_type":"ul","scale_units":"arcsecperpix","scale_lower":0.5,"scale_upper":2.0}' \\
  -F 'file=@M31.fits'`

const ninaExample = `# PowerShell: the MSI adds Seiza to PATH.
# This is the same guided catalog setup opened by "Seiza Catalog Setup".
seiza setup

# Portable ZIP alternative: install the complete bundle into one directory.
C:\\Seiza\\seiza.exe download-data prebuilt --output C:\\seiza-data
setx SEIZA_CATALOG_DIR C:\\seiza-data

# In N.I.N.A.: Options → Plate Solving
# Plate Solver: ASTAP
# ASTAP path: C:\\Program Files\\Seiza\\seiza.exe
# Blind Solver: ASTAP
# ASTAP path: C:\\Program Files\\Seiza\\seiza.exe`

const sirilExample = `# PowerShell: install the solve-field-compatible layout for Siril.
$sirilDir = "$env:LOCALAPPDATA\\Seiza\\siril-asnet"
seiza install-solve-field --dir $sirilDir

# In Siril: Preferences → Astrometry
# Location of local astrometry.net solver: the value of $sirilDir
# In Plate Solving, select Astrometry.net instead of the internal Siril solver.`

const errorExample = `{
  "error": {
    "code": "bad_request",
    "message": "blind scale bounds are invalid"
  }
}`

export function ApiDocsPage() {
  return <main className="api-docs-page">
    <header className="api-hero">
      <div>
        <p className="eyebrow">SEIZA SERVER API</p>
        <h1>Plate solving for software, scripts, and observatories.</h1>
        <p className="intro">Submit an image, leave the solve in the durable queue, and poll an opaque result URL for WCS calibration, catalog metadata, and optional satellite-track context. The native API is JSON-first; an Astrometry.net-compatible subset is available for existing clients.</p>
      </div>
      <div className="api-base-url" aria-label="API base URL">
        <span>Base URL</span>
        <code>https://seiza.fyi</code>
        <small>Examples use the public service. Self-hosted installations expose the same paths.</small>
      </div>
    </header>

    <div className="api-layout">
      <aside className="api-toc" aria-label="API documentation sections">
        <strong>On this page</strong>
        <a href="#quick-start">Quick start</a>
        <a href="#integrations">Integrations</a>
        <a href="#native-api">Native API</a>
        <a href="#solve-options">Solve options</a>
        <a href="#resumable-uploads">Large uploads</a>
        <a href="#catalog-api">Catalog API</a>
        <a href="#account-api">Accounts and API keys</a>
        <a href="#astrometry-api">Astrometry compatibility</a>
        <a href="#worker-api">Worker API</a>
        <a href="#errors">Errors and limits</a>
      </aside>

      <div className="api-content">
        <DocSection id="quick-start" eyebrow="FIRST SOLVE" title="Multipart upload, then poll.">
          <p>The simplest client sends one <code>file</code> part and an optional JSON <code>options</code> part. The server responds with <code>202 Accepted</code>; CPU-heavy solving happens later in a worker.</p>
          <CodeExample label="Submit an image" code={multipartExample} />
          <CodeExample label="Poll the opaque result URL" code={pollExample} />
          <div className="api-note"><strong>Authentication modes</strong><span>Public installations need no credential. Stub-key mode accepts any nonempty <code>X-API-Key</code> or Bearer token. Account mode can accept anonymous public solves on the normal queue; operators independently control bundled-browser and public API submissions with <code>SEIZA_PUBLIC_UI_SOLVES</code> and <code>SEIZA_PUBLIC_API_SOLVES</code>, both enabled by default. A browser session or revocable account API key attaches submissions to account history and applies its scopes and queue weight.</span></div>
          <div className="api-note"><strong>Surface controls</strong><span>The bundled frontend identifies its native and TUS requests as web traffic; unmarked native requests and Astrometry compatibility use the API setting. This distinction controls supported product surfaces, not adversarial access—a custom public client can reproduce browser headers. Require account credentials for a security boundary.</span></div>
          <div className="api-note"><strong>Result URLs are capabilities.</strong><span>The returned <code>id</code> is an unguessable UUID. Preserve it to revisit the result; the same UUID identifies the durable job to workers and queue transports.</span></div>
          <div className="api-note"><strong>Your images remain yours.</strong><span>Ordinary uploads are stored only temporarily to provide the solve. Seiza does not claim ownership and does not retain the image long-term unless the user explicitly contributes it.</span></div>
        </DocSection>

        <DocSection id="integrations" eyebrow="APPLICATION INTEGRATIONS" title="N.I.N.A., Siril, and persistent clients.">
          <p>The pre-built Seiza packages include compatibility modes for N.I.N.A.’s ASTAP integration and Siril’s local Astrometry.net integration. Neither application needs a Seiza-specific plugin or a Rust toolchain.</p>
          <h3>Install Seiza and its catalogs</h3>
          <ol className="integration-steps">
            <li><strong>Install the release.</strong><span>From the <a href="https://github.com/theatrus/seiza/releases">Seiza releases page</a>, download the latest <code>seiza-cli-…-windows-x86_64.msi</code>. The portable ZIP and Linux packages work too; use the directory-based manual setup shown below when the installer does not provide the PATH entry and catalog-setup shortcut.</span></li>
            <li><strong>Run catalog setup.</strong><span>Leave <strong>Download and configure Seiza catalogs now</strong> selected on the installer’s final page, open <strong>Start → Seiza → Seiza Catalog Setup</strong> later, or run <code>seiza setup</code> in a new PowerShell window. All three start the same guided, SHA-256-verified setup.</span></li>
            <li><strong>Choose for your workload.</strong><span>The lightweight telescope-control preset is sufficient for ordinary hinted N.I.N.A. solves. Choose the denser Gaia preset for narrow or crowded fields, or <strong>Unknown sky position</strong> to install the deep Gaia catalog and maintained blind index required for reliable blind solving.</span></li>
          </ol>
          <div className="api-note"><strong>One catalog directory</strong><span>For a portable or manual installation, run <code>seiza download-data prebuilt --output &lt;directory&gt;</code> and set <code>SEIZA_CATALOG_DIR</code> to that directory. ASTAP-compatible and solve-field-compatible modes select the appropriate available star catalog and blind index automatically. <code>SEIZA_STAR_DATA</code> and <code>SEIZA_BLIND_INDEX</code> remain advanced file-or-directory overrides.</span></div>
          <h3>N.I.N.A.: use Seiza in ASTAP-compatible mode</h3>
          <p>Under <strong>Options → Plate Solving</strong>, select <strong>ASTAP</strong> and point its executable path at the installed <code>seiza.exe</code> (normally <code>C:\Program Files\Seiza\seiza.exe</code> for an all-users install). Select ASTAP and the same executable in the blind-solver slot if you installed the blind preset. Do not add an <code>astap</code> subcommand: N.I.N.A. launches <code>seiza.exe</code> with ASTAP-style flags, and Seiza detects that contract automatically.</p>
          <CodeExample label="Set up the pre-built Seiza binary for N.I.N.A." code={ninaExample} />
          <div className="api-note"><strong>What N.I.N.A. receives</strong><span>N.I.N.A. supplies FITS input, field of view, and optional mount coordinates through ASTAP-style flags. Seiza writes the ASTAP <code>.ini</code> calibration result N.I.N.A. expects, including the full CD matrix for pixel scale, rotation, and parity. See the <a href="https://github.com/theatrus/seiza/blob/main/docs/design/astap-mode.md">ASTAP-compatible mode contract</a> for details.</span></div>
          <h3>Siril: use Seiza in solve-field-compatible mode</h3>
          <p>Siril does not use ASTAP. It can instead launch a local Astrometry.net <code>solve-field</code> installation. Run <code>seiza install-solve-field --dir &lt;directory&gt;</code> once, set Siril’s <strong>Preferences → Astrometry → Location of local astrometry.net solver</strong> to that directory, then select <strong>Astrometry.net</strong> in Plate Solving. Seiza creates the <code>solve-field</code>, <code>bin/bash</code>, and temporary-directory layout Siril expects; Windows does not need Cygwin or ansvr.</p>
          <CodeExample label="Install Seiza's Siril compatibility layout" code={sirilExample} />
          <div className="api-note"><strong>Siril star lists and WCS</strong><span>Siril detects stars and passes the list to Seiza, which returns the standard <code>.wcs</code> result and honors the requested SIP distortion order. When the source image is available, Seiza remeasures flux to make Siril’s PSF-amplitude ordering robust. See Seiza’s <a href="https://github.com/theatrus/seiza/blob/main/docs/design/solve-field-mode.md">solve-field-compatible mode contract</a> and <a href="https://siril.readthedocs.io/en/stable/astrometry/platesolving.html">Siril’s local Astrometry.net documentation</a>.</span></div>
          <div className="api-note"><strong>Server-backed applications</strong><span>Applications that want a persistent newline-delimited JSON-RPC process can run <code>seiza worker --server https://seiza.fyi</code>. That adapter uploads local image paths to this queued service; it is separate from the authenticated internal worker API used to add server compute capacity.</span></div>
        </DocSection>

        <DocSection id="native-api" eyebrow="NATIVE JSON API" title="Jobs and durable result artifacts.">
          <p>Job status is one of <code>queued</code>, <code>solving</code>, <code>succeeded</code>, or <code>failed</code>. Completed responses report end-to-end <code>solve_time_ms</code>. Successful solutions also include full TAN/ICRS WCS, optional SIP distortion, image dimensions, matched-star quality, sky footprint, artifact URLs, and durable solver <code>statistics</code> with decode, detection, and search timings.</p>
          <div className="endpoint-list">
            <Endpoint method="GET" path="/api/v1/health">Read seiza-server and Seiza versions, solver readiness, queue depth, authentication mode, and configured backends.</Endpoint>
            <Endpoint method="POST" path="/api/v1/solves">Submit a multipart image and optional solve settings. Returns <code>202</code>.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}">Poll status and retrieve the completed solution, total solve time, and solver telemetry.</Endpoint>
            <Endpoint method="POST" path="/api/v1/solves/{public_id}/resolve">Create a new queued solve with a new UUID by copying a completed solve’s retained image and applying new JSON settings. The former <code>/retry</code> path remains as a compatible alias.</Endpoint>
            <Endpoint method="POST" path="/api/v1/solves/{public_id}/validation-donation">Contribute a completed solve’s image to the long-term validation set. Requires <code>license_agreed: true</code>; <code>comment</code> and <code>solve_is_invalid</code> are optional. The route retains its historical name for API compatibility.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/annotations">Regenerate projected catalog annotations from the stored WCS.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/preview">Return a retained PNG preview. Add <code>?full=true</code> for native dimensions.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/overlay.svg">Return a self-contained composite SVG for API clients.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/wcs">Download a FITS-compatible, 80-column WCS header.</Endpoint>
          </div>
          <div className="api-note"><strong>HTTP caching and validators</strong><span>Queued and solving job JSON uses <code>no-store</code>. Completed job JSON has a short private cache; annotations cache for five minutes by catalog version; previews and composite overlays use five-minute private caches; WCS downloads are immutable. Cacheable responses include <code>ETag</code> and honor <code>If-None-Match</code>.</span></div>
          <CodeExample label="Re-solve without another upload" code={retryExample} />
          <CodeExample label="Contribute a validation image" code={contributionExample} />
          <div className="api-note"><strong>SIP WCS records</strong><span>When distortion is fitted, <code>solution.wcs.sip</code> contains the order and explicit <code>[p, q, value]</code> records for forward <code>A/B</code> and inverse <code>AP/BP</code> polynomials. The axes become <code>RA---TAN-SIP</code> / <code>DEC--TAN-SIP</code>, and the downloadable WCS includes the complete FITS SIP keyword set.</span></div>
          <div className="api-note"><strong>Solar-system motion</strong><span>Comet and asteroid annotations include the unclamped apparent speed as <code>motion_arcsec_per_hour</code>, plus sky and image direction angles. The rendered overlay uses the rate to scale a visible three-hour indicator: anti-solar for comet tails and along apparent motion for asteroid arrows.</span></div>
          <div className="api-note"><strong>Predicted satellite tracks</strong><span>When one shutter-open interval and an observer location are available, annotations include WCS-clipped <code>satellite_tracks</code> plus element age, illumination, range, elevation, trail-risk, and catalog accounting. These are orbit predictions, not pixel detections. Missing metadata or orbital data leaves the solve successful and reports a specific <code>unavailable_reasons.satellite_tracks</code> message.</span></div>
          <div className="api-note"><strong>Invalid solve reports</strong><span>Set <code>solve_is_invalid: true</code> for an incorrect WCS, a false positive, or a failed solve that should have succeeded. The flag defaults to <code>false</code> and is returned with the contribution metadata.</span></div>
          <div className="api-note"><strong>Validation image permission</strong><span>By setting <code>license_agreed</code>, the contributor attests that they own the image or have authority to contribute it. They retain ownership and give Seiza and its maintainers permission to retain, copy, and process the image as part of Seiza’s validation set, only to test, validate, debug, and improve the Seiza plate solver, including training and evaluating solver-related models. Seiza will not make the validation set public, sell the image, or use it for unrelated purposes. The recorded permission version is <code>seiza-validation-image-grant-v2</code>.</span></div>
          <h3>Annotation and overlay query parameters</h3>
          <div className="option-table">
            <OptionRow name="deep_sky, named_stars, transients, minor_bodies, satellite_tracks" defaultValue="true">Enable each installed catalog or prediction layer.</OptionRow>
            <OptionRow name="outlines" defaultValue="true">Render detailed OpenNGC contours when available; disable it to use catalog ellipses instead.</OptionRow>
            <OptionRow name="star_identifiers, field_stars, historical_transients" defaultValue="false">Enable Tycho-sidecar labels, dense field-star markers, or older transient events.</OptionRow>
            <OptionRow name="star_identifier_mag_limit / max_star_identifiers" defaultValue="10.0 / 150">Limit stellar-identifier labels by magnitude and count.</OptionRow>
            <OptionRow name="field_star_mag_limit" defaultValue="10.0">Field-star limiting magnitude, clamped from −2 through 20.</OptionRow>
            <OptionRow name="max_field_stars" defaultValue="300">Maximum field stars, clamped from 1 through 2,000.</OptionRow>
            <OptionRow name="objects, grid" defaultValue="true, false">Composite SVG controls; annotation filters above also apply.</OptionRow>
          </div>
        </DocSection>

        <DocSection id="solve-options" eyebrow="SOLVER INPUT" title="Blind by default, hinted when you know the field.">
          <p>Supply <code>center_ra_deg</code>, <code>center_dec_deg</code>, and <code>scale_arcsec_per_pixel</code> together for a hinted solve. When all three are absent, FITS uploads use compatible position and scale headers automatically; other images use blind solving.</p>
          <div className="option-table">
            <OptionRow name="center_ra_deg / center_dec_deg" defaultValue="unset">ICRS center hint in degrees; RA 0–360 and Dec −90–90.</OptionRow>
            <OptionRow name="radius_deg" defaultValue="2.0">Position-hint search radius.</OptionRow>
            <OptionRow name="scale_arcsec_per_pixel" defaultValue="unset">Pixel-scale hint required with the two center coordinates.</OptionRow>
            <OptionRow name="scale_tolerance" defaultValue="0.2">Fractional hint tolerance from 0.01 through 1.0.</OptionRow>
            <OptionRow name="min_scale_arcsec_per_pixel" defaultValue="0.1">Lower blind-solve pixel-scale bound.</OptionRow>
            <OptionRow name="max_scale_arcsec_per_pixel" defaultValue="20.0">Upper blind-solve pixel-scale bound.</OptionRow>
            <OptionRow name="sigma" defaultValue="4.0">Positive source-detection threshold.</OptionRow>
            <OptionRow name="ignore_border" defaultValue="0">Pixels ignored around every image edge.</OptionRow>
            <OptionRow name="max_stars" defaultValue="500">Bright detections retained for matching.</OptionRow>
            <OptionRow name="sip_order" defaultValue="0">SIP distortion order 2–5; 0 or 1 keeps a linear TAN solution. A fitted polynomial is accepted only when it materially improves the residual.</OptionRow>
            <OptionRow name="capture_time" defaultValue="compatible FITS time">RFC 3339 shutter-open time. FITS DATE-BEG/DATE-END are preferred; DATE-AVG, DATE-OBS, or DATE-END are normalized when a duration is present.</OptionRow>
            <OptionRow name="exposure_seconds" defaultValue="FITS XPOSURE / EXPTIME / EXPOSURE">Duration of one continuous shutter-open exposure, up to one hour. Do not send a stack integration total.</OptionRow>
            <OptionRow name="observer_latitude_deg / observer_longitude_deg" defaultValue="FITS OBSGEO or SITE">Geodetic site coordinates for topocentric satellite propagation; supply the pair together. Longitude is east-positive.</OptionRow>
            <OptionRow name="observer_altitude_m" defaultValue="0">Optional ellipsoid height used with geodetic coordinates.</OptionRow>
            <OptionRow name="observer_itrf_m" defaultValue="FITS OBSGEO-X/Y/Z">Three-element ITRF meter coordinates. Use this instead of geodetic coordinates, not alongside them.</OptionRow>
          </div>
        </DocSection>

        <DocSection id="resumable-uploads" eyebrow="TUS 1.0" title="Upload large images in resumable parallel parts.">
          <p>The web application uses the same durable TUS flow. It uploads up to three chunk-aligned parts concurrently for files of at least 64 MiB using the TUS concatenation extension; smaller files use a single stream. Session manifests and chunks survive API restarts. Local storage streams final assembly to disk, while S3 uses native multipart copies without routing the completed image back through the API process.</p>
          <div className="endpoint-list compact">
            <Endpoint method="OPTIONS" path="/api/v1/uploads">Discover TUS version, extensions, and maximum upload size.</Endpoint>
            <Endpoint method="POST" path="/api/v1/uploads">Create a normal session, a <code>partial</code> upload, or a <code>final</code> concatenation.</Endpoint>
            <Endpoint method="HEAD" path="/api/v1/uploads/{upload_id}">Read the durable <code>Upload-Offset</code>.</Endpoint>
            <Endpoint method="PATCH" path="/api/v1/uploads/{upload_id}">Append an <code>application/offset+octet-stream</code> chunk.</Endpoint>
            <Endpoint method="DELETE" path="/api/v1/uploads/{upload_id}">Terminate the unfinished session and delete its chunks.</Endpoint>
            <Endpoint method="GET" path="/api/v1/uploads/{upload_id}/result">Return the single queued job created when upload completes.</Endpoint>
          </div>
          <CodeExample label="Raw TUS sequence" code={tusExample} />
        </DocSection>

        <DocSection id="catalog-api" eyebrow="SEIZA CATALOGS" title="Search the sky without uploading an image.">
          <p>The object API reads Seiza’s extensible memory-mapped v4 catalog. Fast spatial and name queries return canonical IDs, aliases, hierarchy, source attribution, sizes, magnitudes, and predicted prominence; the detail endpoint pages in source records, properties, relations, facet selections, catalog geometries, outlines, and build provenance only for the requested object. The stellar API reads the Tycho identifier sidecar for exact TYC/HIP/catalog lookup and proper, Bayer/Flamsteed, variable, and double-star name completion.</p>
          <div className="endpoint-list compact">
            <Endpoint method="GET" path="/api/v1/catalog/objects">Cone query using required <code>ra</code>, <code>dec</code>, and <code>radius</code>.</Endpoint>
            <Endpoint method="GET" path="/api/v1/catalog/objects/search">Exact or prefix lookup across designations, aliases, common names, and stable IDs.</Endpoint>
            <Endpoint method="GET" path="/api/v1/catalog/objects/details/{canonical_id}">Retrieve source-qualified records, relations, selections, geometries, catalog capabilities, and provenance for one stable ID.</Endpoint>
            <Endpoint method="GET" path="/api/v1/catalog/stars/search">Exact TYC/HIP/name lookup or textual stellar-name prefix completion.</Endpoint>
          </div>
          <div className="option-table">
            <OptionRow name="kinds" defaultValue="all">Comma-separated kinds such as <code>galaxy</code>, <code>nebula</code>, <code>dark-nebula</code>, or <code>hii-region</code>.</OptionRow>
            <OptionRow name="max_mag / min_major_arcmin" defaultValue="unset">Magnitude and angular-size filters.</OptionRow>
            <OptionRow name="common_name_only" defaultValue="false">Require a populated popular/common name.</OptionRow>
            <OptionRow name="include_extent_overlaps" defaultValue="true">Include large objects whose extent crosses the cone boundary.</OptionRow>
            <OptionRow name="sort" defaultValue="prominence">Use <code>prominence</code>, <code>size</code>, <code>magnitude</code>, <code>distance</code>, or <code>name</code>.</OptionRow>
            <OptionRow name="limit" defaultValue="100 / 20">Cone limit up to 1,000; name-search limit up to 100.</OptionRow>
            <OptionRow name="q / prefix" defaultValue="required / false">Search text and whether it is a prefix rather than an exact normalized name.</OptionRow>
          </div>
          <CodeExample label="Catalog queries" code={catalogExample} />
        </DocSection>

        <DocSection id="account-api" eyebrow="IDENTITY API" title="Verified accounts and revocable credentials.">
          <p>Account mode supports passwordless email verification, passkey-first sign-in, multiple browser sessions, and scoped API keys. Browser mutations require the session-bound <code>X-CSRF-Token</code>; API-key requests do not use browser cookies or CSRF. A newly created API-key secret is returned once and cannot be retrieved later.</p>
          <div className="endpoint-list compact">
            <Endpoint method="POST" path="/api/v1/auth/email/start">Send a single-use email link and code.</Endpoint>
            <Endpoint method="POST" path="/api/v1/auth/email/complete">Verify the link or code and create a browser session.</Endpoint>
            <Endpoint method="POST" path="/api/v1/auth/passkeys/authentication/start">Start discoverable passkey sign-in.</Endpoint>
            <Endpoint method="POST" path="/api/v1/auth/passkeys/authentication/complete">Verify a passkey assertion and create a browser session.</Endpoint>
            <Endpoint method="POST" path="/api/v1/auth/logout">Revoke the current browser session and clear its cookie.</Endpoint>
            <Endpoint method="POST" path="/api/v1/auth/logout-all">Revoke every session for the signed-in account after recent authentication.</Endpoint>
            <Endpoint method="GET" path="/api/v1/account">Read the signed-in account, live sessions, passkeys, and API-key metadata.</Endpoint>
            <Endpoint method="GET" path="/api/v1/account/solves">List the signed-in account’s 100 most recent submitted solves.</Endpoint>
            <Endpoint method="GET" path="/api/v1/account/passkeys">List the signed-in account’s registered passkeys.</Endpoint>
            <Endpoint method="POST" path="/api/v1/account/passkeys/registration/start">Begin registering a new passkey for the signed-in account.</Endpoint>
            <Endpoint method="POST" path="/api/v1/account/passkeys/registration/complete">Verify the attestation and store the labeled passkey.</Endpoint>
            <Endpoint method="DELETE" path="/api/v1/account/passkeys/{passkey_id}">Remove a registered passkey.</Endpoint>
            <Endpoint method="GET" path="/api/v1/account/api-keys">List API-key metadata without returning secret material.</Endpoint>
            <Endpoint method="POST" path="/api/v1/account/api-keys">Create a named key with explicit <code>solve:read</code> and/or <code>solve:submit</code> scopes.</Endpoint>
            <Endpoint method="DELETE" path="/api/v1/account/api-keys/{key_id}">Immediately revoke an account API key and any Astrometry sessions created from it.</Endpoint>
            <Endpoint method="DELETE" path="/api/v1/account/sessions/{session_id}">Revoke a browser or Astrometry session.</Endpoint>
          </div>
          <div className="api-note"><strong>Recent sign-in required</strong><span>Adding or removing passkeys and API keys requires a browser session verified within the last ten minutes. Older sessions receive <code>403</code> and must sign in again first.</span></div>
          <div className="api-note"><strong>Account-level fairness</strong><span>All credentials belonging to one account submit as the same durable owner. API-key names and scopes cannot select queue priority. Anonymous public submissions use queue weight 1 and the normal queue.</span></div>
          <div className="api-note"><strong>History and result capabilities</strong><span>Signed-in browser, API-key, and Astrometry submissions appear in recent account history. Public submissions are not retroactively attached after sign-in. An unguessable result URL remains sufficient to read a result, and image/preview retention is unchanged.</span></div>
        </DocSection>

        <DocSection id="astrometry-api" eyebrow="COMPATIBILITY API" title="A practical Astrometry.net subset.">
          <p>Existing clients can use the familiar <code>request-json</code> form field. In account mode, login with an API key validates <code>solve:submit</code> and returns a persisted, expiring account session; omitting the key returns a public normal-queue session. Public and stub-key modes retain their compatibility behavior. Upload returns a numeric compatibility ID while the native queue remains UUID-based. Upload accepts <code>ul</code> and <code>ev</code> scale types with <code>degwidth</code>, <code>arcminwidth</code>, or <code>arcsecperpix</code> units.</p>
          <div className="endpoint-list compact">
            <Endpoint method="POST" path="/api/login">Create an Astrometry-style session.</Endpoint>
            <Endpoint method="POST" path="/api/upload">Submit <code>request-json</code> plus one file.</Endpoint>
            <Endpoint method="GET" path="/api/submissions/{job_id}">Poll submission and calibration linkage.</Endpoint>
            <Endpoint method="GET" path="/api/jobs/{job_id}">Read compatible job status.</Endpoint>
            <Endpoint method="GET" path="/api/jobs/{job_id}/calibration">Read RA, Dec, radius, orientation, parity, and pixel scale.</Endpoint>
            <Endpoint method="GET" path="/api/jobs/{job_id}/info">Read filename, calibration, and objects in the field.</Endpoint>
          </div>
          <CodeExample label="Login and upload" code={astrometryExample} />
          <div className="api-note"><strong>Compatibility boundary</strong><span><code>downsample_factor &gt; 1</code> is not implemented; resize before uploading. URL uploads and the remainder of the Astrometry.net API are not exposed.</span></div>
        </DocSection>

        <DocSection id="worker-api" eyebrow="OPERATORS" title="Authenticated, lease-safe remote workers.">
          <p>Use the packaged <code>seiza-server worker --server …</code> client unless you are implementing another worker runtime. Every call requires <code>Authorization: Bearer $SEIZA_WORKER_TOKEN</code>; input also requires the current lease token.</p>
          <div className="endpoint-list compact">
            <Endpoint method="POST" path="/api/v1/internal/worker/claim">Claim the next weighted-LRU job; <code>204</code> means the queue is empty.</Endpoint>
            <Endpoint method="POST" path="/api/v1/internal/worker/claim/{job_id}">Claim a specific SQS-delivered job.</Endpoint>
            <Endpoint method="GET" path="/api/v1/internal/worker/jobs/{job_id}/input">Download input with <code>X-Seiza-Lease-Token</code>.</Endpoint>
            <Endpoint method="POST" path="/api/v1/internal/worker/jobs/{job_id}/heartbeat">Extend a live lease with JSON <code>{'{"lease_token":"…"}'}</code>.</Endpoint>
            <Endpoint method="POST" path="/api/v1/internal/worker/jobs/{job_id}/complete">Submit the lease token plus either a solution or error.</Endpoint>
          </div>
        </DocSection>

        <DocSection id="errors" eyebrow="BEHAVIOR" title="Structured failures and explicit retention.">
          <p>JSON failures use a stable envelope. Admission limits return <code>429</code> with <code>Retry-After</code>; invalid requests return <code>400</code>; expired image artifacts return <code>410</code>; catalog endpoints return <code>503</code> when their corresponding catalog is not installed.</p>
          <CodeExample label="Error response" code={errorExample} />
          <div className="api-facts">
            <div><strong>100 MB</strong><span>Default complete-image limit, configurable by the operator.</span></div>
            <div><strong>24 hours</strong><span>Default original and preview retention. WCS and job metadata persist.</span></div>
            <div><strong>Three auth modes</strong><span>Native requests support public, legacy stub-key, or verified account credentials as configured by the operator.</span></div>
          </div>
        </DocSection>
      </div>
    </div>
  </main>
}

function DocSection({ id, eyebrow, title, children }: { id: string; eyebrow: string; title: string; children: ReactNode }) {
  return <section className="api-section" id={id}>
    <p className="eyebrow">{eyebrow}</p>
    <h2>{title}</h2>
    {children}
  </section>
}

function Endpoint({ method, path, children }: { method: string; path: string; children: ReactNode }) {
  return <div className="endpoint-row">
    <div className="endpoint-signature"><span className="http-method" data-method={method}>{method}</span><code>{path}</code></div>
    <p>{children}</p>
  </div>
}

function OptionRow({ name, defaultValue, children }: { name: string; defaultValue: string; children: ReactNode }) {
  return <div className="option-row">
    <code>{name}</code>
    <p>{children}</p>
    <span>{defaultValue}</span>
  </div>
}

function CodeExample({ label, code }: { label: string; code: string }) {
  const [copyState, setCopyState] = useState<'idle' | 'copied' | 'selected'>('idle')
  const codeRef = useRef<HTMLElement>(null)
  const copy = async () => {
    const nextState = await copyText(code, codeRef.current)
    setCopyState(nextState)
    window.setTimeout(() => setCopyState('idle'), 1_500)
  }
  return <figure className="code-example">
    <figcaption><span>{label}</span><button type="button" data-copy-example onClick={() => void copy()}>{copyState === 'idle' ? 'Copy' : copyState === 'copied' ? 'Copied' : 'Selected'}</button></figcaption>
    <pre><code ref={codeRef}>{code}</code></pre>
  </figure>
}

async function copyText(value: string, fallbackNode: HTMLElement | null): Promise<'copied' | 'selected'> {
  try {
    if (window.location.protocol === 'https:' && navigator.clipboard) {
      await navigator.clipboard.writeText(value)
      return 'copied'
    }
  } catch {
    // Plain-HTTP self-hosts may expose Clipboard but reject write access.
  }
  const textarea = document.createElement('textarea')
  textarea.value = value
  textarea.setAttribute('readonly', '')
  textarea.style.position = 'fixed'
  textarea.style.opacity = '0'
  document.body.append(textarea)
  textarea.select()
  const copied = document.execCommand('copy')
  textarea.remove()
  if (copied) return 'copied'
  if (fallbackNode) {
    const selection = window.getSelection()
    const range = document.createRange()
    range.selectNodeContents(fallbackNode)
    selection?.removeAllRanges()
    selection?.addRange(range)
  }
  return 'selected'
}
