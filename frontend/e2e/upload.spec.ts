import { expect, test, type Page } from '@playwright/test'
import { mockHealth } from './health'

const publicId = '550e8400-e29b-41d4-a716-446655440000'
const uploadId = '8c741b20-3c42-4e75-95d4-fbc87cc68730'
const oneMiB = 1024 * 1024
const chunkSize = 32 * oneMiB
const resumableFileSize = chunkSize + oneMiB
const parallelFileSize = (chunkSize * 2) + oneMiB

interface UploadConcurrencyState {
  active: number
  maxActive: number
}

type InstrumentedWindow = Window & {
  __seizaUploadConcurrency?: UploadConcurrencyState
}

test.beforeEach(async ({ page }) => {
  await mockHealth(page)
})

test.describe('acquisition time zone', () => {
  test.use({ timezoneId: 'America/Los_Angeles' })

  test('labels local time explicitly and preserves the instant when switching to UTC', async ({ page }) => {
    await page.goto('/solve')
    const captureTime = page.getByLabel('Date and time')
    const timeZone = page.getByLabel('Time zone')

    await expect(timeZone).toHaveValue('local')
    await captureTime.fill('2026-07-16T12:30')
    await expect(timeZone.locator('option:checked')).toContainText('America/Los_Angeles (UTC-07:00)')
    await expect(page.locator('.capture-time-note')).toContainText('this browser will interpret the value as America/Los_Angeles (UTC-07:00)')

    await timeZone.selectOption('utc')
    await expect(captureTime).toHaveValue('2026-07-16T19:30')
    await expect(page.locator('.capture-time-note')).toContainText('Coordinated Universal Time, with no local offset')

    await timeZone.selectOption('local')
    await expect(captureTime).toHaveValue('2026-07-16T12:30')
  })
})

async function instrumentUploadConcurrency(page: Page) {
  await page.addInitScript(() => {
    const state: UploadConcurrencyState = { active: 0, maxActive: 0 }
    ;(window as InstrumentedWindow).__seizaUploadConcurrency = state

    const originalSend = XMLHttpRequest.prototype.send
    XMLHttpRequest.prototype.send = function (body?: Document | XMLHttpRequestBodyInit | null) {
      const isUploadBody = body instanceof Blob
        || body instanceof ArrayBuffer
        || ArrayBuffer.isView(body)
      if (isUploadBody) {
        state.active += 1
        state.maxActive = Math.max(state.maxActive, state.active)
        this.addEventListener('loadend', () => {
          state.active -= 1
        }, { once: true })
      }
      originalSend.call(this, body)
    }
  })
}

function defaultSolveOptions() {
  return {
    center_ra_deg: null,
    center_dec_deg: null,
    radius_deg: 2,
    scale_arcsec_per_pixel: null,
    scale_tolerance: 0.2,
    min_scale_arcsec_per_pixel: 0.1,
    max_scale_arcsec_per_pixel: 20,
    sigma: 4,
    ignore_border: 0,
    max_stars: 500,
    sip_order: 0,
    capture_time: null,
    exposure_seconds: null,
    observer_latitude_deg: null,
    observer_longitude_deg: null,
    observer_altitude_m: null,
    observer_itrf_m: null,
  }
}

async function setStableFile(page: Page, name: string, size: number, type = 'application/fits') {
  await page.getByLabel('FITS, XISF, or image file').evaluate((node, file) => {
    const input = node as HTMLInputElement
    const bytes = new Uint8Array(file.size)
    bytes.fill(42)
    const transfer = new DataTransfer()
    transfer.items.add(new File([bytes], file.name, {
      type: file.type,
      lastModified: 1_720_000_000_000,
    }))
    input.files = transfer.files
    input.dispatchEvent(new Event('input', { bubbles: true }))
    input.dispatchEvent(new Event('change', { bubbles: true }))
  }, { name, size, type })
}

function queuedJob(id: string, filename: string) {
  return {
    id,
    status: 'queued',
    created_at: '2026-07-14T02:00:00Z',
    started_at: null,
    completed_at: null,
    original_filename: filename,
    options: defaultSolveOptions(),
    input_expires_at: '2026-07-15T02:00:00Z',
    input_available: true,
    preview_url: null,
    overlay_url: null,
    annotations_url: null,
    wcs_url: null,
    solution: null,
    error: null,
    validation_donation: null,
  }
}

