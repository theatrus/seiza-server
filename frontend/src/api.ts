import Uppy from '@uppy/core'
import Tus from '@uppy/tus'

export type JobStatus = 'queued' | 'solving' | 'succeeded' | 'failed'

const uploadChunkBytes = 32 * 1024 * 1024
const parallelUploadThresholdBytes = uploadChunkBytes * 2
const parallelUploadParts = 3
const webClient = 'web'

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
  sip_order?: number | null
  capture_time?: string | null
  exposure_seconds?: number | null
  observer_latitude_deg?: number | null
  observer_longitude_deg?: number | null
  observer_altitude_m?: number | null
  observer_itrf_m?: [number, number, number] | null
  satellite_metadata_source?: 'explicit' | 'fits_header'
  satellite_metadata_keywords?: string[]
}

export type SipCoefficient = [p: number, q: number, value: number]

export interface SipDistortion {
  order: number
  a: SipCoefficient[]
  b: SipCoefficient[]
  ap: SipCoefficient[]
  bp: SipCoefficient[]
}

export interface OverlayObject {
  stable_id?: string
  name: string
  common_name: string
  kind: string
  mag: number | null
  x: number
  y: number
  semi_major_px: number
  semi_minor_px: number
  angle_deg: number | null
  source?: string
  catalog_source?: string
  aliases?: string[]
  parent_ids?: string[]
  alternate_ids?: string[]
  alternate_sources?: string[]
  ra_deg?: number
  dec_deg?: number
  discovered?: string
  near_capture?: boolean
  distance_au?: number
  motion_arcsec_per_hour?: number
  direction_pa_deg?: number
  direction_angle_deg?: number
  outlines?: Array<{
    geometry_id: string
    source_record_id: string
    role: string
    quality: string
    level: string | null
    contours: Array<{
      closed: boolean
      points: Array<[number, number]>
    }>
  }>
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
    sip?: SipDistortion
  }
  footprint: [[number, number], [number, number], [number, number], [number, number]]
  objects: OverlayObject[]
  catalog_version?: string
  capture_time?: string
  statistics?: {
    total_ms: number
    decode_ms: number
    detection_ms: number
    search_ms: number
    mode: 'blind' | 'hinted'
    detected_stars: number
    catalog_stars: number
    blind_index_patterns?: number
    hint_source?: 'explicit' | 'fits_header'
    hint_keywords?: string[]
  }
}

export interface Annotations {
  job_id: string
  catalog_version: string
  capture_time: string | null
  available?: Record<string, boolean>
  unavailable_reasons?: Record<string, string>
  counts: Record<string, number>
  objects: OverlayObject[]
  satellite_tracks?: SatelliteTrack[]
  satellite_search?: SatelliteSearchSummary
}

export interface SatelliteTrackSegment {
  start: [number, number]
  end: [number, number]
}

export interface SatellitePixelAlignment {
  status: 'detected' | 'not_detected' | 'not_evaluated'
  not_evaluated_reason?: 'empty_path' | 'too_short' | 'insufficient_coverage' | null
  segments: SatelliteTrackSegment[]
  mean_normal_offset_px: number
  angle_delta_deg: number
  contrast_adu: number
  contrast_sigma: number
  continuity: number
  coverage: number
  search_radius_px: number
}

export interface SatelliteTrack {
  stable_id: string
  label: string
  name: string
  norad_id: number | null
  cospar_id: string | null
  source: string
  element_epoch_utc: string
  element_age_seconds: number
  sample_interval_seconds: number
  maximum_apparent_rate_arcsec_per_second: number | null
  segments: SatelliteTrackSegment[]
  pixel_alignment?: SatellitePixelAlignment | null
  risk: {
    level: 'low' | 'possible' | 'high'
    score: number
    maximum_sunlight_fraction: number
    minimum_range_km: number
    maximum_elevation_deg: number
    clipped_length_px: number
  }
}

export interface SatelliteSearchSummary {
  catalog_source: string
  catalog_retrieved_at: string | null
  elements_considered: number
  propagation_failures: number
  stale_elements: number
  pixel_alignment_attempted?: boolean
  pixel_aligned?: number
  pixel_alignment_error?: string | null
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
  solve_time_ms: number | null
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
  auth_mode: 'public' | 'stub-api-key' | 'accounts'
  public_solve_access: {
    ui: boolean
    api: boolean
  }
  job_backend: 'sqlx' | 'dynamodb'
  queue_transport: 'local' | 'sqs'
  embedded_workers: number
}

