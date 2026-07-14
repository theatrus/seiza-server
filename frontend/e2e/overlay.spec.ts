import { expect, test, type Page } from '@playwright/test'
import { readFile } from 'node:fs/promises'
import { fileURLToPath } from 'node:url'

const previewPath = fileURLToPath(new URL('../public/seiza-mark.png', import.meta.url))
const publicId = '42-550e8400-e29b-41d4-a716-446655440000'

const baseObjects = [
  {
    name: 'M 31', common_name: 'Andromeda Galaxy', kind: 'galaxy', mag: 3.4,
    x: 520, y: 510, semi_major_px: 160, semi_minor_px: 55, angle_deg: 32,
    source: 'deep_sky', ra_deg: 10.68, dec_deg: 41.27,
  },
  {
    name: 'HIP 123', common_name: 'Alpheratz', kind: 'star', mag: 2.1,
    x: 270, y: 330, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'deep_sky', ra_deg: 2.1, dec_deg: 29.1,
  },
  {
    name: '', common_name: '', kind: 'field-star', mag: 8.2,
    x: 700, y: 250, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'star_catalog', ra_deg: 10.4, dec_deg: 41.5,
  },
  {
    name: 'SN 2020abc', common_name: 'type II, disc. 2020/01/02, in M 31', kind: 'transient', mag: 17.2,
    x: 610, y: 570, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'transient', discovered: '2020-01-02', near_capture: false,
  },
  {
    name: 'C/2026 A1', common_name: 'V~9.2, 0.84 AU', kind: 'comet', mag: 9.2,
    x: 420, y: 210, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'minor_body', distance_au: 0.84, direction_pa_deg: 45, direction_angle_deg: 18,
  },
]

const solution = {
  center_ra_deg: 10.6847,
  center_dec_deg: 41.269,
  pixel_scale_arcsec_per_pixel: 3.6,
  matched_stars: 42,
  rms_arcsec: 0.41,
  image_width: 1024,
  image_height: 1024,
  wcs: {
    crval: [10.6847, 41.269],
    crpix: [512, 512],
    cd: [[-0.001, 0], [0, -0.001]],
    ctype: ['RA---TAN', 'DEC--TAN'],
    cunit: ['deg', 'deg'],
    radesys: 'ICRS',
    equinox: 2000,
  },
  footprint: [[11.36, 41.78], [10.01, 41.78], [10.02, 40.75], [11.35, 40.75]],
  objects: baseObjects.filter((object) => object.kind !== 'field-star' && object.near_capture !== false),
  catalog_version: 'objects:test;stars:test',
  capture_time: '2026-07-13T04:05:06Z',
}

async function mockSolution(page: Page, inputAvailable = true) {
  await page.route(`**/api/v1/solves/${publicId}/annotations**`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      job_id: publicId,
      catalog_version: 'objects:test;stars:test',
      capture_time: '2026-07-13T04:05:06Z',
      counts: { deep_sky: 1, named_stars: 1, field_stars: 1, transients: 1, historical_transients: 1, minor_bodies: 1 },
      objects: baseObjects,
    }),
  }))
  await page.route(`**/api/v1/solves/${publicId}/preview**`, async (route) => route.fulfill({
    contentType: 'image/png',
    path: previewPath,
  }))
  await page.route(`**/api/v1/solves/${publicId}`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      id: publicId,
      status: 'succeeded',
      created_at: '2026-07-13T04:00:00Z',
      started_at: '2026-07-13T04:00:01Z',
      completed_at: '2026-07-13T04:00:03Z',
      original_filename: 'M31.fits',
      input_expires_at: '2026-07-14T04:00:00Z',
      input_available: inputAvailable,
      preview_url: inputAvailable ? `/api/v1/solves/${publicId}/preview` : null,
      overlay_url: inputAvailable ? `/api/v1/solves/${publicId}/overlay.svg` : null,
      annotations_url: `/api/v1/solves/${publicId}/annotations`,
      wcs_url: `/api/v1/solves/${publicId}/wcs`,
      solution,
      error: null,
    }),
  }))
}

test('keeps the interactive SVG aligned and filters annotation layers', async ({ page }) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)
  await expect(page.getByRole('heading', { name: 'Explore the solved field' })).toBeVisible()

  const imageBox = await page.locator('.sky-frame img').boundingBox()
  const overlayBox = await page.locator('.sky-overlay').boundingBox()
  expect(imageBox).not.toBeNull()
  expect(overlayBox).not.toBeNull()
  expect(Math.abs(imageBox!.x - overlayBox!.x)).toBeLessThan(1)
  expect(Math.abs(imageBox!.y - overlayBox!.y)).toBeLessThan(1)
  expect(Math.abs(imageBox!.width - overlayBox!.width)).toBeLessThan(1)
  expect(Math.abs(imageBox!.height - overlayBox!.height)).toBeLessThan(1)

  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(1)
  await expect(page.locator('.field-stars circle')).toHaveCount(0)
  await expect(page.getByText('SN 2020abc · type II', { exact: false })).toHaveCount(0)

  await page.getByRole('button', { name: /Field stars/ }).click()
  await expect(page.locator('.field-stars circle')).toHaveCount(1)
  await page.getByRole('button', { name: /Older transients/ }).click()
  await expect(page.getByText('SN 2020abc · type II', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: /Deep sky/ }).click()
  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(0)

  await page.getByRole('button', { name: 'Expand image' }).click()
  await expect(page.locator('.image-stage')).toHaveClass(/expanded/)
  const expandedImage = await page.locator('.sky-frame img').boundingBox()
  const expandedOverlay = await page.locator('.sky-overlay').boundingBox()
  expect(Math.abs(expandedImage!.width - expandedOverlay!.width)).toBeLessThan(1)
  expect(Math.abs(expandedImage!.height - expandedOverlay!.height)).toBeLessThan(1)
  await page.getByRole('button', { name: 'Close' }).click()
})

test('downloads a rendered PNG with the current overlay', async ({ page }) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)
  const downloadPromise = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Download rendered PNG' }).click()
  const download = await downloadPromise
  expect(download.suggestedFilename()).toBe(`seiza-solution-${publicId}.png`)
  expect(await download.failure()).toBeNull()
  const path = await download.path()
  expect(path).not.toBeNull()
  const png = await readFile(path!)
  expect([...png.subarray(0, 8)]).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
  expect(png.readUInt32BE(16)).toBe(solution.image_width)
  expect(png.readUInt32BE(20)).toBe(solution.image_height)
})

test('keeps calibration and object metadata after the image expires', async ({ page }) => {
  await mockSolution(page, false)
  await page.goto(`/solutions/${publicId}`)
  await expect(page.getByText(/expired and deleted/i)).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Complete WCS calibration' })).toBeVisible()
  await page.getByText(/catalog objects in field/).click()
  await expect(page.getByText('Andromeda Galaxy', { exact: true })).toBeVisible()
  await expect(page.locator('.image-stage')).toHaveCount(0)
})

test('does not treat sequential job numbers as solution URLs', async ({ page }) => {
  const nativeRequests: string[] = []
  page.on('request', (request) => {
    if (request.url().includes('/api/v1/solves/')) nativeRequests.push(request.url())
  })
  await page.goto('/solutions/42')
  await expect(page.getByRole('heading', { name: 'This point is off the chart.' })).toBeVisible()
  expect(nativeRequests).toEqual([])
})
