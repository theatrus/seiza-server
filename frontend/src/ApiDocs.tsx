import { ReactNode, useRef, useState } from 'react'

const multipartExample = `curl -X POST https://seiza.fyi/api/v1/solves \\
  -F 'file=@M31.fits' \\
  -F 'options={"min_scale_arcsec_per_pixel":0.1,"max_scale_arcsec_per_pixel":20}'`

const pollExample = `PUBLIC_ID='1-550e8400-e29b-41d4-a716-446655440000'
curl "https://seiza.fyi/api/v1/solves/$PUBLIC_ID"`

const catalogExample = `# Objects in a three-degree cone around M31
curl 'https://seiza.fyi/api/v1/catalog/objects?ra=10.6848&dec=41.2691&radius=3&kinds=galaxy,nebula&max_mag=14&sort=prominence&limit=100'

# Exact designation or stable-ID lookup
curl 'https://seiza.fyi/api/v1/catalog/objects/search?q=M31'

# Prefix search across names, aliases, and stable IDs
curl 'https://seiza.fyi/api/v1/catalog/objects/search?q=ced&prefix=true&limit=20'`

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
        <p className="intro">Submit an image, leave the solve in the durable queue, and poll an opaque result URL for WCS calibration and catalog metadata. The native API is JSON-first; an Astrometry.net-compatible subset is available for existing clients.</p>
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
        <a href="#native-api">Native API</a>
        <a href="#solve-options">Solve options</a>
        <a href="#resumable-uploads">Large uploads</a>
        <a href="#catalog-api">Catalog API</a>
        <a href="#astrometry-api">Astrometry compatibility</a>
        <a href="#worker-api">Worker API</a>
        <a href="#errors">Errors and limits</a>
      </aside>

      <div className="api-content">
        <DocSection id="quick-start" eyebrow="FIRST SOLVE" title="Multipart upload, then poll.">
          <p>The simplest client sends one <code>file</code> part and an optional JSON <code>options</code> part. The server responds with <code>202 Accepted</code>; CPU-heavy solving happens later in a worker.</p>
          <CodeExample label="Submit an image" code={multipartExample} />
          <CodeExample label="Poll the opaque result URL" code={pollExample} />
          <div className="api-note"><strong>Authentication modes</strong><span>Public installations need no credential. When stub-key mode is enabled, add <code>X-API-Key: …</code> or <code>Authorization: Bearer …</code> to submission and TUS requests.</span></div>
          <div className="api-note"><strong>Result URLs are capabilities.</strong><span>The numeric queue sequence is not sufficient. Preserve the entire <code>id</code>, including its random UUID.</span></div>
        </DocSection>

        <DocSection id="native-api" eyebrow="NATIVE JSON API" title="Jobs and durable result artifacts.">
          <p>Job status is one of <code>queued</code>, <code>solving</code>, <code>succeeded</code>, or <code>failed</code>. Successful responses include full TAN/ICRS WCS, image dimensions, matched-star quality, sky footprint, and artifact URLs.</p>
          <div className="endpoint-list">
            <Endpoint method="GET" path="/api/v1/health">Read solver readiness, queue depth, authentication mode, and configured backends.</Endpoint>
            <Endpoint method="POST" path="/api/v1/solves">Submit a multipart image and optional solve settings. Returns <code>202</code>.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}">Poll status and retrieve the completed solution.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/annotations">Regenerate projected catalog annotations from the stored WCS.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/preview">Return a retained PNG preview. Add <code>?full=true</code> for native dimensions.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/overlay.svg">Return a self-contained composite SVG for API clients.</Endpoint>
            <Endpoint method="GET" path="/api/v1/solves/{public_id}/wcs">Download a FITS-compatible, 80-column WCS header.</Endpoint>
          </div>
          <h3>Annotation and overlay query parameters</h3>
          <div className="option-table">
            <OptionRow name="deep_sky, named_stars, transients, minor_bodies" defaultValue="true">Enable each installed catalog layer.</OptionRow>
            <OptionRow name="field_stars, historical_transients" defaultValue="false">Enable dense field-star markers or older transient events.</OptionRow>
            <OptionRow name="field_star_mag_limit" defaultValue="10.0">Field-star limiting magnitude, clamped from −2 through 20.</OptionRow>
            <OptionRow name="max_field_stars" defaultValue="300">Maximum field stars, clamped from 1 through 2,000.</OptionRow>
            <OptionRow name="objects, grid" defaultValue="true, false">Composite SVG controls; annotation filters above also apply.</OptionRow>
          </div>
        </DocSection>

        <DocSection id="solve-options" eyebrow="SOLVER INPUT" title="Blind by default, hinted when you know the field.">
          <p>Supply <code>center_ra_deg</code>, <code>center_dec_deg</code>, and <code>scale_arcsec_per_pixel</code> together for a hinted solve. Leave all three absent for blind solving.</p>
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
            <OptionRow name="capture_time" defaultValue="FITS DATE-OBS">RFC 3339 acquisition time for transients, comets, and asteroids.</OptionRow>
          </div>
        </DocSection>

        <DocSection id="resumable-uploads" eyebrow="TUS 1.0" title="Chunk large images and resume interrupted transfers.">
          <p>The web application uses the same durable TUS flow. Create a session, upload chunks with exact offsets, and read its <code>/result</code> after the declared length is complete. Session manifests and chunks survive API restarts in local or S3 storage.</p>
          <div className="endpoint-list compact">
            <Endpoint method="OPTIONS" path="/api/v1/uploads">Discover TUS version, extensions, and maximum upload size.</Endpoint>
            <Endpoint method="POST" path="/api/v1/uploads">Create a session using <code>Upload-Length</code> and <code>Upload-Metadata</code>.</Endpoint>
            <Endpoint method="HEAD" path="/api/v1/uploads/{upload_id}">Read the durable <code>Upload-Offset</code>.</Endpoint>
            <Endpoint method="PATCH" path="/api/v1/uploads/{upload_id}">Append an <code>application/offset+octet-stream</code> chunk.</Endpoint>
            <Endpoint method="DELETE" path="/api/v1/uploads/{upload_id}">Terminate the unfinished session and delete its chunks.</Endpoint>
            <Endpoint method="GET" path="/api/v1/uploads/{upload_id}/result">Return the single queued job created when upload completes.</Endpoint>
          </div>
          <CodeExample label="Raw TUS sequence" code={tusExample} />
        </DocSection>

        <DocSection id="catalog-api" eyebrow="SEIZA 0.4.1 CATALOGS" title="Search the sky without uploading an image.">
          <p>The object API reads Seiza’s memory-mapped v3 catalog and returns stable IDs, aliases, hierarchy, source provenance, sizes, magnitudes, and predicted prominence. It also remains compatible with legacy v1 files, whose provenance fields are empty.</p>
          <div className="endpoint-list compact">
            <Endpoint method="GET" path="/api/v1/catalog/objects">Cone query using required <code>ra</code>, <code>dec</code>, and <code>radius</code>.</Endpoint>
            <Endpoint method="GET" path="/api/v1/catalog/objects/search">Exact or prefix lookup across designations, aliases, common names, and stable IDs.</Endpoint>
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

        <DocSection id="astrometry-api" eyebrow="COMPATIBILITY API" title="A practical Astrometry.net subset.">
          <p>Existing clients can use the familiar <code>request-json</code> form field. Login returns an opaque stub session; upload accepts <code>ul</code> and <code>ev</code> scale types with <code>degwidth</code>, <code>arcminwidth</code>, or <code>arcsecperpix</code> units.</p>
          <div className="endpoint-list compact">
            <Endpoint method="POST" path="/api/login">Create an Astrometry-style session.</Endpoint>
            <Endpoint method="POST" path="/api/upload">Submit <code>request-json</code> plus one file.</Endpoint>
            <Endpoint method="GET" path="/api/submissions/{job_id}">Poll submission and calibration linkage.</Endpoint>
            <Endpoint method="GET" path="/api/jobs/{job_id}">Read compatible job status.</Endpoint>
            <Endpoint method="GET" path="/api/jobs/{job_id}/calibration">Read RA, Dec, radius, orientation, parity, and pixel scale.</Endpoint>
            <Endpoint method="GET" path="/api/jobs/{job_id}/info">Read filename, calibration, and objects in the field.</Endpoint>
          </div>
          <CodeExample label="Login and upload" code={astrometryExample} />
          <div className="api-note"><strong>Compatibility boundary</strong><span><code>downsample_factor &gt; 1</code> is not implemented; resize before uploading. The session and API-key store is intentionally a stub today.</span></div>
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
          <p>JSON failures use a stable envelope. Admission limits return <code>429</code> with <code>Retry-After</code>; invalid requests return <code>400</code>; expired image artifacts return <code>410</code>; catalog endpoints return <code>503</code> when no object catalog is installed.</p>
          <CodeExample label="Error response" code={errorExample} />
          <div className="api-facts">
            <div><strong>100 MB</strong><span>Default complete-image limit, configurable by the operator.</span></div>
            <div><strong>24 hours</strong><span>Default original and preview retention. WCS and job metadata persist.</span></div>
            <div><strong>Public or stub key</strong><span>Native submissions accept <code>X-API-Key</code> or a Bearer token when key mode is enabled.</span></div>
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