interface ApiFailure { error?: { message?: string } }

export interface Account {
  id: string
  email: string
  email_verified_at: string
  created_at: string
}

export interface AccountSession {
  id: string
  kind: 'browser' | 'astrometry'
  api_key_id: string | null
  created_at: string
  last_seen_at: string
  expires_at: string
  revoked_at: string | null
  current: boolean
}

export interface PasskeySummary {
  id: string
  label: string
  created_at: string
  last_used_at: string | null
}

export interface ApiKeySummary {
  id: string
  name: string
  display_prefix: string
  scopes: string[]
  queue_weight: number
  created_at: string
  expires_at: string | null
  last_used_at: string | null
}

export interface AccountDetails {
  account: Account
  csrf_token: string | null
  passkey_setup_required: boolean
  passkeys: PasskeySummary[]
  api_keys: ApiKeySummary[]
  sessions: AccountSession[]
}

export interface AccountSolve {
  id: string
  status: JobStatus
  original_filename: string
  created_at: string
  started_at: string | null
  completed_at: string | null
  solve_time_ms: number | null
}

export interface EmailSignInStart {
  challenge_id: string
  resend_at: string
}

export interface CompletedSignIn {
  status: 'success'
  account: Account
  account_created: boolean
  passkey_setup_required: boolean
  csrf_token: string
}

export class ApiError extends Error {
  readonly status: number

  constructor(message: string, status: number) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

async function expectJson<T>(response: Response): Promise<T> {
  const payload = await response.json() as T & ApiFailure
  if (!response.ok) throw new ApiError(payload.error?.message ?? `Request failed (${response.status})`, response.status)
  return payload
}

export async function getHealth(): Promise<Health> {
  return expectJson<Health>(await sessionFetch('/api/v1/health'))
}

export async function startEmailSignIn(email: string): Promise<EmailSignInStart> {
  return expectJson<EmailSignInStart>(await sessionFetch('/api/v1/auth/email/start', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ email }),
  }))
}

export async function completeEmailSignIn(request: {
  link_token?: string
  email?: string
  challenge_id?: string
  code?: string
}): Promise<CompletedSignIn> {
  return expectJson<CompletedSignIn>(await sessionFetch('/api/v1/auth/email/complete', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(request),
  }))
}

export async function getAccount(): Promise<AccountDetails | null> {
  const response = await sessionFetch('/api/v1/account')
  if (response.status === 401 || response.status === 404) return null
  return expectJson<AccountDetails>(response)
}

export async function getAccountSolves(): Promise<AccountSolve[]> {
  const response = await expectJson<{ solves: AccountSolve[] }>(await sessionFetch('/api/v1/account/solves'))
  return response.solves
}

export async function logout(all = false): Promise<void> {
  await expectJson(await sessionFetch(all ? '/api/v1/auth/logout-all' : '/api/v1/auth/logout', {
    method: 'POST',
  }, true))
}

export async function signInWithPasskey(): Promise<void> {
  ensureWebAuthn()
  const started = await expectJson<WebAuthnStart>(await sessionFetch('/api/v1/auth/passkeys/authentication/start', {
    method: 'POST',
  }))
  const publicKey = authenticationOptions(started.options.publicKey)
  const credential = await navigator.credentials.get({
    publicKey,
    // This is an explicit user action, so open the authenticator chooser. The
    // server's discoverable challenge is also compatible with conditional UI.
    mediation: 'optional',
  })
  if (!(credential instanceof PublicKeyCredential)) throw new Error('Passkey sign-in was cancelled')
  await expectJson(await sessionFetch('/api/v1/auth/passkeys/authentication/complete', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ challenge_id: started.challenge_id, credential: authenticationCredentialJson(credential) }),
  }))
}

