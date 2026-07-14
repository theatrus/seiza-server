import { FormEvent, ReactNode, useEffect, useRef, useState } from 'react'
import { Annotations, Job, OverlayObject, SolveOptions, getAnnotations, getSolve, submitSolve } from './api'
import { AstroOverlay, OverlayControls } from './AstroOverlay'
import type { OverlayLayers } from './AstroOverlay'

const pending = new Set(['queued', 'solving'])
const defaultOverlayLayers: OverlayLayers = {
  deepSky: true,
  namedStars: true,
  fieldStars: false,
  transients: true,
  minorBodies: true,
  historicalTransients: false,
  grid: true,
}

function numberOrUndefined(value: FormDataEntryValue | null): number | undefined {
  if (typeof value !== 'string' || value.trim() === '') return undefined
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : undefined
}

function navigate(path: string) {
  window.history.pushState({}, '', path)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

function Link({ to, children, className }: { to: string; children: ReactNode; className?: string }) {
  return <a href={to} className={className} onClick={(event) => {
    if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey) {
      event.preventDefault()
      navigate(to)
    }
  }}>{children}</a>
}

export default function App() {
  const [path, setPath] = useState(window.location.pathname)
  useEffect(() => {
    const updatePath = () => setPath(window.location.pathname)
    window.addEventListener('popstate', updatePath)
    return () => window.removeEventListener('popstate', updatePath)
  }, [])
  const solutionMatch = path.match(/^\/solutions\/(\d+-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/)

  return <div className="site-shell">
    <SiteHeader />
    {path === '/' && <HomePage />}
    {path === '/solve' && <SolvePage />}
    {solutionMatch && <SolutionPage jobId={solutionMatch[1]} />}
    {path !== '/' && path !== '/solve' && !solutionMatch && <NotFoundPage />}
    <SiteFooter />
  </div>
}

function SiteHeader() {
  return <nav className="site-nav" aria-label="Primary navigation">
    <Link to="/" className="brand-link">
      <img src="/seiza-mark.png" alt="" width="38" height="38" />
      <span><strong>Seiza</strong><small>星座 · せいざ</small></span>
    </Link>
    <div className="nav-links">
      <Link to="/">About</Link>
      <Link to="/solve" className="button small">Solve an image</Link>
    </div>
  </nav>
}

function HomePage() {
  return <main>
    <section className="hero">
      <div>
        <p className="eyebrow">OPEN-SOURCE ASTROMETRY</p>
        <h1>Find exactly where your image meets the sky.</h1>
        <p className="intro">Seiza is a fast plate-solving library written in Rust. It recognizes star patterns, determines an image’s celestial coordinates, and returns a complete, standards-friendly WCS solution.</p>
        <div className="actions">
          <Link to="/solve" className="button">Solve an image</Link>
          <a href="https://github.com/theatrus/seiza">Explore the source <span aria-hidden="true">↗</span></a>
        </div>
      </div>
      <div className="hero-mark" aria-hidden="true">
        <img src="/seiza-mark.png" alt="" />
        <span className="kanji">星座</span>
        <span className="hiragana">せいざ · constellation</span>
      </div>
    </section>

    <section className="story-grid" aria-labelledby="how-it-works">
      <div>
        <p className="eyebrow">HOW IT WORKS</p>
        <h2 id="how-it-works">Geometry, not guesswork.</h2>
      </div>
      <ol className="steps">
        <li><span>01</span><div><strong>Detect</strong><p>Seiza finds reliable star centroids in FITS and common image formats.</p></div></li>
        <li><span>02</span><div><strong>Match</strong><p>Geometric star patterns are compared with a compact sky catalog, blind or with an optional hint.</p></div></li>
        <li><span>03</span><div><strong>Calibrate</strong><p>A tangent-plane WCS maps every pixel to ICRS sky coordinates and enables a catalog overlay.</p></div></li>
      </ol>
    </section>

    <section className="about-card">
      <div>
        <p className="eyebrow">ABOUT SEIZA</p>
        <h2>A small, inspectable engine for astronomical software.</h2>
      </div>
      <div className="about-copy">
        <p>Seiza (星座, せいざ) is Japanese for “constellation.” The library and this service were created by <strong>Yann Ramin</strong> to provide a modern, embeddable plate solver for observatories, imaging tools, and curious skywatchers.</p>
        <p>The project is released under the Apache License 2.0. Use the Rust crate directly, integrate the HTTP API, or run this server on your own infrastructure.</p>
        <div className="text-links">
          <a href="https://crates.io/crates/seiza">seiza on crates.io <span aria-hidden="true">↗</span></a>
          <a href="https://github.com/theatrus/seiza-server">server on GitHub <span aria-hidden="true">↗</span></a>
        </div>
      </div>
    </section>
  </main>
}

function SolvePage() {
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    const file = form.get('file')
    if (!(file instanceof File) || file.size === 0) {
      setError('Choose an image to solve.')
      return
    }
    const ra = numberOrUndefined(form.get('center_ra_deg'))
    const dec = numberOrUndefined(form.get('center_dec_deg'))
    const scale = numberOrUndefined(form.get('scale_arcsec_per_pixel'))
    if ([ra, dec, scale].some((value) => value !== undefined) && [ra, dec, scale].some((value) => value === undefined)) {
      setError('A hinted solve needs RA, Dec, and pixel scale together.')
      return
    }
    const options: SolveOptions = {
      center_ra_deg: ra,
      center_dec_deg: dec,
      scale_arcsec_per_pixel: scale,
      radius_deg: numberOrUndefined(form.get('radius_deg')),
      scale_tolerance: numberOrUndefined(form.get('scale_tolerance')),
      min_scale_arcsec_per_pixel: numberOrUndefined(form.get('min_scale')),
      max_scale_arcsec_per_pixel: numberOrUndefined(form.get('max_scale')),
    }
    const captureTime = form.get('capture_time')
    if (typeof captureTime === 'string' && captureTime !== '') {
      const parsed = new Date(captureTime)
      if (Number.isNaN(parsed.getTime())) {
        setError('Capture time is not a valid date and time.')
        return
      }
      options.capture_time = parsed.toISOString()
    }
    setSubmitting(true)
    setError(null)
    try {
      const job = await submitSolve(file, options)
      navigate(`/solutions/${job.id}`)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Upload failed')
      setSubmitting(false)
    }
  }

  return <main className="narrow-page">
    <header className="page-heading">
      <p className="eyebrow">PLATE SOLVER</p>
      <h1>Queue a new image.</h1>
      <p className="intro">Your solve runs in a background worker. The result gets its own durable, unguessable URL; the uploaded image and preview are automatically deleted after about one day.</p>
    </header>
    <section className="panel">
      <form onSubmit={onSubmit}>
        <label className="file-input"><span>FITS or image file</span><input name="file" type="file" accept=".fits,.fit,.fts,image/png,image/jpeg,image/tiff,image/webp" required /></label>
        <fieldset>
          <legend>Optional position hint</legend>
          <p>Leave all three blank for a blind solve. Hints make narrow fields faster and more reliable.</p>
          <div className="form-grid">
            <label>RA (degrees)<input name="center_ra_deg" type="number" min="0" max="360" step="any" placeholder="210.802" /></label>
            <label>Dec (degrees)<input name="center_dec_deg" type="number" min="-90" max="90" step="any" placeholder="54.349" /></label>
            <label>Pixel scale (arcsec/px)<input name="scale_arcsec_per_pixel" type="number" min="0.01" step="any" placeholder="1.24" /></label>
            <label>Search radius (degrees)<input name="radius_deg" type="number" min="0.1" step="any" placeholder="2" /></label>
          </div>
        </fieldset>
        <fieldset>
          <legend>Acquisition time</legend>
          <p>FITS DATE-OBS is used automatically. For other images, provide the local capture time to position comets and asteroids and scope transient events.</p>
          <label>Capture time<input name="capture_time" type="datetime-local" step="1" /></label>
        </fieldset>
        <details>
          <summary>Blind solve settings</summary>
          <div className="form-grid">
            <label>Minimum scale (arcsec/px)<input name="min_scale" type="number" min="0.01" step="any" placeholder="0.3" /></label>
            <label>Maximum scale (arcsec/px)<input name="max_scale" type="number" min="0.01" step="any" placeholder="20" /></label>
            <label>Hint scale tolerance<input name="scale_tolerance" type="number" min="0.01" max="1" step="0.01" placeholder="0.2" /></label>
          </div>
        </details>
        <button className="button" disabled={submitting}>{submitting ? 'Queueing…' : 'Queue solve'}</button>
      </form>
    </section>
    {error && <p className="error" role="alert">{error}</p>}
  </main>
}

