import { FormEvent, ReactNode, useEffect, useRef, useState } from 'react'
import { downloadBlob, renderOverlayPng } from '@seiza/astro-overlay/export'
import { Annotations, Health, Job, OverlayObject, SolveOptions, donateValidationImage, getAnnotations, getHealth, getSolve, retrySolve, submitSolve } from './api'
import { ApiDocsPage } from './ApiDocs'
import { AstroOverlay, OverlayControls } from './AstroOverlay'
import type { OverlayLayers } from './AstroOverlay'
import type { DeepSkyCatalogId } from './catalogs'

const pending = new Set(['queued', 'solving'])
const defaultOverlayLayers: OverlayLayers = {
  deepSky: true,
  namedStars: true,
  starIdentifiers: false,
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

function solveOptionsFromForm(form: FormData, defaults?: SolveOptions): SolveOptions {
  const ra = numberOrUndefined(form.get('center_ra_deg'))
  const dec = numberOrUndefined(form.get('center_dec_deg'))
  const scale = numberOrUndefined(form.get('scale_arcsec_per_pixel'))
  if ([ra, dec, scale].some((value) => value !== undefined) && [ra, dec, scale].some((value) => value === undefined)) {
    throw new Error('A hinted solve needs RA, Dec, and pixel scale together. Leave all three blank for a blind solve.')
  }
  const options: SolveOptions = {
    center_ra_deg: ra,
    center_dec_deg: dec,
    scale_arcsec_per_pixel: scale,
    radius_deg: numberOrUndefined(form.get('radius_deg')),
    scale_tolerance: numberOrUndefined(form.get('scale_tolerance')),
    min_scale_arcsec_per_pixel: numberOrUndefined(form.get('min_scale')),
    max_scale_arcsec_per_pixel: numberOrUndefined(form.get('max_scale')),
    sigma: defaults?.sigma,
    ignore_border: defaults?.ignore_border,
    max_stars: defaults?.max_stars,
  }
  const captureTime = form.get('capture_time')
  if (typeof captureTime === 'string' && captureTime !== '') {
    const parsed = new Date(captureTime)
    if (Number.isNaN(parsed.getTime())) throw new Error('Acquisition time is not a valid date and time.')
    options.capture_time = parsed.toISOString()
  }
  return options
}

function localDateTimeValue(value?: string | null) {
  if (!value) return ''
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 19)
}

