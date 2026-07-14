import { expect, test, type Page } from '@playwright/test'

const publicId = '91-550e8400-e29b-41d4-a716-446655440000'
const uploadId = '8c741b20-3c42-4e75-95d4-fbc87cc68730'
const chunkSize = 5 * 1024 * 1024

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
    input_expires_at: '2026-07-15T02:00:00Z',
    input_available: true,
    preview_url: null,
    overlay_url: null,
    annotations_url: null,
    wcs_url: null,
    solution: null,
    error: null,
  }
}

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

  await page.getByText('Blind solve settings', { exact: true }).click()
  await page.getByLabel('Minimum scale (arcsec/px)').fill('0.4')
  await page.getByRole('button', { name: 'Queue solve' }).click()
  await expect(page).toHaveURL(`/solutions/${newJobId}`)
  expect(creations).toBe(2)
})