function SolutionPage({ jobId }: { jobId: string }) {
  const [job, setJob] = useState<Job | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    let active = true
    let timer: number | undefined
    const refresh = async () => {
      try {
        const next = await getSolve(jobId)
        if (!active) return
        setJob(next)
        setError(null)
        if (pending.has(next.status)) timer = window.setTimeout(refresh, 1_500)
      } catch (reason) {
        if (active) setError(reason instanceof Error ? reason.message : String(reason))
      }
    }
    void refresh()
    return () => { active = false; if (timer) window.clearTimeout(timer) }
  }, [jobId])

  return <main className="solution-page">
    <header className="solution-heading">
      <div><p className="eyebrow">SOLUTION</p><h1>{job ? titleForStatus(job.status) : 'Loading solution…'}</h1></div>
      {job && <span className={`status ${job.status}`}>{job.status}</span>}
    </header>
    {error && <p className="error" role="alert">{error}</p>}
    {job && <SolutionContent job={job} />}
  </main>
}

function titleForStatus(status: Job['status']) {
  if (status === 'queued') return 'Waiting in the queue.'
  if (status === 'solving') return 'Reading the stars.'
  if (status === 'failed') return 'The solve did not converge.'
  return 'The field is solved.'
}

function SolutionContent({ job }: { job: Job }) {
  const [annotations, setAnnotations] = useState<Annotations | null>(null)
  const [annotationError, setAnnotationError] = useState<string | null>(null)
  const [layers, setLayers] = useState(defaultOverlayLayers)
  const [expanded, setExpanded] = useState(false)
  const [downloading, setDownloading] = useState(false)
  const [exportError, setExportError] = useState<string | null>(null)
  const frameRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    let active = true
    if (!job.annotations_url) {
      return () => { active = false }
    }
    getAnnotations(job.annotations_url)
      .then((result) => {
        if (active) {
          setAnnotations(result)
          setAnnotationError(null)
        }
      })
      .catch((reason) => {
        if (active) setAnnotationError(reason instanceof Error ? reason.message : String(reason))
      })
    return () => { active = false }
  }, [job.annotations_url])
  const solution = job.solution
  const currentAnnotations = annotations?.job_id === job.id ? annotations : null
  const overlayObjects = currentAnnotations?.objects ?? solution?.objects ?? []
  const overlayCounts = currentAnnotations?.counts ?? countObjects(overlayObjects)
  const downloadPng = async () => {
    if (!job.preview_url || !solution || !frameRef.current) return
    setDownloading(true)
    setExportError(null)
    try {
      await downloadRenderedPng(job.preview_url, frameRef.current, solution, job.id)
    } catch (reason) {
      setExportError(reason instanceof Error ? reason.message : String(reason))
    } finally {
      setDownloading(false)
    }
  }
  return <>
    <section className="job-meta">
      <div><span>File</span><strong>{job.original_filename}</strong></div>
      <div><span>Submitted</span><strong>{new Date(job.created_at).toLocaleString()}</strong></div>
      <div><span>Image retention</span><strong>{job.input_available ? `until ${new Date(job.input_expires_at).toLocaleString()}` : 'expired and deleted'}</strong></div>
    </section>
    {job.error && <p className="error">{job.error}</p>}
    {pending.has(job.status) && <section className="panel waiting"><div className="orbit" aria-hidden="true"><span /></div><p>This durable page refreshes automatically. You can bookmark it or come back later.</p></section>}
    {solution && <>
      {job.preview_url ? <section className="overlay-card">
        <div className="section-heading"><div><p className="eyebrow">SKY OVERLAY</p><h2>Explore the solved field</h2></div><div className="overlay-actions"><button className="button small secondary" type="button" onClick={() => setExpanded(true)}>Expand image</button><button className="button small" type="button" disabled={downloading} onClick={() => void downloadPng()}>{downloading ? 'Rendering…' : 'Download rendered PNG'}</button></div></div>
        <OverlayControls layers={layers} counts={overlayCounts} onChange={setLayers} />
        {annotationError && <p className="overlay-warning">Live catalogs could not be refreshed: {annotationError}</p>}
        {exportError && <p className="overlay-warning">PNG rendering failed: {exportError}</p>}
        <div className={`image-stage${expanded ? ' expanded' : ''}`} role={expanded ? 'dialog' : undefined} aria-modal={expanded || undefined} aria-label={expanded ? 'Expanded astronomical image overlay' : undefined}>
          {expanded && <button className="overlay-close" type="button" onClick={() => setExpanded(false)}>Close</button>}
          <div className="sky-frame" ref={frameRef}>
            <img src={job.preview_url} alt="Uploaded astronomical image" />
            <AstroOverlay solution={solution} objects={overlayObjects} layers={layers} />
          </div>
        </div>
        <p className="retention-note">The SVG annotations are rendered interactively over the image. The temporary image expires after one day; WCS and catalog metadata remain available.</p>
      </section> : !job.input_available && <p className="expired-note">The uploaded image and visual overlay have been deleted after their one-day retention period. The complete WCS solution remains below.</p>}
      <section className="metric-grid">
        <Metric label="Center RA" value={`${solution.center_ra_deg.toFixed(8)}°`} />
        <Metric label="Center Dec" value={`${solution.center_dec_deg.toFixed(8)}°`} />
        <Metric label="Pixel scale" value={`${solution.pixel_scale_arcsec_per_pixel.toFixed(5)}″/px`} />
        <Metric label="Fit quality" value={`${solution.matched_stars} stars · ${solution.rms_arcsec.toFixed(4)}″ RMS`} />
      </section>
      <WcsDetails job={job} />
    </>}
  </>
}