function SolveOptionsFields({ defaults }: { defaults?: SolveOptions }) {
  return <>
    <fieldset className="optional-fields">
      <legend>Position and scale <span className="optional-badge">Optional</span></legend>
      <p><strong>No coordinates are required.</strong> Compatible FITS headers supply position and scale automatically; other images solve blind. If you add a position hint, provide all three values.</p>
      <div className="form-grid">
        <label>RA (degrees)<input name="center_ra_deg" type="number" min="0" max="360" step="any" placeholder="Optional · 210.802" defaultValue={defaults?.center_ra_deg ?? ''} /></label>
        <label>Dec (degrees)<input name="center_dec_deg" type="number" min="-90" max="90" step="any" placeholder="Optional · 54.349" defaultValue={defaults?.center_dec_deg ?? ''} /></label>
        <label>Pixel scale (arcsec/px)<input name="scale_arcsec_per_pixel" type="number" min="0.01" step="any" placeholder="Optional · 1.24" defaultValue={defaults?.scale_arcsec_per_pixel ?? ''} /></label>
        <label>Search radius (degrees)<input name="radius_deg" type="number" min="0.1" step="any" placeholder="Optional · 2" defaultValue={defaults?.radius_deg ?? ''} /></label>
      </div>
    </fieldset>
    <fieldset className="optional-fields">
      <legend>Acquisition time <span className="optional-badge">Optional</span></legend>
      <p><strong>FITS DATE-OBS is used automatically.</strong> For JPEG, PNG, and other images, add the capture time when known so Seiza can position comets and asteroids and scope transient events.</p>
      <label>Acquisition time<input name="capture_time" type="datetime-local" step="1" defaultValue={localDateTimeValue(defaults?.capture_time)} /></label>
    </fieldset>
    <details>
      <summary>Advanced blind-solve limits <span className="optional-badge">Optional</span></summary>
      <div className="form-grid">
        <label>Minimum scale (arcsec/px)<input name="min_scale" type="number" min="0.01" step="any" placeholder="0.1" defaultValue={defaults?.min_scale_arcsec_per_pixel ?? ''} /></label>
        <label>Maximum scale (arcsec/px)<input name="max_scale" type="number" min="0.01" step="any" placeholder="20" defaultValue={defaults?.max_scale_arcsec_per_pixel ?? ''} /></label>
        <label>Hint scale tolerance<input name="scale_tolerance" type="number" min="0.01" max="1" step="0.01" placeholder="0.2" defaultValue={defaults?.scale_tolerance ?? ''} /></label>
      </div>
    </details>
  </>
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
  const solutionMatch = path.match(/^\/solutions\/((?:\d+-)?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/)

  return <div className="site-shell">
    <SiteHeader />
    {path === '/' && <HomePage />}
    {path === '/solve' && <SolvePage />}
    {path === '/docs/api' && <ApiDocsPage />}
    {solutionMatch && <SolutionPage jobId={solutionMatch[1]} />}
    {path !== '/' && path !== '/solve' && path !== '/docs/api' && !solutionMatch && <NotFoundPage />}
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
      <Link to="/docs/api">API</Link>
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

    <section className="about-card integration-card" aria-labelledby="nina-integration">
      <div>
        <p className="eyebrow">APPLICATION INTEGRATIONS</p>
        <h2 id="nina-integration">Bring Seiza into N.I.N.A. without a plugin.</h2>
      </div>
      <div className="about-copy">
        <p><code>seiza-cli</code> 0.5 speaks the ASTAP command-line contract N.I.N.A. already uses. Select ASTAP for the normal and blind solver, point it at <code>seiza.exe</code>, and keep the star catalog on the imaging machine.</p>
        <div className="text-links">
          <a href="/docs/api#integrations">Set up N.I.N.A. <span aria-hidden="true">→</span></a>
          <a href="https://github.com/theatrus/seiza/releases">Windows releases <span aria-hidden="true">↗</span></a>
        </div>
      </div>
    </section>

    <section className="about-card">
      <div>
        <p className="eyebrow">ABOUT SEIZA</p>
        <h2>A small, inspectable engine for astronomical software.</h2>
      </div>
      <div className="about-copy">
        <p>Seiza (星座, せいざ) is Japanese for “constellation.” The library and this service were created by <strong><a href="https://theatr.us">Yann Ramin</a></strong> to provide a modern, embeddable plate solver for observatories, imaging tools, and curious skywatchers.</p>
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
  const [uploadProgress, setUploadProgress] = useState(0)

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    const file = form.get('file')
    if (!(file instanceof File) || file.size === 0) {
      setError('Choose an image to solve.')
      return
    }
    let options: SolveOptions
    try {
      options = solveOptionsFromForm(form)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
      return
    }
    setSubmitting(true)
    setUploadProgress(0)
    setError(null)
    try {
      const job = await submitSolve(file, options, setUploadProgress)
      navigate(`/solutions/${job.id}`)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Upload failed')
      setSubmitting(false)
    }
  }

  return <main className="solve-page">
    <header className="page-heading">
      <p className="eyebrow">PLATE SOLVER</p>
      <h1>Solve this image.</h1>
      <p className="intro">Choose an image to start. Large uploads are resumable, and solving happens in the background. Your result gets an unguessable link; the image and preview are deleted after about a day.</p>
      <p className="ownership-note">Your image remains yours. Seiza does not claim ownership and stores it only temporarily to provide the solve unless you explicitly allow Seiza to use it for validation afterward.</p>
    </header>
    <section className="panel">
      <form onSubmit={onSubmit}>
        <div className="file-submit-row">
          <label className="file-input"><span>FITS or image file</span><input name="file" type="file" accept=".fits,.fit,.fts,image/png,image/jpeg,image/tiff,image/webp" required /></label>
          <button className="button solve-submit-button" disabled={submitting}>{submitting ? `Uploading · ${uploadProgress}%` : <><span>Solve</span><span className="go-arrow" aria-hidden="true">→</span></>}</button>
        </div>
        {submitting && <div className="upload-progress" aria-live="polite">
          <progress max="100" value={uploadProgress} />
          <span>Uploading resumably · {uploadProgress}%</span>
        </div>}
        <SolveOptionsFields />
      </form>
    </section>
    {error && <p className="error" role="alert">{error}</p>}
  </main>
}