function solvedJob(id: string, filename: string) {
  return {
    ...queuedJob(id, filename),
    status: 'succeeded',
    completed_at: '2026-07-14T02:01:00Z',
    solution: {
      center_ra_deg: 202.47,
      center_dec_deg: 47.2,
      pixel_scale_arcsec_per_pixel: 1.35,
      matched_stars: 42,
      rms_arcsec: 0.8,
      image_width: 1200,
      image_height: 800,
      wcs: {
        crval: [202.47, 47.2],
        crpix: [600, 400],
        cd: [[-0.000375, 0], [0, 0.000375]],
        ctype: ['RA---TAN', 'DEC--TAN'],
        cunit: ['deg', 'deg'],
        radesys: 'ICRS',
        equinox: 2000,
      },
      footprint: [[202.7, 47.0], [202.2, 47.0], [202.2, 47.4], [202.7, 47.4]],
      objects: [],
      catalog_version: 'test',
    },
  }
}

test('places the solve action beside the file selector and satellite opt-in below', async ({ page }) => {
  await page.goto('/solve')
  await expect(page.getByRole('heading', { name: 'Solve this image.' })).toBeVisible()
  const controls = page.locator('.upload-controls')
  const row = page.locator('.file-submit-row')
  const fileSelector = row.locator('.file-input')
  const satelliteRow = controls.locator('.satellite-trail-opt-in')
  const satelliteTrails = satelliteRow.getByRole('checkbox', { name: 'Show predicted satellite trails' })
  const solveButton = row.getByRole('button', { name: 'Solve', exact: true })

  await expect(fileSelector.getByLabel('FITS, XISF, or image file')).toBeVisible()
  await expect(satelliteRow).toHaveText('Show predicted satellite trails')
  await expect(controls.locator('.satellite-trail-requirements')).toHaveText('Requires FITS or XISF observer and time metadata, or optional fields filled in below.')
  await expect(satelliteTrails).toBeVisible()
  await expect(satelliteTrails).not.toBeChecked()
  await expect(solveButton).toBeVisible()
  const fileBox = await fileSelector.boundingBox()
  const buttonBox = await solveButton.boundingBox()
  const satelliteBox = await satelliteRow.boundingBox()
  expect(fileBox).not.toBeNull()
  expect(buttonBox).not.toBeNull()
  expect(satelliteBox).not.toBeNull()
  expect(buttonBox!.x).toBeGreaterThan(fileBox!.x + fileBox!.width)
  expect(Math.abs((buttonBox!.y + buttonBox!.height) - (fileBox!.y + fileBox!.height))).toBeLessThan(2)
  expect(satelliteBox!.y).toBeGreaterThanOrEqual(fileBox!.y + fileBox!.height)
  const viewport = page.viewportSize()
  expect(viewport).not.toBeNull()
  expect(fileBox!.y + fileBox!.height).toBeLessThan(viewport!.height)
})

