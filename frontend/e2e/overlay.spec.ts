import { expect, test, type Page } from '@playwright/test'
import { readFile } from 'node:fs/promises'

const publicId = '42-550e8400-e29b-41d4-a716-446655440000'
const starFieldSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <radialGradient id="sky" cx="62%" cy="42%" r="70%">
      <stop offset="0" stop-color="#183052"/><stop offset=".42" stop-color="#081526"/><stop offset="1" stop-color="#01050b"/>
    </radialGradient>
    <radialGradient id="nebula" cx="50%" cy="50%" r="50%">
      <stop offset="0" stop-color="#5b8fc7" stop-opacity=".25"/><stop offset="1" stop-color="#14253b" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="1024" height="1024" fill="url(#sky)"/>
  <ellipse cx="565" cy="510" rx="390" ry="210" fill="url(#nebula)" transform="rotate(-18 565 510)"/>
  <g fill="#f4f8ff">
    <circle cx="75" cy="96" r="2"/><circle cx="158" cy="210" r="1.4"/><circle cx="230" cy="73" r="1.2"/>
    <circle cx="317" cy="278" r="2.4"/><circle cx="401" cy="124" r="1.5"/><circle cx="493" cy="237" r="1.1"/>
    <circle cx="608" cy="92" r="2.1"/><circle cx="711" cy="188" r="1.4"/><circle cx="818" cy="74" r="1.8"/>
    <circle cx="932" cy="229" r="1.2"/><circle cx="111" cy="403" r="1.5"/><circle cx="205" cy="536" r="2.2"/>
    <circle cx="348" cy="449" r="1.1"/><circle cx="468" cy="602" r="2.6"/><circle cx="574" cy="408" r="1.4"/>
    <circle cx="693" cy="553" r="1.8"/><circle cx="806" cy="375" r="1.1"/><circle cx="939" cy="487" r="2.3"/>
    <circle cx="84" cy="723" r="1.2"/><circle cx="191" cy="873" r="2"/><circle cx="292" cy="688" r="1.6"/>
    <circle cx="390" cy="835" r="1.1"/><circle cx="526" cy="752" r="2.1"/><circle cx="642" cy="903" r="1.3"/>
    <circle cx="744" cy="707" r="2.4"/><circle cx="866" cy="846" r="1.2"/><circle cx="962" cy="680" r="1.8"/>
  </g>
  <g fill="#a9d8ff"><circle cx="270" cy="355" r="3"/><circle cx="653" cy="326" r="2.8"/><circle cx="781" cy="638" r="3.2"/></g>