function SolutionPage({ jobId }: { jobId: string }) {
  const [job, setJob] = useState<Job | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [pollVersion, setPollVersion] = useState(0)
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
  }, [jobId, pollVersion])

  const isSettled = job != null && !pending.has(job.status)

  return <main className={`solution-page${isSettled ? ' solution-page-settled' : ''}`}>
    <header className="solution-heading">
      <div><p className="eyebrow">SOLUTION</p><h1>{job ? titleForStatus(job.status) : 'Loading solution…'}</h1></div>
      {job && <span className={`status ${job.status}`}>{job.status}</span>}
    </header>
    {error && <p className="error" role="alert">{error}</p>}
    {job && <SolutionContent job={job} onRetried={(retried) => {
      setJob(retried)
      setPollVersion((version) => version + 1)
    }} onDonated={setJob} />}
  </main>
}

function titleForStatus(status: Job['status']) {
  if (status === 'queued') return 'Waiting in the queue.'
  if (status === 'solving') return 'Reading the stars.'
  if (status === 'failed') return 'The solve did not converge.'
  return 'The field is solved.'
}

function SolutionContent({ job, onRetried, onDonated }: { job: Job; onRetried: (job: Job) => void; onDonated: (job: Job) => void }) {
  const [annotations, setAnnotations] = useState<Annotations | null>(null)
  const [annotationError, setAnnotationError] = useState<string | null>(null)
  const [layers, setLayers] = useState(defaultOverlayLayers)
  const [hiddenCatalogs, setHiddenCatalogs] = useState<DeepSkyCatalogId[]>([])
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
  const overlayAvailability = currentAnnotations?.available
  const minorBodiesNeedCaptureTime = overlayAvailability?.minor_bodies === false
    && currentAnnotations?.capture_time == null
    && currentAnnotations?.catalog_version.split(';').some((version) => version.startsWith('minor-bodies:')) === true
  const unavailableLayers = overlayAvailability && [
    ['deep_sky', 'Deep sky'],
    ['named_stars', 'Named stars'],
    ['star_identifiers', 'Star identifiers'],
    ['transients', 'Transients'],
    ['minor_bodies', 'Solar system'],
  ].filter(([key]) => overlayAvailability[key] === false
    && !(key === 'minor_bodies' && minorBodiesNeedCaptureTime)).map(([, label]) => label)
  const disabledReasons = minorBodiesNeedCaptureTime
    ? { minor_bodies: 'Solar system positions require an acquisition time for this image' }
    : undefined
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
      <div><span>Total solve time</span><strong>{job.solve_time_ms != null ? formatDurationMs(job.solve_time_ms) : job.status === 'solving' ? 'Timing…' : job.status === 'queued' ? 'Waiting for worker' : 'Not recorded'}</strong></div>
      <div><span>Image retention</span><strong>{job.validation_donation ? 'contributed for long-term validation' : job.input_available ? `until ${new Date(job.input_expires_at).toLocaleString()}` : 'expired and deleted'}</strong></div>
    </section>
    {!pending.has(job.status) && <ValidationDonationPanel job={job} onDonated={onDonated} />}
    {job.error && <p className="error">{job.error}</p>}
    {job.status === 'failed' && job.input_available && <RetrySolveForm job={job} onRetried={onRetried} />}
    {job.status === 'failed' && !job.input_available && <p className="expired-note">This image can no longer be retried because its one-day upload retention period has ended. Upload it again to start a new solve.</p>}
    {pending.has(job.status) && <section className="panel waiting"><div className="orbit" aria-hidden="true"><span /></div><p>This durable page refreshes automatically. You can bookmark it or come back later.</p></section>}
    {solution && <>
      {job.preview_url ? <section className="overlay-card">
        <div className="section-heading"><div><p className="eyebrow">SKY OVERLAY</p><h2>Explore the solved field</h2></div><div className="overlay-actions"><button className="button small secondary" type="button" onClick={() => setExpanded(true)}>Expand image</button><button className="button small" type="button" disabled={downloading} onClick={() => void downloadPng()}>{downloading ? 'Rendering…' : 'Download rendered PNG'}</button></div></div>
        <OverlayControls
          layers={layers}
          counts={overlayCounts}
          available={overlayAvailability}
          disabledReasons={disabledReasons}
          objects={overlayObjects}
          hiddenCatalogs={hiddenCatalogs}
          onChange={setLayers}
          onHiddenCatalogsChange={setHiddenCatalogs}
        />
        {unavailableLayers && unavailableLayers.length > 0 && <p className="overlay-warning">Catalog data unavailable for this solution: {unavailableLayers.join(', ')}.</p>}
        {minorBodiesNeedCaptureTime && <p className="overlay-warning">Solar system positions require an acquisition time for this image. The minor-body catalog is installed.</p>}
        {annotationError && <p className="overlay-warning">Live catalogs could not be refreshed: {annotationError}</p>}
        {exportError && <p className="overlay-warning">PNG rendering failed: {exportError}</p>}
        <div className={`image-stage${expanded ? ' expanded' : ''}`} role={expanded ? 'dialog' : undefined} aria-modal={expanded || undefined} aria-label={expanded ? 'Expanded astronomical image overlay' : undefined}>
          {expanded && <button className="overlay-close" type="button" onClick={() => setExpanded(false)}>Close</button>}
          <div className="sky-frame" ref={frameRef}>
            <img src={job.preview_url} alt="Uploaded astronomical image" />
            <AstroOverlay solution={solution} objects={overlayObjects} layers={layers} hiddenCatalogs={hiddenCatalogs} />
          </div>
        </div>
        <p className="retention-note">The SVG annotations are rendered interactively over the image. {job.validation_donation ? 'This contributed image is retained in Seiza’s long-term validation set.' : 'The temporary image expires after one day; WCS and catalog metadata remain available.'}</p>
      </section> : !job.input_available && <p className="expired-note">The uploaded image and visual overlay have been deleted after their one-day retention period. The complete WCS solution remains below.</p>}
      <section className="metric-grid">
        <Metric label="Center RA" value={`${solution.center_ra_deg.toFixed(8)}°`} />
        <Metric label="Center Dec" value={`${solution.center_dec_deg.toFixed(8)}°`} />
        <Metric label="Pixel scale" value={`${solution.pixel_scale_arcsec_per_pixel.toFixed(5)}″/px`} />
        <Metric label="Fit quality" value={`${solution.matched_stars} stars · ${solution.rms_arcsec.toFixed(4)}″ RMS`} />
      </section>
      {solution.statistics && <SolverStatistics job={job} />}
      <WcsDetails job={job} />
      <ValidationDonationReminder job={job} />
    </>}
  </>
}