test('submits manual satellite observation metadata with a JPEG upload', async ({ page }) => {
  let submittedOptions: Record<string, unknown> | null = null
  let offset = 0
  await page.route('**/api/v1/uploads', async (route) => {
    const metadata = route.request().headers()['upload-metadata']
    const encodedOptions = metadata.split(',').map((field) => field.trim().split(' ')).find(([name]) => name === 'options')?.[1]
    expect(encodedOptions).toBeDefined()
    submittedOptions = JSON.parse(Buffer.from(encodedOptions!, 'base64').toString('utf8'))
    await route.fulfill({
      status: 201,
      headers: { Location: `/api/v1/uploads/${uploadId}`, 'Tus-Resumable': '1.0.0', 'Upload-Offset': '0' },
    })
  })
  await page.route(`**/api/v1/uploads/${uploadId}`, async (route) => {
    const request = route.request()
    if (request.method() === 'HEAD') {
      await route.fulfill({ status: 200, headers: { 'Tus-Resumable': '1.0.0', 'Upload-Length': '1024', 'Upload-Offset': String(offset) } })
      return
    }
    offset = 1024
    await route.fulfill({ status: 204, headers: { 'Tus-Resumable': '1.0.0', 'Upload-Offset': String(offset) } })
  })
  const job = queuedJob(publicId, 'night-sky.jpg')
  await page.route(`**/api/v1/uploads/${uploadId}/result`, async (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(job) }))
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({ contentType: 'application/json', body: JSON.stringify(job) }))

  await page.goto('/solve')
  await setStableFile(page, 'night-sky.jpg', 1024, 'image/jpeg')
  await page.getByLabel('Time zone').selectOption('utc')
  await page.getByLabel('Date and time').fill('2026-07-19T04:05:06')
  await page.getByLabel('Exposure (seconds)').fill('30')
  await page.getByLabel('Observer latitude (° N)').fill('37.3')
  await page.getByLabel('Observer longitude (° E)').fill('-122')
  await page.getByLabel('Observer altitude (m)').fill('50')
  await page.getByRole('checkbox', { name: 'Show predicted satellite trails' }).check()
  await page.getByRole('button', { name: 'Solve', exact: true }).click()

  await expect(page).toHaveURL(`/solutions/${publicId}?satellite_tracks=true`)
  expect(submittedOptions).toMatchObject({
    capture_time: '2026-07-19T04:05:06.000Z',
    exposure_seconds: 30,
    observer_latitude_deg: 37.3,
    observer_longitude_deg: -122,
    observer_altitude_m: 50,
  })
})

test('uploads large images as parallel TUS parts and concatenates them', async ({ page, browserName }) => {
  await instrumentUploadConcurrency(page)
  const partIds = [
    '51000000-0000-4000-8000-000000000001',
    '51000000-0000-4000-8000-000000000002',
    '51000000-0000-4000-8000-000000000003',
  ]
  const finalId = '51000000-0000-4000-8000-000000000004'
  const partLengths = new Map<string, number>()
  let partialCreations = 0
  let finalCreations = 0

  await page.route('**/api/v1/uploads', async (route) => {
    const request = route.request()
    const concat = request.headers()['upload-concat']
    if (concat === 'partial') {
      const id = partIds[partialCreations++]
      expect(id).toBeDefined()
      partLengths.set(id, Number(request.headers()['upload-length']))
      await route.fulfill({
        status: 201,
        headers: {
          Location: `/api/v1/uploads/${id}`,
          'Tus-Resumable': '1.0.0',
          'Upload-Offset': '0',
        },
      })
      return
    }
    expect(concat).toBe(`final;${partIds.map((id) => `/api/v1/uploads/${id}`).join(' ')}`)
    expect(request.headers()['upload-metadata']).toContain('filename ')
    finalCreations += 1
    await route.fulfill({
      status: 201,
      headers: {
        Location: `/api/v1/uploads/${finalId}`,
        'Tus-Resumable': '1.0.0',
        'Upload-Offset': String(parallelFileSize),
      },
    })
  })
  await page.route(/\/api\/v1\/uploads\/51000000-0000-4000-8000-00000000000[1-3]$/, async (route) => {
    const request = route.request()
    const id = new URL(request.url()).pathname.split('/').pop()!
    if (request.method() === 'HEAD') {
      await route.fulfill({
        status: 200,
        headers: {
          'Tus-Resumable': '1.0.0',
          'Upload-Length': String(partLengths.get(id)),
          'Upload-Offset': '0',
          'Upload-Concat': 'partial',
        },
      })
      return
    }
    expect(request.method()).toBe('PATCH')
    await new Promise((resolve) => setTimeout(resolve, 40))
    const interceptedSize = request.postDataBuffer()?.length ?? 0
    if (browserName === 'chromium') expect(interceptedSize).toBe(partLengths.get(id))
    await route.fulfill({
      status: 204,
      headers: {
        'Tus-Resumable': '1.0.0',
        'Upload-Offset': String(partLengths.get(id)),
      },
    })
  })
  const job = queuedJob(publicId, 'parallel-field.fits')
  job.options = {
    ...job.options,
    capture_time: '2026-07-19T04:05:06Z',
    exposure_seconds: 30,
    observer_latitude_deg: 37.3,
    observer_longitude_deg: -122,
    observer_altitude_m: 50,
    satellite_metadata_source: 'fits_header',
    satellite_metadata_keywords: ['DATE-OBS', 'EXPTIME', 'SITELAT', 'SITELONG'],
  }
  await page.route(`**/api/v1/uploads/${finalId}/result`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))

  await page.goto('/solve')
  await setStableFile(page, 'parallel-field.fits', parallelFileSize)
  await page.getByRole('button', { name: 'Solve', exact: true }).click()

  await expect(page).toHaveURL(`/solutions/${publicId}?satellite_tracks=true`)
  expect(partialCreations).toBe(3)
  expect(finalCreations).toBe(1)
  const uploadConcurrency = await page.evaluate(
    () => (window as InstrumentedWindow).__seizaUploadConcurrency?.maxActive ?? 0,
  )
  expect(uploadConcurrency).toBeGreaterThan(1)
  expect([...partLengths.values()]).toEqual([
    chunkSize,
    chunkSize,
    oneMiB,
  ])
})

