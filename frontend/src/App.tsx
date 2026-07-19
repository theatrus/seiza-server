import { FormEvent, ReactNode, useCallback, useEffect, useId, useRef, useState } from 'react'
import { downloadBlob, renderOverlayPng } from '@seiza/astro-overlay/export'
import { AccountDetails, Annotations, ApiError, Health, Job, OverlayObject, SolveOptions, completeEmailSignIn, createApiKey, donateValidationImage, getAccount, getAnnotations, getHealth, getSolve, logout, registerPasskey, resolveSolve, revokeApiKey, revokePasskey, revokeSession, signInWithPasskey, startEmailSignIn, submitSolve } from './api'
import { ApiDocsPage } from './ApiDocs'
import { AstroOverlay, OverlayControls } from './AstroOverlay'
import { DataSourcesPage } from './DataSources'
import type { OverlayLayers } from './AstroOverlay'
import type { SuggestedDeepSkyCatalogId as DeepSkyCatalogId } from '@seiza/astro-overlay'

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

type CaptureTimeZone = 'local' | 'utc'

function numberOrUndefined(value: FormDataEntryValue | null): number | undefined {
  if (typeof value !== 'string' || value.trim() === '') return undefined
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : undefined
}

function parseCaptureDateTime(value: string, timeZone: CaptureTimeZone) {
  const parsed = new Date(timeZone === 'utc' ? `${value}Z` : value)
  return Number.isNaN(parsed.getTime()) ? null : parsed
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
    sip_order: numberOrUndefined(form.get('sip_order')),
  }
  const captureTime = form.get('capture_time')
  if (typeof captureTime === 'string' && captureTime !== '') {
    const captureTimeZone = form.get('capture_time_zone') ?? 'local'
    if (captureTimeZone !== 'local' && captureTimeZone !== 'utc') {
      throw new Error('Choose whether the acquisition time is local time or UTC.')
    }
    const parsed = parseCaptureDateTime(captureTime, captureTimeZone)
    if (!parsed) throw new Error('Acquisition time is not a valid date and time.')
    options.capture_time = parsed.toISOString()
  }
  return options
}

function dateTimeInputValue(value: string | null | undefined, timeZone: CaptureTimeZone) {
  if (!value) return ''
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return ''
  if (timeZone === 'utc') return date.toISOString().slice(0, 19)
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000)
  return local.toISOString().slice(0, 19)
}

function localTimeZoneDescription(value: string) {
  const date = parseCaptureDateTime(value, 'local') ?? new Date()
  const offsetMinutes = -date.getTimezoneOffset()
  const sign = offsetMinutes >= 0 ? '+' : '-'
  const hours = String(Math.floor(Math.abs(offsetMinutes) / 60)).padStart(2, '0')
  const minutes = String(Math.abs(offsetMinutes) % 60).padStart(2, '0')
  const zoneName = Intl.DateTimeFormat().resolvedOptions().timeZone || 'browser local time'
  return `${zoneName} (UTC${sign}${hours}:${minutes})`
}