function SolverStatistics({ job }: { job: Job }) {
  const solution = job.solution!
  const stats = solution.statistics!
  const matchYield = stats.detected_stars > 0
    ? `${solution.matched_stars}/${stats.detected_stars} · ${(solution.matched_stars / stats.detected_stars * 100).toFixed(1)}%`
    : 'No detections'
  const indexDetail = stats.blind_index_patterns != null
    ? ` · ${stats.blind_index_patterns.toLocaleString()} blind-index patterns`
    : ''
  const strategy = stats.mode === 'blind'
    ? 'Blind solve'
    : stats.hint_source === 'fits_header'
      ? `Hinted · FITS ${stats.hint_keywords?.join(', ') ?? 'headers'}`
      : 'Hinted solve'
  return <section className="solver-stats">
    <div className="section-heading"><div><p className="eyebrow">SOLVER TELEMETRY</p><h2>Nerd stats</h2></div></div>
    <div className="metric-grid">
      <Metric label="Solver pipeline" value={formatDurationMs(stats.total_ms)} />
      <Metric label="Strategy" value={strategy} />
      <Metric label="Detected stars" value={stats.detected_stars.toLocaleString()} />
      <Metric label="Match yield" value={matchYield} />
    </div>
    <p className="solver-phase-breakdown">
      Decode {formatDurationMs(stats.decode_ms)} · detect {formatDurationMs(stats.detection_ms)} · search and fit {formatDurationMs(stats.search_ms)} · {solution.image_width.toLocaleString()}×{solution.image_height.toLocaleString()} px · {stats.catalog_stars.toLocaleString()} catalog stars{indexDetail}
    </p>
  </section>
}