export async function registerPasskey(label: string): Promise<PasskeySummary> {
  ensureWebAuthn()
  const started = await expectJson<WebAuthnStart>(await sessionFetch('/api/v1/account/passkeys/registration/start', {
    method: 'POST',
  }, true))
  const credential = await navigator.credentials.create({
    publicKey: registrationOptions(started.options.publicKey),
  })
  if (!(credential instanceof PublicKeyCredential)) throw new Error('Passkey setup was cancelled')
  const result = await expectJson<{ passkey: PasskeySummary }>(await sessionFetch('/api/v1/account/passkeys/registration/complete', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({
      challenge_id: started.challenge_id,
      label,
      credential: registrationCredentialJson(credential),
    }),
  }, true))
  return result.passkey
}

export async function revokePasskey(passkeyId: string): Promise<void> {
  await expectJson(await sessionFetch(`/api/v1/account/passkeys/${passkeyId}`, {
    method: 'DELETE',
  }, true))
}

export async function createApiKey(name: string, scopes: string[]): Promise<{ api_key: ApiKeySummary; token: string }> {
  return expectJson(await sessionFetch('/api/v1/account/api-keys', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ name, scopes }),
  }, true))
}

export async function revokeApiKey(keyId: string): Promise<void> {
  await expectJson(await sessionFetch(`/api/v1/account/api-keys/${keyId}`, {
    method: 'DELETE',
  }, true))
}