test('uploads large images in resumable TUS chunks before queueing', async ({ page, browserName }) => {
  const chunks: Array<{ offset: number; size: number }> = []
  let offset = 0
  let totalSize = 0
  let usedLegacyMultipart = false

  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/api/v1/solves')) {
      usedLegacyMultipart = true
    }
  })
  await page.route('**/api/v1/uploads', async (route) => {
    const request = route.request()
    expect(request.method()).toBe('POST')
    totalSize = Number(request.headers()['upload-length'])
    expect(request.headers()['tus-resumable']).toBe('1.0.0')
    expect(request.headers()['x-seiza-client']).toBe('web')
    expect(request.headers()['upload-metadata']).toContain('filename ')
    await route.fulfill({
      status: 201,
      headers: {
        Location: `/api/v1/uploads/${uploadId}`,
        'Tus-Resumable': '1.0.0',
        'Upload-Offset': '0',
      },
    })
  })
  await page.route(`**/api/v1/uploads/${uploadId}`, async (route) => {
    const request = route.request()
    if (request.method() === 'HEAD') {
      await route.fulfill({
        status: 200,
        headers: {
          'Tus-Resumable': '1.0.0',
          'Upload-Length': String(totalSize),
          'Upload-Offset': String(offset),
        },
      })
      return
    }
    expect(request.method()).toBe('PATCH')
    expect(Number(request.headers()['upload-offset'])).toBe(offset)
    expect(request.headers()['content-type']).toBe('application/offset+octet-stream')
    expect(request.headers()['x-seiza-client']).toBe('web')
    const interceptedSize = request.postDataBuffer()?.length ?? 0
    // Playwright WebKit exposes Blob-backed routed PATCH bodies as empty.
    // Chromium still verifies their exact bytes; WebKit verifies the request
    // offsets and count while the mock supplies the expected server advance.
    if (browserName === 'chromium') expect(interceptedSize).toBeGreaterThan(0)
    const size = interceptedSize || Math.min(chunkSize, totalSize - offset)
    chunks.push({ offset, size })
    offset += size
    await route.fulfill({
      status: 204,
      headers: {
        'Tus-Resumable': '1.0.0',
        'Upload-Offset': String(offset),
      },
    })
  })
  const job = queuedJob(publicId, 'large-field.fits')
  await page.route(`**/api/v1/uploads/${uploadId}/result`, async (route) => {
    expect(route.request().headers()['x-seiza-client']).toBe('web')
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify(job),
    })
  })
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))

  await page.goto('/solve')
  await setStableFile(page, 'large-field.fits', resumableFileSize)
  await page.getByRole('button', { name: 'Solve', exact: true }).click()

  await expect(page).toHaveURL(`/solutions/${publicId}`)
  await expect(page.getByRole('heading', { name: 'Waiting in the queue.' })).toBeVisible()
  await expect(page.locator('main.solution-page')).not.toHaveClass(/solution-page-settled/)
  expect(usedLegacyMultipart).toBe(false)
  expect(chunks).toEqual([
    { offset: 0, size: chunkSize },
    { offset: chunkSize, size: oneMiB },
  ])
})