function ValidationDonationPanel({ job, onDonated }: { job: Job; onDonated: (job: Job) => void }) {
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  if (job.validation_donation) {
    return <section className="donation-cta donated-cta" id="validation-donation">
      <details className="donation-details">
        <summary><span className="donation-cta-copy"><span className="eyebrow">VALIDATION SET</span><strong>Contributed to Seiza’s validation set</strong></span><span className="donation-cta-action">View details</span></summary>
        <div className="donation-form">
          <p>Seiza will retain this image under the grant accepted on {new Date(job.validation_donation.donated_at).toLocaleString()}. You still own it.</p>
          {job.validation_donation.solve_is_invalid && <p className="donation-invalid"><strong>Invalid solve</strong>This result was marked invalid for validation.</p>}
          {job.validation_donation.comment && <p className="donation-comment"><strong>Your note</strong>{job.validation_donation.comment}</p>}
        </div>
      </details>
    </section>
  }

  if (!job.input_available) {
    return <section className="donation-cta unavailable-cta" id="validation-donation">
      <span className="donation-cta-copy"><span className="eyebrow">VALIDATION SET</span><strong>Image no longer available to contribute</strong></span>
    </section>
  }

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    setSubmitting(true)
    setError(null)
    try {
      onDonated(await donateValidationImage(
        job.id,
        String(form.get('validation_comment') ?? ''),
        form.get('validation_solve_is_invalid') === 'on',
        form.get('validation_license_agreed') === 'on',
      ))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Image contribution failed')
      setSubmitting(false)
    }
  }

  return <section className="donation-cta" id="validation-donation">
    <details className="donation-details">
      <summary><span className="donation-cta-copy"><span className="eyebrow">VALIDATION SET</span><strong>Help improve Seiza with this image</strong></span><span className="donation-cta-action">Review contribution</span></summary>
      <div className="donation-form">
        <p className="donation-intro">Ordinary uploads remain yours and are deleted after about one day. If you opt in here, Seiza will keep this image for its long-term validation and training set. A note about why the solve succeeded or failed is optional.</p>
        <form onSubmit={onSubmit}>
          <label>Optional comment<textarea name="validation_comment" maxLength={2000} rows={4} placeholder="What makes this image useful for solver validation?" /></label>
          <label className="validation-quality">
            <input name="validation_solve_is_invalid" type="checkbox" />
            <span><strong>Mark this solve result as invalid</strong><small>Use this for an incorrect WCS, a false positive, or a failed solve that should have succeeded.</small></span>
          </label>
          <label className="license-consent">
            <input name="validation_license_agreed" type="checkbox" required />
            <span><strong>I attest that I own this image or have authority to contribute it.</strong><small>I keep ownership and give Seiza and its maintainers permission to retain, copy, and process this image as part of Seiza’s validation set, only to test, validate, debug, and improve the Seiza plate solver, including training and evaluating solver-related models. Seiza will not make the validation set public, sell the image, or use it for unrelated purposes.</small></span>
          </label>
          <button className="button" disabled={submitting}>{submitting ? 'Contributing image…' : 'Contribute image for validation'}</button>
          {error && <p className="error" role="alert">{error}</p>}
        </form>
      </div>
    </details>
  </section>
}

