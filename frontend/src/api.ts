import Uppy from '@uppy/core'
import Tus from '@uppy/tus'

export type JobStatus = 'queued' | 'solving' | 'succeeded' | 'failed'

const uploadChunkBytes = 5 * 1024 * 1024
const parallelUploadThresholdBytes = uploadChunkBytes * 2
const parallelUploadParts = 3

export interface SolveOptions {
  center_ra_deg?: number | null
  center_dec_deg?: number | null
  radius_deg?: number | null
  scale_arcsec_per_pixel?: number | null
  scale_tolerance?: number | null
  min_scale_arcsec_per_pixel?: number | null
  max_scale_arcsec_per_pixel?: number | null
  sigma?: number | null
  ignore_border?: number | null
  max_stars?: number | null
  capture_time?: string | null
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
  available?: Record<string, boolean>
  counts: Record<string, number>
  objects: OverlayObject[]
}

export interface ValidationDonation {
  comment: string | null
  solve_is_invalid: boolean
  license_version: string
  donated_at: string
}

export interface Job {
  id: string
  status: JobStatus
  created_at: string
  started_at: string | null
  completed_at: string | null
  original_filename: string
  options: SolveOptions
  input_expires_at: string
  input_available: boolean
  preview_url: string | null
  overlay_url: string | null
  annotations_url: string | null
  wcs_url: string | null
  solution: Solution | null
  error: string | null
  validation_donation: ValidationDonation | null
}

export interface Health {
  status: 'ready' | 'degraded'
  versions: {
    seiza_server: string
    seiza: string
  }
  solver_ready: boolean
  queue_depth: number
  auth_mode: 'public' | 'stub-api-key'
  job_backend: 'sqlx' | 'dynamodb'
  queue_transport: 'local' | 'sqs'
  embedded_workers: number
}

interface ApiFailure { error?: { message?: string } }

async function expectJson<T>(response: Response): Promise<T> {
  const payload = await response.json() as T & ApiFailure
  if (!response.ok) throw new Error(payload.error?.message ?? `Request failed (${response.status})`)
  return payload
}

export async function getHealth(): Promise<Health> {
  return expectJson<Health>(await fetch('/api/v1/health'))
}

export async function submitSolve(
  file: File,
  options: SolveOptions,
  onProgress?: (progress: number) => void,
): Promise<Job> {
  const uppy = new Uppy({
    // Uppy includes its instance ID in the TUS fingerprint. Scope resumable
    // sessions to the solve settings as well as the file identity so changing
    // options never resumes an upload created with stale metadata.
    id: `seiza-solve-${solveOptionsFingerprint(options)}`,
    autoProceed: false,
    restrictions: { maxNumberOfFiles: 1 },
  })
  uppy.use(Tus, {
    endpoint: '/api/v1/uploads',
    chunkSize: uploadChunkBytes,
    retryDelays: [0, 1_000, 3_000, 5_000],
    limit: 1,
    // Keep failed/interrupted uploads resumable, but never reuse a completed
    // upload URL and its already-created solve job.
    removeFingerprintOnSuccess: true,
    allowedMetaFields: false,
    onBeforeRequest: (request, uploadedFile) => {
      request.setHeader('Upload-Metadata', [
        ['filename', uploadedFile.name],
        ['filetype', uploadedFile.type || 'application/octet-stream'],
        ['options', JSON.stringify(options)],
      ].map(([key, value]) => `${key} ${base64Metadata(value)}`).join(','))
    },
  })
  if (onProgress) uppy.on('progress', onProgress)
  const fileId = uppy.addFile({
    name: file.name,
    type: file.type,
    data: file,
  })
  if (file.size >= parallelUploadThresholdBytes) {
    uppy.setFileState(fileId, {
      tus: {
        ...uppy.getFile(fileId)?.tus,
        parallelUploads: parallelUploadParts,
      },
    })
  }
  try {
    const result = await uppy.upload()
    const failed = result?.failed ?? []
    if (failed.length > 0) {
      throw failed[0].error ?? new Error('Upload failed')
    }
    const uploaded = result?.successful?.[0]
    const uploadUrl = uploaded?.uploadURL ?? uploaded?.response?.uploadURL
    if (!uploadUrl) throw new Error('Upload completed without a result URL')
    return expectJson<Job>(await fetch(`${uploadUrl}/result`))
  } finally {
    const tus = uppy.getPlugin('Tus')
    for (const uploadedFile of uppy.getFiles()) {
      // Stop local requests without terminating an incomplete server session.
      // Successful uploads have already removed their stored fingerprint;
      // interrupted uploads remain resumable for the same solve settings.
      tus?.resetUploaderReferences(uploadedFile.id, { abort: false })
    }
    uppy.destroy()
  }
}

function solveOptionsFingerprint(options: SolveOptions): string {
  const normalized = Object.fromEntries(
    Object.entries(options)
      .filter(([, value]) => value !== undefined)
      .sort(([left], [right]) => left.localeCompare(right)),
  )
  return base64Metadata(JSON.stringify(normalized))
    .replaceAll('+', '-')
    .replaceAll('/', '_')
    .replace(/=+$/, '')
}

function base64Metadata(value: string): string {
  const bytes = new TextEncoder().encode(value)
  let binary = ''
  for (let offset = 0; offset < bytes.length; offset += 8_192) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8_192))
  }
  return btoa(binary)
}

export async function getSolve(jobId: string): Promise<Job> {
  return expectJson<Job>(await fetch(`/api/v1/solves/${jobId}`))
}

export async function retrySolve(jobId: string, options: SolveOptions): Promise<Job> {
  return expectJson<Job>(await fetch(`/api/v1/solves/${jobId}/retry`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(options),
  }))
}

export async function donateValidationImage(
  jobId: string,
  comment: string,
  solveIsInvalid: boolean,
  licenseAgreed: boolean,
): Promise<Job> {
  return expectJson<Job>(await fetch(`/api/v1/solves/${jobId}/validation-donation`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ comment, solve_is_invalid: solveIsInvalid, license_agreed: licenseAgreed }),
  }))
}

export async function getAnnotations(url: string): Promise<Annotations> {
  const separator = url.includes('?') ? '&' : '?'
  return expectJson<Annotations>(await fetch(
    `${url}${separator}field_stars=true&star_identifiers=true&historical_transients=true&field_star_mag_limit=10&max_field_stars=300&star_identifier_mag_limit=10&max_star_identifiers=150`,
  ))
}