</svg>`

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
  {
    name: '(12345)', common_name: 'Test asteroid', kind: 'asteroid', mag: 14.1,
    x: 760, y: 700, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'minor_body', distance_au: 1.42, direction_pa_deg: 122, direction_angle_deg: 136,
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
      available: { deep_sky: true, named_stars: true, field_stars: true, transients: true, historical_transients: true, minor_bodies: true, grid: true },
      counts: { deep_sky: 1, named_stars: 1, field_stars: 1, transients: 1, historical_transients: 1, minor_bodies: 2 },
      objects: baseObjects,
    }),
  }))
  await page.route(`**/api/v1/solves/${publicId}/preview**`, async (route) => route.fulfill({
    contentType: 'image/svg+xml',
    body: starFieldSvg,
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

  const gridLabels = page.locator('.coordinate-grid text')
  await expect(gridLabels.first()).toBeVisible()
  const overlayBounds = await page.locator('.sky-overlay').boundingBox()
  expect(overlayBounds).not.toBeNull()
  const gridLabelBounds = await gridLabels.evaluateAll((labels) => labels.map((label) => {
    const box = (label as SVGGraphicsElement).getBBox()
    const rendered = label.getBoundingClientRect()
    return {
      x: box.x,
      y: box.y,
      right: box.x + box.width,
      bottom: box.y + box.height,
      renderedX: rendered.x,
      renderedY: rendered.y,
      renderedRight: rendered.right,
      renderedBottom: rendered.bottom,
      fontSize: Number.parseFloat(getComputedStyle(label).fontSize),
    }
  }))
  for (const box of gridLabelBounds) {
    expect(box.x).toBeGreaterThanOrEqual(0)
    expect(box.y).toBeGreaterThanOrEqual(0)
    expect(box.right).toBeLessThanOrEqual(solution.image_width)
    expect(box.bottom).toBeLessThanOrEqual(solution.image_height)
    expect(box.renderedX).toBeGreaterThanOrEqual(overlayBounds!.x)
    expect(box.renderedY).toBeGreaterThanOrEqual(overlayBounds!.y)
    expect(box.renderedRight).toBeLessThanOrEqual(overlayBounds!.x + overlayBounds!.width)
    expect(box.renderedBottom).toBeLessThanOrEqual(overlayBounds!.y + overlayBounds!.height)
    expect(box.fontSize).toBeGreaterThanOrEqual(18)
  }

  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(1)
  await expect(page.locator('[data-kind="galaxy"]')).toBeVisible()
  await expect(page.locator('[data-kind="star"]')).toBeVisible()
  await expect(page.locator('[data-kind="comet"]')).toBeVisible()
  await expect(page.locator('[data-kind="asteroid"]')).toBeVisible()
  const cometTail = page.locator('[data-kind="comet"] .comet-tail')
  const asteroidTail = page.locator('[data-kind="asteroid"] .asteroid-tail')
  await expect(cometTail).toHaveAttribute('data-direction-angle', '18')
  await expect(asteroidTail).toHaveAttribute('data-direction-angle', '136')
  expect(await cometTail.evaluate((tail) => (tail as SVGPathElement).getTotalLength())).toBeGreaterThan(100)
  expect(await asteroidTail.evaluate((tail) => (tail as SVGPathElement).getTotalLength())).toBeGreaterThan(75)
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

test('explains and disables catalog layers that are unavailable', async ({ page }) => {
  await mockSolution(page)
  await page.route(`**/api/v1/solves/${publicId}/annotations**`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      job_id: publicId,
      catalog_version: 'stars:test',
      capture_time: null,
      available: { deep_sky: false, named_stars: false, field_stars: true, transients: false, historical_transients: false, minor_bodies: false, grid: true },
      counts: { deep_sky: 0, named_stars: 0, field_stars: 1, transients: 0, historical_transients: 0, minor_bodies: 0 },
      objects: baseObjects.filter((object) => object.kind === 'field-star'),
    }),
  }))
  await page.goto(`/solutions/${publicId}`)

  await expect(page.getByText(/Overlay data unavailable for this solution/)).toContainText('Deep sky, Named stars, Transients, Solar system')
  await expect(page.getByRole('button', { name: 'Deep sky · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Named stars · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Field stars · 1' })).toBeEnabled()
})

test('downloads a branded rendered PNG with the current overlay', async ({ page }, testInfo) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)
  await page.evaluate(() => {
    const originalCreateObjectUrl = URL.createObjectURL.bind(URL)
    const state = window as typeof window & { __seizaSerializedOverlay?: Promise<string> }
    URL.createObjectURL = (object: Blob | MediaSource) => {
      if (object instanceof Blob && object.type.startsWith('image/svg+xml')) {
        state.__seizaSerializedOverlay = object.text()
      }
      return originalCreateObjectUrl(object)
    }
  })
  const watermarkPromise = page.waitForRequest((request) => request.url().includes('seiza-mark.png?watermark=1'))
  const downloadPromise = page.waitForEvent('download')
  await page.getByRole('button', { name: 'Download rendered PNG' }).click()
  await watermarkPromise
  const download = await downloadPromise
  expect(download.suggestedFilename()).toBe(`seiza-solution-${publicId}.png`)
  expect(await download.failure()).toBeNull()
  const path = testInfo.outputPath('seiza-solution-branded.png')
  await download.saveAs(path)
  const png = await readFile(path)
  expect([...png.subarray(0, 8)]).toEqual([137, 80, 78, 71, 13, 10, 26, 10])
  expect(png.readUInt32BE(16)).toBe(solution.image_width)
  expect(png.readUInt32BE(20)).toBe(solution.image_height)

  const renderedLabels = await page.evaluate(async () => {
    const state = window as typeof window & { __seizaSerializedOverlay?: Promise<string> }
    if (!state.__seizaSerializedOverlay) return []
    const markup = await state.__seizaSerializedOverlay
    const parsed = new DOMParser().parseFromString(markup, 'image/svg+xml')
    const host = document.createElement('div')
    host.style.cssText = 'position:absolute;left:-10000px;top:0;'
    const svg = document.importNode(parsed.documentElement, true) as unknown as SVGSVGElement
    host.append(svg)
    document.body.append(host)
    const labels = [...svg.querySelectorAll<SVGTextElement>('.coordinate-grid text')].map((label) => {
      const box = label.getBBox()
      return {
        x: box.x,
        y: box.y,
        right: box.x + box.width,
        bottom: box.y + box.height,
        fontSize: Number.parseFloat(getComputedStyle(label).fontSize),
      }
    })
    host.remove()
    return labels
  })
  expect(renderedLabels.length).toBeGreaterThan(0)
  for (const label of renderedLabels) {
    expect(label.x).toBeGreaterThanOrEqual(0)
    expect(label.y).toBeGreaterThanOrEqual(0)
    expect(label.right).toBeLessThanOrEqual(solution.image_width)
    expect(label.bottom).toBeLessThanOrEqual(solution.image_height)
    expect(label.fontSize).toBeGreaterThanOrEqual(18)
  }
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