function ValidationDonationReminder({ job }: { job: Job }) {
  if (job.validation_donation) {
    return <aside className="donation-reminder"><span>Thank you—this solved image is part of Seiza’s long-term validation set.</span><a href="#validation-donation">View contribution details ↑</a></aside>
  }
  if (!job.input_available) return null
  return <aside className="donation-reminder"><span>Help improve future solves by contributing this field to Seiza’s validation set.</span><a href="#validation-donation">Contribute this solved image ↑</a></aside>
}

function RetrySolveForm({ job, onRetried }: { job: Job; onRetried: (job: Job) => void }) {
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    let options: SolveOptions
    try {
      options = solveOptionsFromForm(new FormData(event.currentTarget), job.options)
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason))
      return
    }
    setSubmitting(true)
    setError(null)
    try {
      onRetried(await retrySolve(job.id, options))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Retry failed')
      setSubmitting(false)
    }
  }

  return <section className="panel retry-panel">
    <div className="section-heading">
      <div><p className="eyebrow">TRY AGAIN</p><h2>Re-solve the retained image</h2></div>
      <span className="no-upload-badge">No re-upload</span>
    </div>
    <p className="retry-intro">Add a position or scale hint and place this same image back in the queue. Its private solution URL stays unchanged. {job.validation_donation ? 'The contributed validation copy remains available for this retry.' : 'The original image-retention deadline also stays unchanged.'}</p>
    <form onSubmit={onSubmit}>
      <SolveOptionsFields defaults={job.options} />
      <button className="button" disabled={submitting}>{submitting ? 'Starting retry…' : 'Retry retained image'}</button>
      {error && <p className="error" role="alert">{error}</p>}
    </form>
  </section>
}

async function downloadRenderedPng(previewUrl: string, frame: HTMLDivElement, solution: NonNullable<Job['solution']>, jobId: string) {
  const separator = previewUrl.includes('?') ? '&' : '?'
  const response = await fetch(`${previewUrl}${separator}full=true`)
  if (!response.ok) throw new Error(`full-resolution image request failed (${response.status})`)
  const overlay = frame.querySelector('svg')
  if (!overlay) throw new Error('the overlay is not ready')
  const seizaMark = await loadImage('/seiza-mark.png?watermark=1')
  const png = await renderOverlayPng({
    background: await response.blob(),
    overlay,
    width: solution.image_width,
    height: solution.image_height,
    decorate: (context, size) => drawSeizaWatermark(
      context,
      seizaMark,
      size.width,
      size.height,
    ),
  })
  downloadBlob(png, `seiza-solution-${jobId}.png`)
}