async function downloadRenderedPng(previewUrl: string, frame: HTMLDivElement, solution: NonNullable<Job['solution']>, jobId: string) {
  const separator = previewUrl.includes('?') ? '&' : '?'
  const response = await fetch(`${previewUrl}${separator}full=true`)
  if (!response.ok) throw new Error(`full-resolution image request failed (${response.status})`)
  const sourceUrl = URL.createObjectURL(await response.blob())
  const overlay = frame.querySelector('svg')
  if (!overlay) {
    URL.revokeObjectURL(sourceUrl)
    throw new Error('the overlay is not ready')
  }
  const serialized = overlay.cloneNode(true) as SVGSVGElement
  serialized.setAttribute('xmlns', 'http://www.w3.org/2000/svg')
  serialized.setAttribute('width', String(solution.image_width))
  serialized.setAttribute('height', String(solution.image_height))
  const overlayUrl = URL.createObjectURL(new Blob(
    [new XMLSerializer().serializeToString(serialized)],
    { type: 'image/svg+xml;charset=utf-8' },
  ))
  try {
    const [sourceImage, overlayImage] = await Promise.all([loadImage(sourceUrl), loadImage(overlayUrl)])
    const canvas = document.createElement('canvas')
    canvas.width = solution.image_width
    canvas.height = solution.image_height
    const context = canvas.getContext('2d')
    if (!context) throw new Error('this browser does not provide a 2D canvas')
    context.drawImage(sourceImage, 0, 0, canvas.width, canvas.height)
    context.drawImage(overlayImage, 0, 0, canvas.width, canvas.height)
    const png = await new Promise<Blob>((resolve, reject) => canvas.toBlob(
      (value) => value ? resolve(value) : reject(new Error('the browser could not encode the PNG')),
      'image/png',
    ))
    const downloadUrl = URL.createObjectURL(png)
    const link = document.createElement('a')
    link.href = downloadUrl
    link.download = `seiza-solution-${jobId}.png`
    link.click()
    window.setTimeout(() => URL.revokeObjectURL(downloadUrl), 0)
  } finally {
    URL.revokeObjectURL(sourceUrl)
    URL.revokeObjectURL(overlayUrl)
  }
}

