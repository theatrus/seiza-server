export type JobStatus = 'queued' | 'solving' | 'succeeded' | 'failed'

export interface SolveOptions {
  center_ra_deg?: number
  center_dec_deg?: number
  radius_deg?: number
  scale_arcsec_per_pixel?: number
  scale_tolerance?: number
  min_scale_arcsec_per_pixel?: number
  max_scale_arcsec_per_pixel?: number
  capture_time?: string
}

export interface OverlayObject {
  name: string
  common_name: string
  kind: string
  mag: number | null
  x: number
  y: number
  semi_major_px: number
  semi_minor_px: number
  angle_deg: number
  source?: string
  ra_deg?: number
  dec_deg?: number
  discovered?: string
  near_capture?: boolean
  distance_au?: number
  direction_pa_deg?: number
  direction_angle_deg?: number
}

export interface Solution {
  center_ra_deg: number
  center_dec_deg: number
  pixel_scale_arcsec_per_pixel: number
  matched_stars: number
  rms_arcsec: number
  image_width: number
  image_height: number
  wcs: {
    crval: [number, number]
    crpix: [number, number]
    cd: [[number, number], [number, number]]
    ctype: [string, string]
    cunit: [string, string]
    radesys: string
    equinox: number
  }
  footprint: [[number, number], [number, number], [number, number], [number, number]]
  objects: OverlayObject[]
  catalog_version?: string
  capture_time?: string
}

export interface Annotations {
  job_id: string
  catalog_version: string
  capture_time: string | null
  counts: Record<string, number>
  objects: OverlayObject[]
}

export interface Job {
  id: string
  status: JobStatus
  created_at: string
  started_at: string | null
  completed_at: string | null
  original_filename: string
  input_expires_at: string
  input_available: boolean
  preview_url: string | null
  overlay_url: string | null
  annotations_url: string | null
  wcs_url: string | null
  solution: Solution | null
  error: string | null
}

interface ApiFailure { error?: { message?: string } }

async function expectJson<T>(response: Response): Promise<T> {
  const payload = await response.json() as T & ApiFailure
  if (!response.ok) throw new Error(payload.error?.message ?? `Request failed (${response.status})`)
  return payload
}

export async function submitSolve(file: File, options: SolveOptions): Promise<Job> {
  const form = new FormData()
  form.append('file', file)
  form.append('options', JSON.stringify(options))
  return expectJson<Job>(await fetch('/api/v1/solves', { method: 'POST', body: form }))
}

export async function getSolve(jobId: string): Promise<Job> {
  return expectJson<Job>(await fetch(`/api/v1/solves/${jobId}`))
}

export async function getAnnotations(url: string): Promise<Annotations> {
  const separator = url.includes('?') ? '&' : '?'
  return expectJson<Annotations>(await fetch(
    `${url}${separator}field_stars=true&historical_transients=true&field_star_mag_limit=10&max_field_stars=300`,
  ))
}