test('submitting the same completed file creates a new upload and solve job', async ({ page }) => {
  const uploadIds = [
    '11111111-1111-4111-8111-111111111111',
    '22222222-2222-4222-8222-222222222222',
  ]
  const jobIds = [
    '11111111-1111-4111-8111-111111111111',
    '22222222-2222-4222-8222-222222222222',
  ]
  const sizes = new Map<string, number>()
  const offsets = new Map<string, number>()
  let creations = 0

  await page.route(/\/api\/v1\/uploads$/, async (route) => {
    const index = creations++
    const id = uploadIds[index]
    expect(id).toBeDefined()
    const size = Number(route.request().headers()['upload-length'])
    sizes.set(id, size)
    offsets.set(id, 0)
    await route.fulfill({
      status: 201,
      headers: {
        Location: `/api/v1/uploads/${id}`,
        'Tus-Resumable': '1.0.0',
        'Upload-Offset': '0',
      },
    })
  })
  await page.route(/\/api\/v1\/uploads\/[^/]+(?:\/result)?$/, async (route) => {
    const request = route.request()
    const parts = new URL(request.url()).pathname.split('/')
    const id = parts[parts.length - 1] === 'result' ? parts[parts.length - 2] : parts[parts.length - 1]
    const index = uploadIds.indexOf(id)
    if (parts[parts.length - 1] === 'result') {
      await route.fulfill({ contentType: 'application/json', body: JSON.stringify(queuedJob(jobIds[index], 'repeat.fits')) })
      return
    }
    if (request.method() === 'HEAD') {
      await route.fulfill({
        status: 200,
        headers: {
          'Tus-Resumable': '1.0.0',
          'Upload-Length': String(sizes.get(id)),
          'Upload-Offset': String(offsets.get(id)),
        },
      })
      return
    }
    expect(request.method()).toBe('PATCH')
    const nextOffset = (offsets.get(id) ?? 0) + (request.postDataBuffer()?.length ?? sizes.get(id) ?? 0)
    offsets.set(id, nextOffset)
    await route.fulfill({
      status: 204,
      headers: { 'Tus-Resumable': '1.0.0', 'Upload-Offset': String(nextOffset) },
    })
  })
  await page.route('**/api/v1/solves/**', async (route) => {
    const id = new URL(route.request().url()).pathname.split('/').pop()!
    await route.fulfill({ contentType: 'application/json', body: JSON.stringify(queuedJob(id, 'repeat.fits')) })
  })

  for (const jobId of jobIds) {
    await page.goto('/solve')
    await setStableFile(page, 'repeat.fits', 1024)
    await page.getByRole('button', { name: 'Solve', exact: true }).click()
    await expect(page).toHaveURL(`/solutions/${jobId}`)
  }
  expect(creations).toBe(2)
})