function loadImage(url: string) {
  return new Promise<HTMLImageElement>((resolve, reject) => {
    const image = new Image()
    image.onload = () => resolve(image)
    image.onerror = () => reject(new Error('the browser could not decode a rendered image layer'))
    image.src = url
  })
}

function countObjects(objects: OverlayObject[]) {
  const counts: Record<string, number> = {}
  for (const object of objects) {
    const layer = object.kind === 'field-star'
      ? 'field_stars'
      : object.kind === 'star' || object.kind === 'double-star'
        ? 'named_stars'
        : object.kind === 'transient'
          ? 'transients'
          : object.kind === 'comet' || object.kind === 'asteroid'
            ? 'minor_bodies'
            : 'deep_sky'
    counts[layer] = (counts[layer] ?? 0) + 1
  }
  counts.historical_transients = objects.filter((object) => object.kind === 'transient' && object.near_capture === false).length
  return counts
}

function WcsDetails({ job }: { job: Job }) {
  const solution = job.solution!
  const wcs = solution.wcs
  return <section className="wcs-card">
    <div className="section-heading"><div><p className="eyebrow">WORLD COORDINATE SYSTEM</p><h2>Complete WCS calibration</h2></div>{job.wcs_url && <a className="button small" href={job.wcs_url}>Download .wcs</a>}</div>
    <div className="wcs-grid">
      <DataPair label="Projection" value={`${wcs.ctype[0]} / ${wcs.ctype[1]}`} />
      <DataPair label="Reference frame" value={`${wcs.radesys} · equinox ${wcs.equinox.toFixed(1)}`} />
      <DataPair label="CRVAL" value={`${format(wcs.crval[0])}, ${format(wcs.crval[1])} deg`} />
      <DataPair label="CRPIX (zero-indexed)" value={`${format(wcs.crpix[0])}, ${format(wcs.crpix[1])} px`} />
      <DataPair label="Image dimensions" value={`${solution.image_width} × ${solution.image_height} px`} />
      <DataPair label="Units" value={`${wcs.cunit[0]} / ${wcs.cunit[1]}`} />
      <DataPair label="Capture time" value={solution.capture_time ? new Date(solution.capture_time).toLocaleString() : 'Not recorded'} />
      <DataPair label="Annotation catalog" value={solution.catalog_version ?? 'Not configured'} />
    </div>
    <div className="matrix-wrap">
      <h3>CD matrix <small>degrees per pixel</small></h3>
      <code>{formatScientific(wcs.cd[0][0])} &nbsp; {formatScientific(wcs.cd[0][1])}<br />{formatScientific(wcs.cd[1][0])} &nbsp; {formatScientific(wcs.cd[1][1])}</code>
    </div>
    <div className="footprint-wrap">
      <h3>ICRS footprint <small>RA, Dec in degrees</small></h3>
      <ol>{solution.footprint.map(([ra, dec], index) => <li key={index}><span>Corner {index + 1}</span><code>{format(ra)}, {format(dec)}</code></li>)}</ol>
    </div>
    {solution.objects.length > 0 && <details className="object-list"><summary>{solution.objects.length} catalog objects in field</summary><ul>{solution.objects.map((object, index) => <li key={`${object.name}-${index}`}><strong>{object.common_name || object.name}</strong><span>{object.name} · {object.kind}{object.mag == null ? '' : ` · mag ${object.mag.toFixed(1)}`}</span></li>)}</ul></details>}
  </section>
}

function format(value: number) { return value.toFixed(10) }
function formatScientific(value: number) { return value.toExponential(12) }
function Metric({ label, value }: { label: string; value: string }) { return <div><span>{label}</span><strong>{value}</strong></div> }
function DataPair({ label, value }: { label: string; value: string }) { return <div><dt>{label}</dt><dd>{value}</dd></div> }

function NotFoundPage() {
  return <main className="narrow-page"><section className="empty-state"><p className="eyebrow">404</p><h1>This point is off the chart.</h1><p className="intro">The page does not exist, but the solver is ready for another field.</p><Link to="/solve" className="button">Solve an image</Link></section></main>
}

function SiteFooter() {
  return <footer><span>Seiza · 星座 · せいざ</span><span>Apache-2.0 · Built by Yann Ramin</span></footer>
}
