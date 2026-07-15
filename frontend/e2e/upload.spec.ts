import { expect, test, type Page } from '@playwright/test'

const publicId = '91-550e8400-e29b-41d4-a716-446655440000'
const uploadId = '8c741b20-3c42-4e75-95d4-fbc87cc68730'
const chunkSize = 5 * 1024 * 1024

interface UploadConcurrencyState {
  active: number
  maxActive: number
}

type InstrumentedWindow = Window & {
  __seizaUploadConcurrency?: UploadConcurrencyState
}

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
    capture_time: null,
  }
}

async function setStableFile(page: Page, name: string, size: number) {
  await page.getByLabel('FITS or image file').evaluate((node, file) => {
    const input = node as HTMLInputElement
    const bytes = new Uint8Array(file.size)
    bytes.fill(42)
    const transfer = new DataTransfer()
    transfer.items.add(new File([bytes], file.name, {
      type: 'application/fits',
      lastModified: 1_720_000_000_000,
    }))
    input.files = transfer.files
    input.dispatchEvent(new Event('input', { bubbles: true }))
    input.dispatchEvent(new Event('change', { bubbles: true }))
  }, { name, size })
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
        'Upload-Offset': String(12 * 1024 * 1024),
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
  await page.route(`**/api/v1/uploads/${finalId}/result`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))

  await page.goto('/solve')
  await page.getByLabel('FITS or image file').setInputFiles({
    name: 'parallel-field.fits',
    mimeType: 'application/fits',
    buffer: Buffer.alloc(12 * 1024 * 1024, 42),
  })
  await page.getByRole('button', { name: 'Queue solve' }).click()

  await expect(page).toHaveURL(`/solutions/${publicId}`)
  expect(partialCreations).toBe(3)
  expect(finalCreations).toBe(1)
  const uploadConcurrency = await page.evaluate(
    () => (window as InstrumentedWindow).__seizaUploadConcurrency?.maxActive ?? 0,
  )
  expect(uploadConcurrency).toBeGreaterThan(1)
  expect([...partLengths.values()]).toEqual([
    4 * 1024 * 1024,
    4 * 1024 * 1024,
    4 * 1024 * 1024,
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
  await page.route(`**/api/v1/uploads/${uploadId}/result`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(job),
  }))

  await page.goto('/solve')
  await page.getByLabel('FITS or image file').setInputFiles({
    name: 'large-field.fits',
    mimeType: 'application/fits',
    buffer: Buffer.alloc(6 * 1024 * 1024, 42),
  })
  await page.getByRole('button', { name: 'Queue solve' }).click()

  await expect(page).toHaveURL(`/solutions/${publicId}`)
  await expect(page.getByRole('heading', { name: 'Waiting in the queue.' })).toBeVisible()
  expect(usedLegacyMultipart).toBe(false)
  expect(chunks).toEqual([
    { offset: 0, size: chunkSize },
    { offset: chunkSize, size: 1024 * 1024 },
  ])
})

test('submitting the same completed file creates a new upload and solve job', async ({ page }) => {
  const uploadIds = [
    '11111111-1111-4111-8111-111111111111',
    '22222222-2222-4222-8222-222222222222',
  ]
  const jobIds = [
    '101-11111111-1111-4111-8111-111111111111',
    '102-22222222-2222-4222-8222-222222222222',
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
    await page.getByRole('button', { name: 'Queue solve' }).click()
    await expect(page).toHaveURL(`/solutions/${jobId}`)
  }
  expect(creations).toBe(2)
})

test('changing settings does not resume an interrupted upload with stale metadata', async ({ page }) => {
  const uploadIds = [
    '33333333-3333-4333-8333-333333333333',
    '44444444-4444-4444-8444-444444444444',
  ]
  const newJobId = '104-44444444-4444-4444-8444-444444444444'
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
  await page.getByRole('button', { name: 'Queue solve' }).click()
  await expect(page.getByRole('alert')).toBeVisible()

  await page.locator('summary').click()
  await page.getByLabel('Minimum scale (arcsec/px)').fill('0.4')
  await page.getByRole('button', { name: 'Queue solve' }).click()
  await expect(page).toHaveURL(`/solutions/${newJobId}`)
  expect(creations).toBe(2)
})

test('retries a failed retained image with hints without uploading it again', async ({ page }) => {
  let uploadRequests = 0
  let retryRequests = 0
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
  let current = failed

  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/api/v1/uploads')) {
      uploadRequests += 1
    }
  })
  await page.route(`**/api/v1/solves/${publicId}**`, async (route) => {
    const request = route.request()
    if (new URL(request.url()).pathname.endsWith('/retry')) {
      expect(request.method()).toBe('POST')
      const options = request.postDataJSON()
      expect(options).toMatchObject({
        center_ra_deg: 202.47,
        center_dec_deg: 47.2,
        scale_arcsec_per_pixel: 1.35,
        radius_deg: 3,
        sigma: 5.5,
        ignore_border: 12,
        max_stars: 300,
      })
      retryRequests += 1
      current = {
        ...queuedJob(publicId, 'failed-field.jpg'),
        options: { ...defaultSolveOptions(), ...options },
      }
      await route.fulfill({
        status: 202,
        contentType: 'application/json',
        body: JSON.stringify(current),
      })
      return
    }
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify(current),
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
  await page.getByRole('button', { name: 'Retry retained image' }).click()

  await expect(page).toHaveURL(`/solutions/${publicId}`)
  await expect(page.getByRole('heading', { name: 'Waiting in the queue.' })).toBeVisible()
  expect(retryRequests).toBe(1)
  expect(uploadRequests).toBe(0)
})

for (const status of ['succeeded', 'failed'] as const) {
  test(`donates a ${status} solve to the validation set with an explicit image grant`, async ({ page }) => {
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
            license_version: 'seiza-validation-image-grant-v1',
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
    await expect(page.getByRole('heading', { name: 'Donate this image to improve Seiza' })).toBeVisible()
    await page.getByLabel('Optional comment').fill('Useful sparse-field regression image')
    await page.getByLabel('Mark this solve result as invalid').check()
    await page.getByLabel('I own this image or have authority to grant this license.').check()
    await page.getByRole('button', { name: 'Donate image to validation set' }).click()

    await expect(page.getByRole('heading', { name: 'Thank you for donating this image.' })).toBeVisible()
    await expect(page.getByText('donated for long-term validation')).toBeVisible()
    await expect(page.getByText('This result was marked invalid for validation.')).toBeVisible()
    await expect(page.getByText('Useful sparse-field regression image')).toBeVisible()
    expect(donationRequests).toBe(1)
  })
}