test('changing settings does not resume an interrupted upload with stale metadata', async ({ page }) => {
  const uploadIds = [
    '33333333-3333-4333-8333-333333333333',
    '44444444-4444-4444-8444-444444444444',
  ]
  const newJobId = '44444444-4444-4444-8444-444444444444'
  let creations = 0

  await page.route(/\/api\/v1\/uploads$/, async (route) => {
    const id = uploadIds[creations++]
    expect(id).toBeDefined()
    await route.fulfill({
      status: 201,
      headers: {
        Location: `/api/v1/uploads/${id}`,
        'Tus-Resumable': '1.0.0',
        'Upload-Offset': '0',
      },
    })
  })
  await page.route(/\/api\/v1\/uploads\/[^/]+(?:\/result)?$/, async (route) => {
    const request = route.request()
    const parts = new URL(request.url()).pathname.split('/')
    const id = parts[parts.length - 1] === 'result' ? parts[parts.length - 2] : parts[parts.length - 1]
    if (parts[parts.length - 1] === 'result') {
      await route.fulfill({ contentType: 'application/json', body: JSON.stringify(queuedJob(newJobId, 'settings.fits')) })
      return
    }
    if (request.method() === 'HEAD') {
      await route.fulfill({
        status: 200,
        headers: { 'Tus-Resumable': '1.0.0', 'Upload-Length': '1024', 'Upload-Offset': '0' },
      })
      return
    }
    if (id === uploadIds[0]) {
      await route.fulfill({ status: 400, headers: { 'Tus-Resumable': '1.0.0' }, body: 'test interruption' })
      return
    }
    await route.fulfill({
      status: 204,
      headers: { 'Tus-Resumable': '1.0.0', 'Upload-Offset': '1024' },
    })
  })
  await page.route(`**/api/v1/solves/${newJobId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(queuedJob(newJobId, 'settings.fits')),
  }))

  await page.goto('/solve')
  await setStableFile(page, 'settings.fits', 1024)
  await page.getByRole('button', { name: 'Solve', exact: true }).click()
  await expect(page.getByRole('alert')).toBeVisible()

  await page.locator('summary').click()
  await page.getByLabel('Minimum scale (arcsec/px)').fill('0.4')
  await page.getByRole('button', { name: 'Solve', exact: true }).click()
  await expect(page).toHaveURL(`/solutions/${newJobId}`)
  expect(creations).toBe(2)
})

test('re-solves a failed retained image under a new URL without uploading it again', async ({ page }) => {
  let uploadRequests = 0
  let retryRequests = 0
  const resolvedId = '77777777-7777-4777-8777-777777777777'
  const failed = {
    ...queuedJob(publicId, 'failed-field.jpg'),
    status: 'failed',
    completed_at: '2026-07-14T02:01:00Z',
    options: {
      ...defaultSolveOptions(),
      sigma: 5.5,
      ignore_border: 12,
      max_stars: 300,
    },
    error: 'blind solve did not converge',
  }
  const resolved = queuedJob(resolvedId, 'failed-field.jpg')

  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/api/v1/uploads')) {
      uploadRequests += 1
    }
  })
  await page.route('**/api/v1/solves/**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === `/api/v1/solves/${publicId}/resolve`) {
      expect(request.method()).toBe('POST')
      const options = request.postDataJSON()
      expect(options).toMatchObject({
        center_ra_deg: 202.47,
        center_dec_deg: 47.2,
        scale_arcsec_per_pixel: 1.35,
        radius_deg: 3,
        capture_time: '2026-07-16T12:30:00.000Z',
        sip_order: 3,
        sigma: 5.5,
        ignore_border: 12,
        max_stars: 300,
      })
      retryRequests += 1
      await route.fulfill({
        status: 202,
        contentType: 'application/json',
        body: JSON.stringify({
          ...resolved,
          options: { ...defaultSolveOptions(), ...options },
        }),
      })
      return
    }
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify(path === `/api/v1/solves/${resolvedId}` ? resolved : failed),
    })
  })

  await page.goto(`/solutions/${publicId}`)
  await expect(page.getByRole('heading', { name: 'The solve did not converge.' })).toBeVisible()
  await expect(page.getByText('No re-upload')).toBeVisible()
  await expect(page.getByText('No coordinates are required.')).toBeVisible()
  await page.getByLabel('RA (degrees)').fill('202.47')
  await page.getByLabel('Dec (degrees)').fill('47.2')
  await page.getByLabel('Pixel scale (arcsec/px)').fill('1.35')
  await page.getByLabel('Search radius (degrees)').fill('3')
  await page.getByText('Advanced solve controls').click()
  await page.getByLabel('SIP distortion order').selectOption('3')
  await page.getByLabel('Time zone').selectOption('utc')
  await page.getByLabel('Date and time').fill('2026-07-16T12:30')
  await page.getByRole('button', { name: 'Re-solve retained image' }).click()

  await expect(page).toHaveURL(`/solutions/${resolvedId}`)
  await expect(page.getByRole('heading', { name: 'Waiting in the queue.' })).toBeVisible()
  expect(retryRequests).toBe(1)
  expect(uploadRequests).toBe(0)

  await page.goBack()
  await expect(page).toHaveURL(`/solutions/${publicId}`)
  await expect(page.getByRole('heading', { name: 'The solve did not converge.' })).toBeVisible()
})

test('keeps successful retained-image re-solving collapsed at the bottom', async ({ page }) => {
  const resolvedId = '88888888-8888-4888-8888-888888888888'
  const solved = solvedJob(publicId, 'solved-field.fits')
  const resolved = queuedJob(resolvedId, 'solved-field.fits')
  let retryRequests = 0

  await page.route('**/api/v1/solves/**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === `/api/v1/solves/${publicId}/resolve`) {
      retryRequests += 1
      await route.fulfill({ status: 202, contentType: 'application/json', body: JSON.stringify(resolved) })
      return
    }
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify(path === `/api/v1/solves/${resolvedId}` ? resolved : solved),
    })
  })

  await page.goto(`/solutions/${publicId}`)
  const reSolveDetails = page.getByText('Re-solve this retained image with different settings')
  await expect(reSolveDetails).toBeVisible()
  await expect(page.getByRole('button', { name: 'Re-solve retained image' })).toBeHidden()
  await reSolveDetails.click()
  await expect(page.getByRole('button', { name: 'Re-solve retained image' })).toBeVisible()
  await page.getByRole('button', { name: 'Re-solve retained image' }).click()

  await expect(page).toHaveURL(`/solutions/${resolvedId}`)
  await expect(page.getByRole('heading', { name: 'Waiting in the queue.' })).toBeVisible()
  expect(retryRequests).toBe(1)
})

for (const status of ['succeeded', 'failed'] as const) {
  test(`contributes a ${status} solve to the validation set with an explicit image grant`, async ({ page }) => {
    let donationRequests = 0
    let current = {
      ...queuedJob(publicId, `${status}-validation.jpg`),
      status,
      completed_at: '2026-07-14T02:01:00Z',
      error: status === 'failed' ? 'blind solve did not converge' : null,
      validation_donation: null as null | {
        comment: string
        solve_is_invalid: boolean
        license_version: string
        donated_at: string
      },
    }

    await page.route(`**/api/v1/solves/${publicId}**`, async (route) => {
      const request = route.request()
      if (new URL(request.url()).pathname.endsWith('/validation-donation')) {
        expect(request.method()).toBe('POST')
        expect(request.postDataJSON()).toEqual({
          comment: 'Useful sparse-field regression image',
          solve_is_invalid: true,
          license_agreed: true,
        })
        donationRequests += 1
        current = {
          ...current,
          validation_donation: {
            comment: 'Useful sparse-field regression image',
            solve_is_invalid: true,
            license_version: 'seiza-validation-image-grant-v2',
            donated_at: '2026-07-14T02:05:00Z',
          },
        }
      }
      await route.fulfill({
        contentType: 'application/json',
        body: JSON.stringify(current),
      })
    })

    await page.goto(`/solutions/${publicId}`)
    const donationCta = page.locator('#validation-donation')
    const donationDetails = donationCta.locator('details')
    await expect(donationCta.getByText('Help improve Seiza with this image')).toBeVisible()
    await expect(page.getByLabel('Optional comment')).toBeHidden()
    await donationCta.locator('summary').click()
    await expect(donationDetails).toHaveAttribute('open', '')
    await expect(page.getByText('I attest that I own this image or have authority to contribute it.')).toBeVisible()
    await expect(donationCta).toContainText('only to test, validate, debug, and improve the Seiza plate solver')
    await expect(donationCta).not.toContainText('any purpose')
    await page.getByLabel('Optional comment').fill('Useful sparse-field regression image')
    await page.getByLabel('Mark this solve result as invalid').check()
    await page.getByLabel('I attest that I own this image or have authority to contribute it.').check()
    await page.getByRole('button', { name: 'Contribute image for validation' }).click()

    await expect(page.getByText('Contributed to Seiza’s validation set')).toBeVisible()
    await expect(page.getByText('contributed for long-term validation')).toBeVisible()
    await expect(page.getByText('This result was marked invalid for validation.')).toBeVisible()
    await expect(page.getByText('Useful sparse-field regression image')).toBeVisible()
    expect(donationRequests).toBe(1)
  })
}