export async function revokeSession(sessionId: string): Promise<void> {
  await expectJson(await sessionFetch(`/api/v1/account/sessions/${sessionId}`, {
    method: 'DELETE',
  }, true))
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
      const csrf = csrfToken()
      if (csrf) request.setHeader('X-CSRF-Token', csrf)
      request.setHeader('X-Seiza-Client', webClient)
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
  const parallelUploadBoundaries = alignedParallelUploadBoundaries(file.size)
  if (parallelUploadBoundaries.length > 1) {
    uppy.setFileState(fileId, {
      tus: {
        ...uppy.getFile(fileId)?.tus,
        parallelUploads: parallelUploadBoundaries.length,
        parallelUploadBoundaries,
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
    return expectJson<Job>(await sessionFetch(`${uploadUrl}/result`))
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

function alignedParallelUploadBoundaries(size: number): Array<{ start: number, end: number }> {
  if (size < parallelUploadThresholdBytes) return []
  const chunkCount = Math.ceil(size / uploadChunkBytes)
  const partCount = Math.min(parallelUploadParts, chunkCount)
  const boundaries: Array<{ start: number, end: number }> = []
  let firstChunk = 0
  for (let part = 0; part < partCount; part += 1) {
    const remainingChunks = chunkCount - firstChunk
    const remainingParts = partCount - part
    const chunksInPart = Math.ceil(remainingChunks / remainingParts)
    const nextChunk = firstChunk + chunksInPart
    boundaries.push({
      start: firstChunk * uploadChunkBytes,
      end: Math.min(size, nextChunk * uploadChunkBytes),
    })
    firstChunk = nextChunk
  }
  return boundaries
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
  return expectJson<Job>(await sessionFetch(`/api/v1/solves/${jobId}`))
}

export async function resolveSolve(jobId: string, options: SolveOptions): Promise<Job> {
  return expectJson<Job>(await sessionFetch(`/api/v1/solves/${jobId}/resolve`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(options),
  }, true))
}

export async function donateValidationImage(
  jobId: string,
  comment: string,
  solveIsInvalid: boolean,
  licenseAgreed: boolean,
): Promise<Job> {
  return expectJson<Job>(await sessionFetch(`/api/v1/solves/${jobId}/validation-donation`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ comment, solve_is_invalid: solveIsInvalid, license_agreed: licenseAgreed }),
  }, true))
}

export async function getAnnotations(url: string, satelliteTracks = false): Promise<Annotations> {
  const separator = url.includes('?') ? '&' : '?'
  return expectJson<Annotations>(await sessionFetch(
    `${url}${separator}field_stars=true&star_identifiers=true&historical_transients=true&field_star_mag_limit=10&max_field_stars=300&star_identifier_mag_limit=10&max_star_identifiers=150&satellite_tracks=${satelliteTracks}`,
  ))
}

function csrfToken(): string | null {
  for (const cookie of document.cookie.split(';')) {
    const separator = cookie.indexOf('=')
    if (separator < 0) continue
    const name = cookie.slice(0, separator).trim()
    if (name === '__Host-seiza_csrf' || name === 'seiza_csrf') {
      return decodeURIComponent(cookie.slice(separator + 1))
    }
  }
  return null
}

interface WebAuthnStart {
  challenge_id: string
  options: {
    publicKey: Record<string, unknown>
    mediation?: string
  }
}

function ensureWebAuthn() {
  if (!window.isSecureContext || !('PublicKeyCredential' in window) || !navigator.credentials) {
    throw new Error('Passkeys require a supported browser on a secure connection')
  }
}

function registrationOptions(value: Record<string, unknown>): PublicKeyCredentialCreationOptions {
  const options = value as unknown as PublicKeyCredentialCreationOptionsJSON
  return {
    ...options,
    challenge: decodeBase64Url(options.challenge),
    user: { ...options.user, id: decodeBase64Url(options.user.id) },
    authenticatorSelection: {
      ...options.authenticatorSelection,
      residentKey: 'required',
      requireResidentKey: true,
    },
    excludeCredentials: options.excludeCredentials?.map((credential) => ({
      ...credential,
      type: 'public-key' as const,
      id: decodeBase64Url(credential.id),
      transports: credential.transports as AuthenticatorTransport[] | undefined,
    })),
  } as unknown as PublicKeyCredentialCreationOptions
}

function authenticationOptions(value: Record<string, unknown>): PublicKeyCredentialRequestOptions {
  const options = value as unknown as PublicKeyCredentialRequestOptionsJSON
  return {
    ...options,
    challenge: decodeBase64Url(options.challenge),
    allowCredentials: options.allowCredentials?.map((credential) => ({
      ...credential,
      type: 'public-key' as const,
      id: decodeBase64Url(credential.id),
      transports: credential.transports as AuthenticatorTransport[] | undefined,
    })),
  } as unknown as PublicKeyCredentialRequestOptions
}

function registrationCredentialJson(credential: PublicKeyCredential) {
  const response = credential.response as AuthenticatorAttestationResponse
  return {
    id: credential.id,
    rawId: encodeBase64Url(credential.rawId),
    type: credential.type,
    response: {
      attestationObject: encodeBase64Url(response.attestationObject),
      clientDataJSON: encodeBase64Url(response.clientDataJSON),
      transports: response.getTransports?.() ?? [],
    },
    clientExtensionResults: credential.getClientExtensionResults(),
    authenticatorAttachment: credential.authenticatorAttachment,
  }
}

function authenticationCredentialJson(credential: PublicKeyCredential) {
  const response = credential.response as AuthenticatorAssertionResponse
  return {
    id: credential.id,
    rawId: encodeBase64Url(credential.rawId),
    type: credential.type,
    response: {
      authenticatorData: encodeBase64Url(response.authenticatorData),
      clientDataJSON: encodeBase64Url(response.clientDataJSON),
      signature: encodeBase64Url(response.signature),
      userHandle: response.userHandle ? encodeBase64Url(response.userHandle) : null,
    },
    clientExtensionResults: credential.getClientExtensionResults(),
    authenticatorAttachment: credential.authenticatorAttachment,
  }
}

function decodeBase64Url(value: string): ArrayBuffer {
  const padded = value.replaceAll('-', '+').replaceAll('_', '/') + '='.repeat((4 - value.length % 4) % 4)
  const binary = atob(padded)
  return Uint8Array.from(binary, (character) => character.charCodeAt(0)).buffer
}

function encodeBase64Url(value: ArrayBuffer): string {
  const bytes = new Uint8Array(value)
  let binary = ''
  for (let offset = 0; offset < bytes.length; offset += 8_192) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + 8_192))
  }
  return btoa(binary).replaceAll('+', '-').replaceAll('/', '_').replace(/=+$/, '')
}

async function sessionFetch(
  input: RequestInfo | URL,
  init: RequestInit = {},
  mutation = false,
): Promise<Response> {
  const headers = new Headers(init.headers)
  headers.set('X-Seiza-Client', webClient)
  const csrf = mutation ? csrfToken() : null
  if (csrf) headers.set('X-CSRF-Token', csrf)
  return fetch(input, { ...init, headers, credentials: 'same-origin' })
}