function drawSeizaWatermark(
  context: CanvasRenderingContext2D,
  logo: HTMLImageElement,
  width: number,
  height: number,
) {
  let scale = Math.max(0.4, Math.min(width / 1_600, height / 1_200, 3.5))
  const measure = () => {
    context.font = `700 ${Math.round(27 * scale)}px ui-sans-serif, system-ui, sans-serif`
    const titleWidth = context.measureText('Solved with Seiza').width
    context.font = `600 ${Math.round(20 * scale)}px ui-sans-serif, system-ui, sans-serif`
    const urlWidth = context.measureText('seiza.fyi').width
    return Math.max(titleWidth, urlWidth)
  }
  const naturalWidth = () => 22 * scale + 64 * scale + 18 * scale + measure() + 24 * scale
  if (naturalWidth() > width * 0.92) scale *= width * 0.92 / naturalWidth()

  const padding = 18 * scale
  const logoSize = 64 * scale
  const gap = 16 * scale
  const textWidth = measure()
  const plaqueWidth = padding + logoSize + gap + textWidth + padding
  const plaqueHeight = Math.max(logoSize + padding * 1.2, 94 * scale)
  const margin = Math.max(8, Math.min(width, height) * 0.018)
  const x = width - plaqueWidth - margin
  const y = height - plaqueHeight - margin
  const radius = 13 * scale

  context.save()
  context.beginPath()
  context.roundRect(x, y, plaqueWidth, plaqueHeight, radius)
  context.fillStyle = 'rgba(4, 12, 18, .84)'
  context.fill()
  context.strokeStyle = 'rgba(125, 219, 232, .72)'
  context.lineWidth = Math.max(1, 1.5 * scale)
  context.stroke()
  context.drawImage(logo, x + padding, y + (plaqueHeight - logoSize) / 2, logoSize, logoSize)

  const textX = x + padding + logoSize + gap
  context.textBaseline = 'alphabetic'
  context.font = `700 ${Math.round(27 * scale)}px ui-sans-serif, system-ui, sans-serif`
  context.fillStyle = '#f5f8f7'
  context.fillText('Solved with Seiza', textX, y + plaqueHeight * 0.48)
  context.font = `600 ${Math.round(20 * scale)}px ui-sans-serif, system-ui, sans-serif`
  context.fillStyle = '#f2c66d'
  context.fillText('seiza.fyi', textX, y + plaqueHeight * 0.75)
  context.restore()
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
      : object.kind === 'identified-star'
        ? 'star_identifiers'
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
function formatDurationMs(value: number) {
  if (value < 1) return `${value.toFixed(2)} ms`
  if (value < 1_000) return `${value.toFixed(value < 10 ? 2 : 1)} ms`
  if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 2 : 1)} s`
  const minutes = Math.floor(value / 60_000)
  return `${minutes}m ${((value % 60_000) / 1_000).toFixed(1)}s`
}
function Metric({ label, value }: { label: string; value: string }) { return <div><span>{label}</span><strong>{value}</strong></div> }
function DataPair({ label, value }: { label: string; value: string }) { return <div><dt>{label}</dt><dd>{value}</dd></div> }

function NotFoundPage() {
  return <main className="narrow-page"><section className="empty-state"><p className="eyebrow">404</p><h1>This point is off the chart.</h1><p className="intro">The page does not exist, but the solver is ready for another field.</p><Link to="/solve" className="button">Solve an image</Link></section></main>
}

function SiteFooter() {
  const [versions, setVersions] = useState<Health['versions'] | null>(null)
  useEffect(() => {
    let active = true
    getHealth().then((health) => {
      if (active) setVersions(health.versions)
    }).catch(() => undefined)
    return () => { active = false }
  }, [])

  return <footer>
    <div className="footer-product">
      <span>Seiza · 星座 · せいざ</span>
      {versions && <span className="footer-versions" aria-label="Software versions">Seiza Server v{versions.seiza_server} · Seiza v{versions.seiza}</span>}
    </div>
    <nav className="footer-links" aria-label="Project links">
      <span>Apache-2.0</span>
      <a href="https://theatr.us">Built by Yann Ramin</a>
      <a href="https://github.com/theatrus/seiza">Seiza GitHub <span aria-hidden="true">↗</span></a>
      <a href="https://github.com/theatrus/seiza-server">Server GitHub <span aria-hidden="true">↗</span></a>
    </nav>
  </footer>
}
