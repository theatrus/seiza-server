import { expect, test } from '@playwright/test'

const publicId = '91-550e8400-e29b-41d4-a716-446655440000'
const uploadId = '8c741b20-3c42-4e75-95d4-fbc87cc68730'
const chunkSize = 5 * 1024 * 1024

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
  const queuedJob = {
    id: publicId,
    status: 'queued',
    created_at: '2026-07-14T02:00:00Z',
    started_at: null,
    completed_at: null,
    original_filename: 'large-field.fits',
    input_expires_at: '2026-07-15T02:00:00Z',
    input_available: true,
    preview_url: null,
    overlay_url: null,
    annotations_url: null,
    wcs_url: null,
    solution: null,
    error: null,
  }
  await page.route(`**/api/v1/uploads/${uploadId}/result`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(queuedJob),
  }))
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify(queuedJob),
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