function SolveOptionsFields({ defaults }: { defaults?: SolveOptions }) {
  const [captureTimeZone, setCaptureTimeZone] = useState<CaptureTimeZone>('local')
  const [captureTime, setCaptureTime] = useState(() => dateTimeInputValue(defaults?.capture_time, 'local'))
  const captureTimeHelpId = useId()

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
      <p><strong>FITS DATE-OBS is used automatically; timestamps without an offset are treated as UTC.</strong> For other images, enter the time shown by the camera or acquisition software and identify whether that clock used local time or UTC. This lets Seiza position comets and asteroids and scope transient events.</p>
      <div className="capture-time-grid">
        <label>Date and time<input name="capture_time" type="datetime-local" step="1" value={captureTime} aria-describedby={captureTimeHelpId} onChange={(event) => setCaptureTime(event.target.value)} /></label>
        <label>Time zone<select name="capture_time_zone" value={captureTimeZone} aria-describedby={captureTimeHelpId} onChange={(event) => {
          const nextTimeZone = event.target.value as CaptureTimeZone
          const instant = captureTime ? parseCaptureDateTime(captureTime, captureTimeZone) : null
          setCaptureTimeZone(nextTimeZone)
          if (instant) setCaptureTime(dateTimeInputValue(instant.toISOString(), nextTimeZone))
        }}>
          <option value="local">Local · {localTimeZoneDescription(captureTime)}</option>
          <option value="utc">UTC · Coordinated Universal Time</option>
        </select></label>
      </div>
      <p id={captureTimeHelpId} className="capture-time-note">{captureTimeZone === 'local'
        ? <><strong>Local entry:</strong> this browser will interpret the value as {localTimeZoneDescription(captureTime)}.</>
        : <><strong>UTC entry:</strong> the value will be interpreted as Coordinated Universal Time, with no local offset.</>} Seiza submits and stores the instant in UTC.</p>
    </fieldset>
    <details>
      <summary>Advanced solve controls <span className="optional-badge">Optional</span></summary>
      <div className="form-grid">
        <label>Minimum scale (arcsec/px)<input name="min_scale" type="number" min="0.01" step="any" placeholder="0.1" defaultValue={defaults?.min_scale_arcsec_per_pixel ?? ''} /></label>
        <label>Maximum scale (arcsec/px)<input name="max_scale" type="number" min="0.01" step="any" placeholder="20" defaultValue={defaults?.max_scale_arcsec_per_pixel ?? ''} /></label>
        <label>Hint scale tolerance<input name="scale_tolerance" type="number" min="0.01" max="1" step="0.01" placeholder="0.2" defaultValue={defaults?.scale_tolerance ?? ''} /></label>
        <label>SIP distortion order<select name="sip_order" defaultValue={String(defaults?.sip_order ?? 0)}>
          <option value="0">Linear TAN only</option>
          <option value="2">Order 2</option>
          <option value="3">Order 3</option>
          <option value="4">Order 4</option>
          <option value="5">Order 5</option>
        </select></label>
      </div>
      <p className="solve-control-note">SIP orders 2–5 fit optical distortion after the linear solve. Seiza keeps the linear result unless the polynomial materially improves the residual.</p>
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
  const [authMode, setAuthMode] = useState<Health['auth_mode'] | null>(null)
  const [account, setAccount] = useState<AccountDetails | null>(null)
  const [accountChecked, setAccountChecked] = useState(false)
  useEffect(() => {
    const updatePath = () => setPath(window.location.pathname)
    window.addEventListener('popstate', updatePath)
    return () => window.removeEventListener('popstate', updatePath)
  }, [])
  const refreshAccount = useCallback(async () => {
    const current = await getAccount()
    setAccount(current)
    setAccountChecked(true)
    return current
  }, [])
  useEffect(() => {
    let active = true
    getHealth().then(async (health) => {
      if (!active) return
      setAuthMode(health.auth_mode)
      if (health.auth_mode === 'accounts') {
        const current = await getAccount()
        if (active) setAccount(current)
      }
      if (active) setAccountChecked(true)
    }).catch(() => { if (active) setAccountChecked(true) })
    return () => { active = false }
  }, [])
  const solutionMatch = path.match(/^\/solutions\/((?:\d+-)?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/)

  return <div className="site-shell">
    <SiteHeader accountsEnabled={authMode === 'accounts'} account={account} />
    {path === '/' && <HomePage />}
    {path === '/solve' && <SolvePage accountsEnabled={authMode === 'accounts'} account={account} accountChecked={accountChecked} />}
    {path === '/docs/api' && <ApiDocsPage />}
    {path === '/data-sources' && <DataSourcesPage />}
    {path === '/signin' && <SignInPage accountsEnabled={authMode === 'accounts'} account={account} onAuthenticated={refreshAccount} />}
    {path === '/account' && <AccountPage accountsEnabled={authMode === 'accounts'} account={account} accountChecked={accountChecked} onAccountChanged={refreshAccount} />}
    {solutionMatch && <SolutionPage jobId={solutionMatch[1]} />}
    {path !== '/' && path !== '/solve' && path !== '/docs/api' && path !== '/data-sources' && path !== '/signin' && path !== '/account' && !solutionMatch && <NotFoundPage />}
    <SiteFooter />
  </div>
}

function SiteHeader({ accountsEnabled, account }: { accountsEnabled: boolean; account: AccountDetails | null }) {
  return <nav className="site-nav" aria-label="Primary navigation">
    <Link to="/" className="brand-link">
      <img src="/seiza-mark.png" alt="" width="38" height="38" />
      <span><strong>Seiza</strong><small>星座 · せいざ</small></span>
    </Link>
    <div className="nav-links">
      <Link to="/">About</Link>
      <Link to="/docs/api">API</Link>
      {accountsEnabled && <Link to={account ? '/account' : '/signin'}>{account ? 'Account' : 'Sign in'}</Link>}
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
          <Link to="/data-sources">See our data sources <span aria-hidden="true">→</span></Link>
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
        <p>Download the pre-built Windows <code>seiza.exe</code>, add the hosted catalog files, and select ASTAP for N.I.N.A.’s normal and blind solvers. No Rust toolchain, installer, or plugin is required.</p>
        <div className="text-links">
          <a href="/docs/api#integrations">Set up N.I.N.A. <span aria-hidden="true">→</span></a>
          <a href="https://github.com/theatrus/seiza/releases">Download Windows binary <span aria-hidden="true">↗</span></a>
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
          <Link to="/data-sources">Data sources &amp; acknowledgements <span aria-hidden="true">→</span></Link>
        </div>
      </div>
    </section>
  </main>
}

function SolvePage({
  accountsEnabled,
  account,
  accountChecked,
}: {
  accountsEnabled: boolean
  account: AccountDetails | null
  accountChecked: boolean
}) {
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [uploadProgress, setUploadProgress] = useState(0)

  if (accountsEnabled && !account) {
    if (!accountChecked) return <main className="narrow-page solve-page"><p className="intro">Loading account…</p></main>
    return <main className="narrow-page solve-page">
      <section className="empty-state">
        <p className="eyebrow">PLATE SOLVER</p>
        <h1>Sign in to solve.</h1>
        <p className="intro">This Seiza deployment requires an account before uploading images. Sign in with your passkey or a verified email, then return here to start a solve.</p>
        <Link to="/signin" className="button">Sign in</Link>
      </section>
    </main>
  }

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
    {isSettled && job ? <div className="settled-topbar">
      <SolutionHeading job={job} />
      <ValidationDonationPanel job={job} onDonated={setJob} />
    </div> : <SolutionHeading job={job} />}
    {error && <p className="error" role="alert">{error}</p>}
    {job && <SolutionContent job={job} onRetried={(retried) => {
      setJob(retried)
      setPollVersion((version) => version + 1)
      navigate(`/solutions/${retried.id}`)
    }} />}
  </main>
}

function SolutionHeading({ job }: { job: Job | null }) {
  return <header className="solution-heading">
    <div><p className="eyebrow">SOLUTION</p><h1>{job ? titleForStatus(job.status) : 'Loading solution…'}</h1></div>
    {job && <span className={`status ${job.status}`}>{job.status}</span>}
  </header>
}

function titleForStatus(status: Job['status']) {
  if (status === 'queued') return 'Waiting in the queue.'
  if (status === 'solving') return 'Reading the stars.'
  if (status === 'failed') return 'The solve did not converge.'
  return 'The field is solved.'
}

function SolutionContent({ job, onRetried }: { job: Job; onRetried: (job: Job) => void }) {
  const [annotations, setAnnotations] = useState<Annotations | null>(null)
  const [annotationError, setAnnotationError] = useState<string | null>(null)
  const [layers, setLayers] = useState(defaultOverlayLayers)
  const [hiddenCatalogs, setHiddenCatalogs] = useState<DeepSkyCatalogId[]>([])
  const [showCatalogOutlines, setShowCatalogOutlines] = useState(true)
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
    {!solution && <JobMeta job={job} />}
    {job.error && <p className="error">{job.error}</p>}
    {job.status === 'failed' && job.input_available && <RetrySolveForm job={job} onRetried={onRetried} />}
    {job.status === 'failed' && !job.input_available && <p className="expired-note">This image can no longer be retried because its one-day upload retention period has ended. Upload it again to start a new solve.</p>}
    {pending.has(job.status) && <section className="panel waiting"><div className="orbit" aria-hidden="true"><span /></div><p>This durable page refreshes automatically. You can bookmark it or come back later.</p></section>}
    {solution && <>
      {job.preview_url ? <section className="overlay-card">
        <div className="section-heading"><p className="eyebrow">SKY OVERLAY</p></div>
        <OverlayControls
          layers={layers}
          counts={overlayCounts}
          available={overlayAvailability}
          disabledReasons={disabledReasons}
          objects={overlayObjects}
          hiddenCatalogs={hiddenCatalogs}
          showCatalogOutlines={showCatalogOutlines}
          onChange={setLayers}
          onHiddenCatalogsChange={setHiddenCatalogs}
          onShowCatalogOutlinesChange={setShowCatalogOutlines}
        />
        {unavailableLayers && unavailableLayers.length > 0 && <p className="overlay-warning">Catalog data unavailable for this solution: {unavailableLayers.join(', ')}.</p>}
        {minorBodiesNeedCaptureTime && <p className="overlay-warning">Solar system positions require an acquisition time for this image. The minor-body catalog is installed.</p>}
        {annotationError && <p className="overlay-warning">Live catalogs could not be refreshed: {annotationError}</p>}
        {exportError && <p className="overlay-warning">PNG rendering failed: {exportError}</p>}
        <div
          className={`image-stage${expanded ? ' expanded' : ''}`}
          role={expanded ? 'dialog' : 'button'}
          tabIndex={expanded ? undefined : 0}
          aria-modal={expanded || undefined}
          aria-label={expanded ? 'Expanded astronomical image overlay' : 'Expand image'}
          onClick={() => { if (!expanded) setExpanded(true) }}
          onKeyDown={(event) => {
            if (!expanded && (event.key === 'Enter' || event.key === ' ')) {
              event.preventDefault()
              setExpanded(true)
            }
          }}
        >
          {expanded && <button className="overlay-close" type="button" onClick={() => setExpanded(false)}>Close</button>}
          <div className="sky-frame" ref={frameRef}>
            <img src={job.preview_url} alt="Uploaded astronomical image" />
            <AstroOverlay solution={solution} objects={overlayObjects} layers={layers} hiddenCatalogs={hiddenCatalogs} showCatalogOutlines={showCatalogOutlines} />
          </div>
        </div>
        <div className="overlay-footer">
          <p className="retention-note">The SVG annotations are rendered interactively over the image. {job.validation_donation ? 'This contributed image is retained in Seiza’s long-term validation set.' : 'The temporary image expires after one day; WCS and catalog metadata remain available.'}</p>
          <div className="overlay-actions overlay-actions-below"><button className="button small" type="button" disabled={downloading} onClick={() => void downloadPng()}>{downloading ? 'Rendering…' : 'Download rendered PNG'}</button></div>
        </div>
      </section> : !job.input_available && <p className="expired-note">The uploaded image and visual overlay have been deleted after their one-day retention period. The complete WCS solution remains below.</p>}
      <JobMeta job={job} />
      <section className="metric-grid">
        <Metric label="Center RA" value={`${solution.center_ra_deg.toFixed(8)}°`} />
        <Metric label="Center Dec" value={`${solution.center_dec_deg.toFixed(8)}°`} />
        <Metric label="Pixel scale" value={`${solution.pixel_scale_arcsec_per_pixel.toFixed(5)}″/px`} />
        <Metric label="Fit quality" value={`${solution.matched_stars} stars · ${solution.rms_arcsec.toFixed(4)}″ RMS`} />
      </section>
      {solution.statistics && <SolverStatistics job={job} />}
      <WcsDetails job={job} />
      <ValidationDonationReminder job={job} />
      {job.status === 'succeeded' && job.input_available && <details className="resolve-again-details">
        <summary>Re-solve this retained image with different settings</summary>
        <RetrySolveForm job={job} onRetried={onRetried} />
      </details>}
    </>}
  </>
}

function JobMeta({ job }: { job: Job }) {
  return <section className="job-meta">
    <div><span>File</span><strong>{job.original_filename}</strong></div>
    <div><span>Submitted</span><strong>{new Date(job.created_at).toLocaleString()}</strong></div>
    <div><span>Total solve time</span><strong>{job.solve_time_ms != null ? formatDurationMs(job.solve_time_ms) : job.status === 'solving' ? 'Timing…' : job.status === 'queued' ? 'Waiting for worker' : 'Not recorded'}</strong></div>
    <div><span>Image retention</span><strong>{job.validation_donation ? 'contributed for long-term validation' : job.input_available ? `until ${new Date(job.input_expires_at).toLocaleString()}` : 'expired and deleted'}</strong></div>
  </section>
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
      onRetried(await resolveSolve(job.id, options))
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
    <p className="retry-intro">Add a position or scale hint and place a retained copy of this image back in the queue. The original result stays unchanged and the new solve receives its own private URL. {job.validation_donation ? 'The contributed validation copy remains available as the source image.' : 'The copied image receives a fresh one-day retention window.'}</p>
    <form onSubmit={onSubmit}>
      <SolveOptionsFields defaults={job.options} />
      <button className="button" disabled={submitting}>{submitting ? 'Starting re-solve…' : 'Re-solve retained image'}</button>
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
  const sip = wcs.sip
  const forwardTerms = sip ? sip.a.length + sip.b.length : 0
  const inverseTerms = sip ? sip.ap.length + sip.bp.length : 0
  return <section className="wcs-card">
    <div className="section-heading"><div><p className="eyebrow">WORLD COORDINATE SYSTEM</p><h2>Complete WCS calibration</h2></div>{job.wcs_url && <a className="button small" href={job.wcs_url}>Download .wcs</a>}</div>
    <div className="wcs-grid">
      <DataPair label="Projection" value={`${wcs.ctype[0]} / ${wcs.ctype[1]}`} />
      <DataPair label="Reference frame" value={`${wcs.radesys} · equinox ${wcs.equinox.toFixed(1)}`} />
      <DataPair label="CRVAL" value={`${format(wcs.crval[0])}, ${format(wcs.crval[1])} deg`} />
      <DataPair label="CRPIX (zero-indexed)" value={`${format(wcs.crpix[0])}, ${format(wcs.crpix[1])} px`} />
      <DataPair label="Image dimensions" value={`${solution.image_width} × ${solution.image_height} px`} />
      <DataPair label="Units" value={`${wcs.cunit[0]} / ${wcs.cunit[1]}`} />
      <DataPair label="Distortion model" value={sip ? `SIP order ${sip.order} · ${forwardTerms} forward + ${inverseTerms} inverse coefficients` : 'Linear TAN · no SIP distortion'} />
      <DataPair label="Capture time" value={solution.capture_time ? new Date(solution.capture_time).toLocaleString() : 'Not recorded'} />
      <DataPair label="Annotation catalog" value={solution.catalog_version ?? 'Not configured'} />
    </div>
    <div className="matrix-wrap">
      <h3>CD matrix <small>degrees per pixel</small></h3>
      <code>{formatScientific(wcs.cd[0][0])} &nbsp; {formatScientific(wcs.cd[0][1])}<br />{formatScientific(wcs.cd[1][0])} &nbsp; {formatScientific(wcs.cd[1][1])}</code>
    </div>
    {sip && <details className="sip-records">
      <summary>SIP coefficient records <span>{sip.a.length + sip.b.length + sip.ap.length + sip.bp.length} values</span></summary>
      <p>Forward <code>A/B</code> terms correct pixel offsets before the CD matrix. Inverse <code>AP/BP</code> terms map tangent-plane offsets back to pixels.</p>
      <div className="sip-record-grid">
        <SipCoefficientSet name="A" values={sip.a} />
        <SipCoefficientSet name="B" values={sip.b} />
        <SipCoefficientSet name="AP" values={sip.ap} />
        <SipCoefficientSet name="BP" values={sip.bp} />
      </div>
    </details>}
    <div className="footprint-wrap">
      <h3>ICRS footprint <small>RA, Dec in degrees</small></h3>
      <ol>{solution.footprint.map(([ra, dec], index) => <li key={index}><span>Corner {index + 1}</span><code>{format(ra)}, {format(dec)}</code></li>)}</ol>
    </div>
    {solution.objects.length > 0 && <details className="object-list"><summary>{solution.objects.length} catalog objects in field</summary><ul>{solution.objects.map((object, index) => <li key={`${object.name}-${index}`}><strong>{object.common_name || object.name}</strong><span>{object.name} · {object.kind}{object.mag == null ? '' : ` · mag ${object.mag.toFixed(1)}`}</span></li>)}</ul></details>}
  </section>
}

function SipCoefficientSet({ name, values }: { name: string; values: Array<[number, number, number]> }) {
  return <section><h4>{name}</h4><code>{values.map(([p, q, value]) => `${name}_${p}_${q} = ${formatScientific(value)}`).join('\n')}</code></section>
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

function SignInPage({
  accountsEnabled,
  account,
  onAuthenticated,
}: {
  accountsEnabled: boolean
  account: AccountDetails | null
  onAuthenticated: () => Promise<AccountDetails | null>
}) {
  const linkToken = new URLSearchParams(window.location.search).get('token')
  const [email, setEmail] = useState('')
  const [challengeId, setChallengeId] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [notice, setNotice] = useState<string | null>(null)

  async function finish(request: Parameters<typeof completeEmailSignIn>[0]) {
    setSubmitting(true)
    setError(null)
    try {
      await completeEmailSignIn(request)
      await onAuthenticated()
      window.history.replaceState({}, '', '/account')
      window.dispatchEvent(new PopStateEvent('popstate'))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Sign-in failed')
      setSubmitting(false)
    }
  }

  async function requestEmail(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    try {
      const started = await startEmailSignIn(email)
      setChallengeId(started.challenge_id)
      setNotice('Check your email for a sign-in link or enter the eight-digit code below.')
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Email sign-in is unavailable')
    } finally {
      setSubmitting(false)
    }
  }

  async function submitCode(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const code = String(new FormData(event.currentTarget).get('code') ?? '').replaceAll(' ', '')
    if (!challengeId) return
    await finish({ email, challenge_id: challengeId, code })
  }

  async function authenticateWithPasskey() {
    setSubmitting(true)
    setError(null)
    try {
      await signInWithPasskey()
      await onAuthenticated()
      window.history.replaceState({}, '', '/account')
      window.dispatchEvent(new PopStateEvent('popstate'))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Passkey sign-in failed')
      setSubmitting(false)
    }
  }

  if (!accountsEnabled) {
    return <main className="narrow-page auth-page"><section className="empty-state"><p className="eyebrow">ACCOUNTS</p><h1>Sign-in is not enabled here.</h1><p className="intro">This Seiza deployment currently accepts solves without an account.</p><Link to="/solve" className="button">Solve an image</Link></section></main>
  }
  if (account) {
    return <main className="narrow-page auth-page"><section className="empty-state"><p className="eyebrow">SIGNED IN</p><h1>You are already aboard.</h1><p className="intro">Signed in as {account.account.email}.</p><Link to="/account" className="button">Open your account</Link></section></main>
  }
  if (linkToken) {
    return <main className="narrow-page auth-page"><header className="page-heading"><p className="eyebrow">EMAIL VERIFIED</p><h1>Finish signing in.</h1><p className="intro">The link is valid for one sign-in. Continue only if you requested it.</p></header><section className="panel auth-panel"><button className="button" disabled={submitting} onClick={() => void finish({ link_token: linkToken })}>{submitting ? 'Signing in…' : 'Continue sign-in'}</button>{error && <p className="error" role="alert">{error}</p>}</section></main>
  }

  return <main className="narrow-page auth-page">
    <header className="page-heading"><p className="eyebrow">YOUR SEIZA ACCOUNT</p><h1>Sign in to your sky.</h1><p className="intro">Use a passkey when one is already connected to your account, or verify your email to sign in and set one up.</p></header>
    <div className="auth-grid">
      <section className="panel auth-panel passkey-first">
        <p className="eyebrow">RECOMMENDED</p><h2>Use a passkey</h2>
        <p>Passkeys are phishing-resistant and do not require a password. Your device can offer a passkey already connected to this Seiza account.</p>
        <button className="button" disabled={submitting} onClick={() => void authenticateWithPasskey()}>{submitting ? 'Waiting for passkey…' : 'Use a passkey'}</button>
        {error && <p className="error" role="alert">{error}</p>}
      </section>
      <section className="panel auth-panel">
        <p className="eyebrow">EMAIL VERIFICATION</p><h2>Send a link and code</h2>
        <form onSubmit={requestEmail}>
          <label>Email address<input type="email" autoComplete="email" required value={email} onChange={(event) => setEmail(event.target.value)} /></label>
          <button className="button secondary" disabled={submitting}>{submitting ? 'Sending…' : challengeId ? 'Send another email' : 'Email me a sign-in link'}</button>
        </form>
        {notice && <p className="success-note" role="status">{notice}</p>}
        {challengeId && <form className="code-form" onSubmit={submitCode}>
          <label>Eight-digit code<input name="code" inputMode="numeric" autoComplete="one-time-code" pattern="[0-9 ]{8,15}" required placeholder="12345678" /></label>
          <button className="button" disabled={submitting}>{submitting ? 'Verifying…' : 'Verify and sign in'}</button>
        </form>}
        {error && <p className="error" role="alert">{error}</p>}
      </section>
    </div>
  </main>
}

function AccountPage({
  accountsEnabled,
  account,
  accountChecked,
  onAccountChanged,
}: {
  accountsEnabled: boolean
  account: AccountDetails | null
  accountChecked: boolean
  onAccountChanged: () => Promise<AccountDetails | null>
}) {
  const [submitting, setSubmitting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [reauthRequired, setReauthRequired] = useState(false)
  const [passkeyLabel, setPasskeyLabel] = useState('This device')
  const [apiKeyName, setApiKeyName] = useState('Observatory')
  const [apiKeyRead, setApiKeyRead] = useState(true)
  const [apiKeySubmit, setApiKeySubmit] = useState(true)
  const [createdApiToken, setCreatedApiToken] = useState<string | null>(null)

  async function signOut(all: boolean) {
    setSubmitting(true)
    setError(null)
    try {
      await logout(all)
      await onAccountChanged()
      navigate('/signin')
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Sign-out failed')
      setSubmitting(false)
    }
  }

  // Passkey and API-key changes require a session verified within the last
  // ten minutes; the server refuses older sessions with 403.
  function reportSecurityError(reason: unknown, fallback: string) {
    if (reason instanceof ApiError && reason.status === 403) {
      setReauthRequired(true)
      return
    }
    setError(reason instanceof Error ? reason.message : fallback)
  }

  async function addPasskey(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    setReauthRequired(false)
    try {
      await registerPasskey(passkeyLabel)
      await onAccountChanged()
    } catch (reason) {
      reportSecurityError(reason, 'Passkey setup failed')
    } finally {
      setSubmitting(false)
    }
  }

  async function removePasskey(passkeyId: string) {
    setSubmitting(true)
    setError(null)
    setReauthRequired(false)
    try {
      await revokePasskey(passkeyId)
      await onAccountChanged()
    } catch (reason) {
      reportSecurityError(reason, 'Passkey removal failed')
    } finally {
      setSubmitting(false)
    }
  }

  async function addApiKey(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setSubmitting(true)
    setError(null)
    setReauthRequired(false)
    setCreatedApiToken(null)
    try {
      const scopes = [apiKeyRead && 'solve:read', apiKeySubmit && 'solve:submit'].filter((scope): scope is string => Boolean(scope))
      const created = await createApiKey(apiKeyName, scopes)
      setCreatedApiToken(created.token)
      await onAccountChanged()
    } catch (reason) {
      reportSecurityError(reason, 'API key creation failed')
    } finally {
      setSubmitting(false)
    }
  }

  async function removeApiKey(keyId: string) {
    setSubmitting(true)
    setError(null)
    setReauthRequired(false)
    try {
      await revokeApiKey(keyId)
      await onAccountChanged()
    } catch (reason) {
      reportSecurityError(reason, 'API key revocation failed')
    } finally {
      setSubmitting(false)
    }
  }

  async function removeSession(sessionId: string) {
    setSubmitting(true)
    setError(null)
    try {
      await revokeSession(sessionId)
      await onAccountChanged()
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Session revocation failed')
    } finally {
      setSubmitting(false)
    }
  }

  if (!accountChecked) return <main className="narrow-page auth-page"><p className="intro">Loading account…</p></main>
  if (!accountsEnabled || !account) {
    return <main className="narrow-page auth-page"><section className="empty-state"><p className="eyebrow">ACCOUNT</p><h1>Sign in to continue.</h1><p className="intro">Your account keeps browser sessions, passkeys, and API keys under your control.</p><Link to="/signin" className="button">Sign in</Link></section></main>
  }

  return <main className="account-page">
    <header className="page-heading"><p className="eyebrow">ACCOUNT</p><h1>{account.account.email}</h1><p className="intro">Verified {new Date(account.account.email_verified_at).toLocaleString()}.</p></header>
    {reauthRequired && <section className="account-callout" role="alert"><div><p className="eyebrow">RECENT SIGN-IN REQUIRED</p><h2>Sign in again to change security settings</h2><p>Adding or removing passkeys and API keys requires a session verified within the last ten minutes. Sign in again, then retry the change.</p></div><Link to="/signin" className="button">Sign in again</Link></section>}
    {account.passkey_setup_required && <section className="account-callout"><div><p className="eyebrow">RECOMMENDED NEXT STEP</p><h2>Add a passkey</h2><p>Your email is verified. Add a phishing-resistant passkey for faster sign-in; email remains available for recovery.</p></div><form className="passkey-create-form" onSubmit={addPasskey}><label>Passkey name<input value={passkeyLabel} maxLength={80} required onChange={(event) => setPasskeyLabel(event.target.value)} /></label><button className="button" disabled={submitting}>{submitting ? 'Waiting for device…' : 'Add a passkey'}</button></form></section>}
    {!account.passkey_setup_required && <section className="panel account-section">
      <div className="section-heading"><div><p className="eyebrow">PASSKEYS</p><h2>Phishing-resistant sign-in</h2></div></div>
      <div className="session-list">{account.passkeys.map((passkey) => <div key={passkey.id}><div><strong>{passkey.label}</strong><span>Added {new Date(passkey.created_at).toLocaleString()}{passkey.last_used_at ? ` · last used ${new Date(passkey.last_used_at).toLocaleString()}` : ''}</span></div><button className="text-button" disabled={submitting} onClick={() => void removePasskey(passkey.id)}>Remove</button></div>)}</div>
      <details className="add-security-item"><summary>Add another passkey</summary><form className="passkey-create-form" onSubmit={addPasskey}><label>Passkey name<input value={passkeyLabel} maxLength={80} required onChange={(event) => setPasskeyLabel(event.target.value)} /></label><button className="button small" disabled={submitting}>Add passkey</button></form></details>
    </section>}
    <section className="panel account-section">
      <div className="section-heading"><div><p className="eyebrow">API KEYS</p><h2>Programmatic access</h2></div></div>
      <p className="section-intro">Use account API keys with <code>X-API-Key</code> or <code>Authorization: Bearer</code>. Every key shares this account’s queue identity.</p>
      {createdApiToken && <div className="secret-once" role="status"><strong>Copy this key now—it will not be shown again.</strong><code>{createdApiToken}</code><div className="secret-actions"><button className="button secondary small" onClick={() => void navigator.clipboard.writeText(createdApiToken)}>Copy key</button><button className="text-button" onClick={() => setCreatedApiToken(null)}>I’ve saved it</button></div></div>}
      <div className="session-list">{account.api_keys.map((key) => <div key={key.id}><div><strong>{key.name}</strong><span>{key.display_prefix} · {key.scopes.join(', ')}{key.last_used_at ? ` · last used ${new Date(key.last_used_at).toLocaleString()}` : ''}</span></div><button className="text-button" disabled={submitting} onClick={() => void removeApiKey(key.id)}>Revoke</button></div>)}</div>
      <details className="add-security-item"><summary>Create an API key</summary><form className="api-key-form" onSubmit={addApiKey}><label>Key name<input value={apiKeyName} maxLength={80} required onChange={(event) => setApiKeyName(event.target.value)} /></label><fieldset><legend>Scopes</legend><label className="inline-check"><input type="checkbox" checked={apiKeyRead} onChange={(event) => setApiKeyRead(event.target.checked)} /> Read solve results</label><label className="inline-check"><input type="checkbox" checked={apiKeySubmit} onChange={(event) => setApiKeySubmit(event.target.checked)} /> Submit and re-solve images</label></fieldset><button className="button small" disabled={submitting || (!apiKeyRead && !apiKeySubmit)}>Create key</button></form></details>
    </section>
    <section className="panel account-section">
      <div className="section-heading"><div><p className="eyebrow">SECURITY</p><h2>Signed-in sessions</h2></div><button className="button secondary small" disabled={submitting} onClick={() => void signOut(true)}>Sign out everywhere</button></div>
      <div className="session-list">{account.sessions.map((session) => <div key={session.id}><div><strong>{session.current ? 'This browser' : session.kind === 'browser' ? 'Browser session' : session.api_key_id ? 'Astrometry (API key)' : 'Astrometry session'}</strong><span>Last used {new Date(session.last_seen_at).toLocaleString()} · expires {new Date(session.expires_at).toLocaleString()}</span></div><button className="text-button" disabled={submitting} onClick={() => session.current ? void signOut(false) : void removeSession(session.id)}>{session.current ? 'Sign out' : 'Revoke'}</button></div>)}</div>
      {error && <p className="error" role="alert">{error}</p>}
    </section>
  </main>
}

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
      <Link to="/data-sources">Data sources</Link>
      <a href="https://theatr.us">Built by Yann Ramin</a>
      <a href="https://github.com/theatrus/seiza">Seiza GitHub <span aria-hidden="true">↗</span></a>
      <a href="https://github.com/theatrus/seiza-server">Server GitHub <span aria-hidden="true">↗</span></a>
    </nav>
  </footer>
}
