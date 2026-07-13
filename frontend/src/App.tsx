import { FormEvent, useEffect, useState } from 'react'
import { Job, SolveOptions, getSolve, submitSolve } from './api'

const pending = new Set(['queued', 'solving'])

function numberOrUndefined(value: FormDataEntryValue | null): number | undefined {
  if (typeof value !== 'string' || value.trim() === '') return undefined
  const parsed = Number(value)
  return Number.isFinite(parsed) ? parsed : undefined
}

export default function App() {
  const [job, setJob] = useState<Job | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  useEffect(() => {
    if (!job || !pending.has(job.status)) return
    const id = window.setTimeout(() => {
      getSolve(job.id).then(setJob).catch((reason: unknown) => setError(reason instanceof Error ? reason.message : String(reason)))
    }, 1_500)
    return () => window.clearTimeout(id)
  }, [job])

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
    setSubmitting(true)
    setError(null)
    setJob(null)
    try {
      setJob(await submitSolve(file, options))
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : 'Upload failed')
    } finally {
      setSubmitting(false)
    }
  }

  return <main>
    <header>
      <p className="eyebrow">SEIZA / PLATE SOLVING</p>
      <h1>Find the sky in your image.</h1>
      <p className="intro">Upload a FITS or regular image. The service stores it, queues the solve fairly, and returns a standards-friendly WCS solution.</p>
    </header>

    <section className="panel">
      <form onSubmit={onSubmit}>
        <label className="file-input">
          <span>Image</span>
          <input name="file" type="file" accept=".fits,.fit,.fts,image/png,image/jpeg,image/tiff,image/webp" required />
        </label>
        <fieldset>
          <legend>Optional position hint</legend>
          <p>Leave this blank for a blind solve. A position hint is faster and more reliable for narrow fields.</p>
          <div className="grid">
            <label>RA (degrees)<input name="center_ra_deg" type="number" min="0" max="360" step="any" placeholder="e.g. 210.802" /></label>
            <label>Dec (degrees)<input name="center_dec_deg" type="number" min="-90" max="90" step="any" placeholder="e.g. 54.349" /></label>
            <label>Pixel scale (arcsec/px)<input name="scale_arcsec_per_pixel" type="number" min="0.01" step="any" placeholder="e.g. 1.24" /></label>
            <label>Search radius (degrees)<input name="radius_deg" type="number" min="0.1" step="any" placeholder="2" /></label>
          </div>
        </fieldset>
        <details>
          <summary>Blind solve settings</summary>
          <div className="grid">
            <label>Minimum scale (arcsec/px)<input name="min_scale" type="number" min="0.01" step="any" placeholder="0.3" /></label>
            <label>Maximum scale (arcsec/px)<input name="max_scale" type="number" min="0.01" step="any" placeholder="20" /></label>
            <label>Hint scale tolerance<input name="scale_tolerance" type="number" min="0.01" max="1" step="0.01" placeholder="0.2" /></label>
          </div>
        </details>
        <button disabled={submitting}>{submitting ? 'Queueing…' : 'Queue solve'}</button>
      </form>
    </section>

    {error && <p className="error" role="alert">{error}</p>}
    {job && <JobCard job={job} />}
  </main>
}

function JobCard({ job }: { job: Job }) {
  const solution = job.solution
  return <section className="panel result" aria-live="polite">
    <div className="result-heading"><div><p className="eyebrow">JOB #{job.id}</p><h2>{job.status}</h2></div><span className={`status ${job.status}`}>{job.status}</span></div>
    <p>{job.original_filename}</p>
    {job.error && <p className="error">{job.error}</p>}
    {solution && <div className="solution-grid">
      <Metric label="Center RA" value={`${solution.center_ra_deg.toFixed(6)}°`} />
      <Metric label="Center Dec" value={`${solution.center_dec_deg.toFixed(6)}°`} />
      <Metric label="Pixel scale" value={`${solution.pixel_scale_arcsec_per_pixel.toFixed(4)}″/px`} />
      <Metric label="Quality" value={`${solution.matched_stars} stars · ${solution.rms_arcsec.toFixed(3)}″ RMS`} />
    </div>}
    {pending.has(job.status) && <p className="muted">This page refreshes the job status automatically. Solving runs only in a background worker.</p>}
  </section>
}

function Metric({ label, value }: { label: string, value: string }) {
  return <div><span>{label}</span><strong>{value}</strong></div>
}
