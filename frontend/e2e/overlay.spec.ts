import { expect, test, type Page } from '@playwright/test'
import { readFile } from 'node:fs/promises'
import { mockHealth } from './health'

test.beforeEach(async ({ page }) => {
  page.on('pageerror', (error) => console.error(`[page error] ${error.stack ?? error.message}`))
  await mockHealth(page)
})

const publicId = '550e8400-e29b-41d4-a716-446655440000'
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
    stable_id: 'openngc:NGC7000', catalog_source: 'OpenNGC',
    name: 'NGC 7000', common_name: 'North America Nebula', kind: 'hii-region', mag: null,
    x: 390, y: 760, semi_major_px: 54, semi_minor_px: 32, angle_deg: 12,
    source: 'deep_sky', ra_deg: 10.76, dec_deg: 41.04,
  },
  {
    stable_id: 'openngc:IC5070', catalog_source: 'OpenNGC',
    name: 'IC 5070', common_name: 'Pelican Nebula', kind: 'hii-region', mag: null,
    x: 690, y: 760, semi_major_px: 48, semi_minor_px: 28, angle_deg: -16,
    source: 'deep_sky', ra_deg: 10.52, dec_deg: 41.02,
  },
  {
    stable_id: 'openngc:Sh2-101', catalog_source: 'OpenNGC',
    name: 'Sh2-101', common_name: 'Tulip Nebula', kind: 'hii-region', mag: null,
    x: 220, y: 690, semi_major_px: 48, semi_minor_px: 36, angle_deg: 8,
    source: 'deep_sky', ra_deg: 10.92, dec_deg: 41.08,
    outlines: [{
      geometry_id: 'openngc:Sh2-101#outline-1', source_record_id: 'openngc:Sh2-101',
      role: 'brightness-level', quality: 'catalog', level: '1',
      contours: [{ closed: true, points: [[170, 690], [205, 650], [255, 665], [270, 710], [215, 730]] }],
    }],
  },
  {
    name: 'LDN 935', common_name: '', kind: 'dark-nebula', mag: null,
    x: 790, y: 430, semi_major_px: 42, semi_minor_px: 25, angle_deg: null,
    source: 'deep_sky', ra_deg: 10.31, dec_deg: 41.35,
  },
  {
    name: 'PGC 12345', common_name: '', kind: 'galaxy', mag: 14.2,
    x: 820, y: 790, semi_major_px: 24, semi_minor_px: 12, angle_deg: 24,
    source: 'deep_sky', ra_deg: 10.27, dec_deg: 40.99,
  },
  {
    name: 'HIP 123', common_name: 'Alpheratz', kind: 'star', mag: 2.1,
    x: 270, y: 330, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'deep_sky', ra_deg: 2.1, dec_deg: 29.1,
  },
  {
    name: 'RR Lyr', common_name: 'RRAB', kind: 'identified-star', mag: 7.1,
    x: 350, y: 270, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'star_identifiers:General Catalog of Variable Stars', ra_deg: 291.36, dec_deg: 42.78,
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
    source: 'minor_body', distance_au: 0.84, motion_arcsec_per_hour: 72, direction_pa_deg: 45, direction_angle_deg: 18,
  },
  {
    name: '(12345)', common_name: 'Test asteroid', kind: 'asteroid', mag: 14.1,
    x: 760, y: 700, semi_major_px: 0, semi_minor_px: 0, angle_deg: 0,
    source: 'minor_body', distance_au: 1.42, motion_arcsec_per_hour: 36, direction_pa_deg: 122, direction_angle_deg: 136,
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
    ctype: ['RA---TAN-SIP', 'DEC--TAN-SIP'],
    cunit: ['deg', 'deg'],
    radesys: 'ICRS',
    equinox: 2000,
    sip: {
      order: 2,
      a: [[0, 2, 1.2e-7], [1, 1, -2.1e-7], [2, 0, 8.4e-8]],
      b: [[0, 2, -7.5e-8], [1, 1, 1.8e-7], [2, 0, -1.1e-7]],
      ap: [[0, 0, 0], [0, 1, 0], [0, 2, -1.2e-7], [1, 0, 0], [1, 1, 2.1e-7], [2, 0, -8.4e-8]],
      bp: [[0, 0, 0], [0, 1, 0], [0, 2, 7.5e-8], [1, 0, 0], [1, 1, -1.8e-7], [2, 0, 1.1e-7]],
    },
  },
  footprint: [[11.36, 41.78], [10.01, 41.78], [10.02, 40.75], [11.35, 40.75]],
  objects: baseObjects.filter((object) => object.kind !== 'field-star' && object.near_capture !== false),
  catalog_version: 'objects:test;stars:test',
  capture_time: '2026-07-13T04:05:06Z',
  statistics: {
    total_ms: 1234.5,
    decode_ms: 84.2,
    detection_ms: 126.7,
    search_ms: 1018.4,
    mode: 'blind',
    detected_stars: 264,
    catalog_stars: 18_432_991,
    blind_index_patterns: 4_215_772,
  },
}

async function mockSolution(page: Page, inputAvailable = true) {
  await page.route(`**/api/v1/solves/${publicId}/annotations**`, async (route) => {
    expect(new URL(route.request().url()).searchParams.get('satellite_tracks')).toBe('false')
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        job_id: publicId,
        catalog_version: 'objects:test;stars:test',
        capture_time: '2026-07-13T04:05:06Z',
        available: { deep_sky: true, named_stars: true, star_identifiers: true, field_stars: true, transients: true, historical_transients: true, minor_bodies: true, grid: true },
        counts: { deep_sky: 6, named_stars: 1, star_identifiers: 1, field_stars: 1, transients: 1, historical_transients: 1, minor_bodies: 2 },
        objects: baseObjects,
      }),
    })
  })
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
      solve_time_ms: 2000,
      original_filename: 'M31.fits',
      options: {},
      input_expires_at: '2026-07-14T04:00:00Z',
      input_available: inputAvailable,
      preview_url: inputAvailable ? `/api/v1/solves/${publicId}/preview` : null,
      overlay_url: inputAvailable ? `/api/v1/solves/${publicId}/overlay.svg` : null,
      annotations_url: `/api/v1/solves/${publicId}/annotations`,
      wcs_url: `/api/v1/solves/${publicId}/wcs`,
      solution,
      error: null,
      validation_donation: null,
    }),
  }))
}

test('reports total solve time and durable solver nerd stats', async ({ page }) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)

  await expect(page.locator('main.solution-page')).toHaveClass(/solution-page-settled/)
  const overlayBox = await page.locator('.overlay-card').boundingBox()
  expect(overlayBox).not.toBeNull()
  expect(overlayBox!.y).toBeLessThan(400)
  const imageStageBox = await page.locator('.image-stage').boundingBox()
  expect(imageStageBox).not.toBeNull()
  expect(imageStageBox!.y).toBeLessThan(360)
  await expect(page.getByText('Total solve time').locator('..')).toContainText('2.00 s')
  const nerdStats = page.locator('.solver-stats')
  await expect(nerdStats).toContainText('1.23 s')
  await expect(nerdStats).toContainText('Blind solve')
  await expect(nerdStats).toContainText('264')
  await expect(nerdStats).toContainText('42/264 · 15.9%')
  await expect(nerdStats).toContainText('4,215,772 blind-index patterns')
})

test('reports durable SIP distortion and coefficient records', async ({ page }) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)

  await expect(page.getByText(/SIP order 2/)).toContainText('6 forward + 12 inverse coefficients')
  await page.getByText('SIP coefficient records').click()
  await expect(page.getByText(/A_0_2 = 1\.200000000000e-7/)).toBeVisible()
  await expect(page.getByText(/AP_0_2 = -1\.200000000000e-7/)).toBeVisible()
})

test('keeps the interactive SVG aligned and filters annotation layers', async ({ page }) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)
  await expect(page.locator('.overlay-card .eyebrow')).toHaveText('SKY OVERLAY')
  await expect(page.getByRole('heading', { name: 'Explore the solved field' })).toHaveCount(0)
  await expect(page.locator('#validation-donation').getByText('Help improve Seiza with this image')).toBeVisible()
  await expect(page.getByLabel('Optional comment')).toBeHidden()
  await expect(page.getByRole('link', { name: 'Contribute this solved image' })).toHaveAttribute('href', '#validation-donation')

  const layerButtonRows = await page.locator('.overlay-options > button').evaluateAll((buttons) =>
    [...new Set(buttons.map((button) => Math.round(button.getBoundingClientRect().top)))],
  )
  expect(layerButtonRows).toHaveLength(1)
  await expect(page.locator('.overlay-options .catalog-filter-trigger')).toHaveCount(0)
  const controlBackgrounds = await page.locator('.overlay-control-row').evaluate((row) => ({
    deepSky: getComputedStyle(row.querySelector('.overlay-options > button')!).backgroundColor,
    catalogFilter: getComputedStyle(row.querySelector('.catalog-filter-trigger')!).backgroundColor,
  }))
  expect(controlBackgrounds.catalogFilter).not.toBe(controlBackgrounds.deepSky)

  const imageBox = await page.locator('.sky-frame img').boundingBox()
  const overlayBox = await page.locator('.sky-overlay').boundingBox()
  const overlayActionsBox = await page.locator('.overlay-actions-below').boundingBox()
  expect(imageBox).not.toBeNull()
  expect(overlayBox).not.toBeNull()
  expect(overlayActionsBox).not.toBeNull()
  expect(overlayActionsBox!.y).toBeGreaterThanOrEqual(imageBox!.y + imageBox!.height)
  const overlayFooter = page.locator('.overlay-footer')
  await expect(overlayFooter.locator('.retention-note')).toBeVisible()
  await expect(overlayFooter.locator('.overlay-actions-below')).toBeVisible()
  expect(Math.abs(imageBox!.x - overlayBox!.x)).toBeLessThan(1)
  expect(Math.abs(imageBox!.y - overlayBox!.y)).toBeLessThan(1)
  expect(Math.abs(imageBox!.width - overlayBox!.width)).toBeLessThan(1)
  expect(Math.abs(imageBox!.height - overlayBox!.height)).toBeLessThan(1)

  const visualWeights = await page.locator('.sky-overlay').evaluate((overlay) => {
    const label = overlay.querySelector<SVGTextElement>('.seiza-overlay__label')
    const gridLabel = overlay.querySelector<SVGTextElement>('.seiza-overlay__grid-label')
    const marker = overlay.querySelector<SVGGeometryElement>('.seiza-overlay__marker')
    const gridLine = overlay.querySelector<SVGGeometryElement>('.seiza-overlay__grid-line')
    return {
      labelFontWeight: label ? getComputedStyle(label).fontWeight : '',
      gridFontWeight: gridLabel ? getComputedStyle(gridLabel).fontWeight : '',
      markerStrokeWidth: marker ? Number.parseFloat(getComputedStyle(marker).strokeWidth) : 0,
      gridStrokeWidth: gridLine ? Number.parseFloat(getComputedStyle(gridLine).strokeWidth) : 0,
    }
  })
  expect(visualWeights).toEqual({
    labelFontWeight: '400',
    gridFontWeight: '500',
    markerStrokeWidth: 0.7,
    gridStrokeWidth: 0.65,
  })

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

  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(5)
  await expect(page.locator('.seiza-overlay__marker--outline')).toHaveCount(1)
  const catalogMarkers = {
    messier: page.locator('[data-layer="deep-sky:messier"] .seiza-overlay__marker'),
    ngc: page.locator('[data-layer="deep-sky:ngc"] .seiza-overlay__marker'),
    ic: page.locator('[data-layer="deep-sky:ic"] .seiza-overlay__marker'),
    sharpless: page.locator('[data-layer="deep-sky:sharpless-vdb"] .seiza-overlay__marker'),
    darkNebula: page.locator('[data-layer="deep-sky:dark-nebulae"] .seiza-overlay__marker'),
    pgc: page.locator('[data-layer="deep-sky:pgc"] .seiza-overlay__marker'),
  }
  await expect(catalogMarkers.messier).toHaveCSS('stroke', 'rgb(242, 202, 114)')
  await expect(catalogMarkers.ngc).toHaveCSS('stroke', 'rgb(85, 207, 255)')
  await expect(catalogMarkers.ic).toHaveCSS('stroke', 'rgb(114, 223, 185)')
  await expect(catalogMarkers.sharpless).toHaveCSS('stroke', 'rgb(238, 154, 120)')
  await expect(catalogMarkers.darkNebula).toHaveCSS('stroke', 'rgb(180, 163, 240)')
  await expect(catalogMarkers.pgc).toHaveCSS('stroke', 'rgb(161, 174, 216)')
  const unorientedExtent = page.locator('[data-kind="dark-nebula"] ellipse')
  expect(await unorientedExtent.getAttribute('rx')).toBe(await unorientedExtent.getAttribute('ry'))
  await expect(page.locator('[data-kind="galaxy"]')).toHaveCount(2)
  await expect(page.locator('[data-kind="star"]')).toBeVisible()
  await expect(page.locator('[data-kind="identified-star"]')).toHaveCount(0)
  await expect(page.locator('[data-kind="comet"]')).toBeVisible()
  await expect(page.locator('[data-kind="asteroid"]')).toBeVisible()
  const cometTail = page.locator('[data-kind="comet"] .comet-tail')
  const asteroidTail = page.locator('[data-kind="asteroid"] .asteroid-tail')
  await expect(cometTail).toHaveAttribute('data-direction-angle', '18')
  await expect(asteroidTail).toHaveAttribute('data-direction-angle', '136')
  await expect(cometTail).toHaveAttribute('data-motion-arcsec-per-hour', '72')
  await expect(cometTail).toHaveAttribute('data-motion-vector-length', '60')
  await expect(asteroidTail).toHaveAttribute('data-motion-arcsec-per-hour', '36')
  await expect(asteroidTail).toHaveAttribute('data-motion-vector-length', '42')
  const cometTailLength = await cometTail.evaluate((tail) => (tail as SVGPathElement).getTotalLength())
  const asteroidTailLength = await asteroidTail.evaluate((tail) => (tail as SVGPathElement).getTotalLength())
  expect(cometTailLength).toBeGreaterThan(asteroidTailLength)
  await expect(page.locator('.field-stars circle')).toHaveCount(0)
  await expect(page.getByText('SN 2020abc · type II', { exact: false })).toHaveCount(0)

  await page.getByRole('button', { name: /Star identifiers/ }).click()
  await expect(page.locator('[data-kind="identified-star"]')).toBeVisible()
  await expect(page.getByText('RR Lyr · RRAB', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: /Field stars/ }).click()
  await expect(page.locator('.field-stars circle')).toHaveCount(1)
  await page.getByRole('button', { name: /Older transients/ }).click()
  await expect(page.getByText('SN 2020abc · type II', { exact: false })).toBeVisible()

  await page.getByRole('button', { name: /Filter catalogs 6\/6/ }).click()
  await expect(page.getByRole('group', { name: 'Deep sky catalogs' })).toBeVisible()
  await expect(page.getByRole('checkbox', { name: 'Messier · 1' })).toBeChecked()
  await expect(page.getByRole('checkbox', { name: 'NGC · 1' })).toBeChecked()
  await expect(page.getByRole('checkbox', { name: 'IC · 1' })).toBeChecked()
  await expect(page.getByRole('checkbox', { name: 'Sharpless / vdB · 1' })).toBeChecked()
  await expect(page.getByRole('checkbox', { name: 'Dark nebulae (B / LDN) · 1' })).toBeChecked()
  const detailedOutlines = page.getByRole('checkbox', { name: 'Detailed OpenNGC outlines' })
  await expect(detailedOutlines).toBeChecked()
  await detailedOutlines.uncheck()
  await expect(page.locator('.seiza-overlay__marker--outline')).toHaveCount(0)
  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(6)
  await detailedOutlines.check()
  await expect(page.locator('.seiza-overlay__marker--outline')).toHaveCount(1)
  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(5)
  const pgcCatalog = page.getByRole('checkbox', { name: 'PGC galaxies · 1' })
  await expect(pgcCatalog).toBeChecked()
  await pgcCatalog.uncheck()
  await expect(page.getByRole('button', { name: /Filter catalogs 5\/6/ })).toHaveAttribute('data-filtered', 'true')
  const skyOverlay = page.locator('.sky-overlay')
  await expect(skyOverlay.getByText('PGC 12345', { exact: false })).toHaveCount(0)
  await expect(skyOverlay.getByText('M 31 · Andromeda Galaxy', { exact: false })).toBeVisible()
  await expect(skyOverlay.getByText('Sh2-101 · Tulip Nebula', { exact: false })).toBeVisible()
  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(4)

  await page.getByRole('button', { name: /Deep sky/ }).click()
  await expect(page.locator('.catalog-objects ellipse')).toHaveCount(0)
  await expect(page.locator('.seiza-overlay__marker--outline')).toHaveCount(0)

  await expect(page.locator('.overlay-actions-below').getByRole('button')).toHaveCount(1)
  await expect(page.getByRole('button', { name: 'Download rendered PNG' })).toBeVisible()
  const imageStage = page.getByRole('button', { name: 'Expand image' })
  await imageStage.click()
  await expect(page.locator('.image-stage')).toHaveClass(/expanded/)
  const expandedImage = await page.locator('.sky-frame img').boundingBox()
  const expandedOverlay = await page.locator('.sky-overlay').boundingBox()
  expect(Math.abs(expandedImage!.width - expandedOverlay!.width)).toBeLessThan(1)
  expect(Math.abs(expandedImage!.height - expandedOverlay!.height)).toBeLessThan(1)
  await page.getByRole('button', { name: 'Close' }).click()
  await imageStage.press('Enter')
  await expect(page.locator('.image-stage')).toHaveClass(/expanded/)
  await page.getByRole('button', { name: 'Close' }).click()
})

test('draws and explains predicted satellite tracks when exposure metadata is complete', async ({ page }) => {
  await mockSolution(page)
  await page.route(`**/api/v1/solves/${publicId}/annotations**`, async (route) => {
    expect(new URL(route.request().url()).searchParams.get('satellite_tracks')).toBe('true')
    await route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        job_id: publicId,
        catalog_version: 'objects:test;satellites:test',
        capture_time: '2026-07-13T04:05:06Z',
        available: { deep_sky: true, named_stars: true, star_identifiers: true, field_stars: true, transients: true, historical_transients: true, minor_bodies: true, satellite_tracks: true, grid: true },
        counts: { deep_sky: 6, named_stars: 1, star_identifiers: 1, field_stars: 1, transients: 1, historical_transients: 1, minor_bodies: 2, satellite_tracks: 1 },
        objects: baseObjects,
        satellite_tracks: [{
          stable_id: 'satellite:norad:25544', label: 'ISS (ZARYA) [25544]', name: 'ISS (ZARYA)',
          norad_id: 25544, cospar_id: '1998-067A', source: 'https://celestrak.org/test',
          element_epoch_utc: '2026-07-13T03:00:00Z', element_age_seconds: 3906,
          sample_interval_seconds: 1, maximum_apparent_rate_arcsec_per_second: 185.4,
          segments: [{ start: [110, 780], end: [900, 260] }],
          risk: { level: 'possible', score: 0.48, maximum_sunlight_fraction: 0.82, minimum_range_km: 612, maximum_elevation_deg: 48.3, clipped_length_px: 945 },
        }],
        satellite_search: { catalog_source: 'https://celestrak.org/test', catalog_retrieved_at: '2026-07-13T03:30:00Z', elements_considered: 12000, propagation_failures: 2, stale_elements: 0 },
      }),
    })
  })
  await page.goto(`/solutions/${publicId}?satellite_tracks=true`)

  const track = page.locator('[data-kind="satellite"]')
  await expect(page.getByRole('button', { name: 'Satellite tracks · 1' })).toBeEnabled()
  await expect(track).toBeVisible()
  await expect(track.locator('.seiza-overlay__marker--outline')).toHaveCSS('stroke', 'rgb(255, 209, 102)')
  await expect(page.getByText('Predicted satellite crossings · 1')).toBeVisible()
  await page.getByText('Predicted satellite crossings · 1').click()
  await expect(page.locator('.satellite-track-list strong', { hasText: 'ISS (ZARYA) [25544]' })).toBeVisible()
  await expect(page.getByText('possible trail risk')).toBeVisible()
  await expect(page.getByText(/Checked 12,000 orbital records/)).toBeVisible()

  await page.getByRole('button', { name: 'Satellite tracks · 1' }).click()
  await expect(track).toHaveCount(0)
})

test('explains and disables catalog layers that are unavailable', async ({ page }) => {
  await mockSolution(page)
  await page.route(`**/api/v1/solves/${publicId}/annotations**`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      job_id: publicId,
      catalog_version: 'stars:test',
      capture_time: null,
      available: { deep_sky: false, named_stars: false, star_identifiers: false, field_stars: true, transients: false, historical_transients: false, minor_bodies: false, grid: true },
      counts: { deep_sky: 0, named_stars: 0, star_identifiers: 0, field_stars: 1, transients: 0, historical_transients: 0, minor_bodies: 0 },
      objects: baseObjects.filter((object) => object.kind === 'field-star'),
    }),
  }))
  await page.goto(`/solutions/${publicId}`)

  await expect(page.getByText(/Catalog data unavailable for this solution/)).toContainText('Deep sky, Named stars, Star identifiers, Transients, Solar system')
  await expect(page.getByRole('button', { name: 'Deep sky · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Named stars · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Star identifiers · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Field stars · 1' })).toBeEnabled()
})

test('distinguishes a missing acquisition time from a missing solar-system catalog', async ({ page }) => {
  await mockSolution(page)
  await page.route(`**/api/v1/solves/${publicId}/annotations**`, async (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      job_id: publicId,
      catalog_version: 'objects:test;stars:test;transients:test;minor-bodies:test',
      capture_time: null,
      available: { deep_sky: true, named_stars: true, star_identifiers: true, field_stars: true, transients: true, historical_transients: true, minor_bodies: false, satellite_tracks: false, grid: true },
      unavailable_reasons: { satellite_tracks: 'Satellite tracks require the shutter-open date and time for this image.' },
      counts: { deep_sky: 6, named_stars: 1, star_identifiers: 1, field_stars: 1, transients: 0, historical_transients: 0, minor_bodies: 0, satellite_tracks: 0 },
      objects: baseObjects.filter((object) => object.kind !== 'comet' && object.kind !== 'asteroid'),
    }),
  }))
  await page.goto(`/solutions/${publicId}?satellite_tracks=true`)

  await expect(page.getByText('Solar system positions require an acquisition time for this image. The minor-body catalog is installed.')).toBeVisible()
  await expect(page.getByText(/Catalog data unavailable for this solution/)).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Solar system · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Solar system · 0' })).toHaveAttribute(
    'title',
    'Solar system positions require an acquisition time for this image',
  )
  await expect(page.getByText('Satellite tracks require the shutter-open date and time for this image.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Satellite tracks · 0' })).toBeDisabled()
  await expect(page.getByRole('button', { name: 'Satellite tracks · 0' })).toHaveAttribute(
    'title',
    'Satellite tracks require the shutter-open date and time for this image.',
  )
})

test('downloads a branded rendered PNG with the current overlay', async ({ page }, testInfo) => {
  await mockSolution(page)
  await page.goto(`/solutions/${publicId}`)
  await page.getByRole('button', { name: /Filter catalogs 6\/6/ }).click()
  await page.getByRole('checkbox', { name: 'PGC galaxies · 1' }).uncheck()
  await page.getByRole('checkbox', { name: 'Detailed OpenNGC outlines' }).uncheck()
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

  const renderedOverlay = await page.evaluate(async () => {
    const state = window as typeof window & { __seizaSerializedOverlay?: Promise<string> }
    if (!state.__seizaSerializedOverlay) return {
      labels: [],
      markerStroke: '',
      gridStroke: '',
      labelWeight: '',
      gridWeight: '',
      haloWidth: '',
      objectLabels: [],
      catalogColors: {},
      outlineCount: 0,
      sharplessEllipseCount: 0,
    }
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
    const markerStroke = svg.style.getPropertyValue('--seiza-overlay-marker-stroke-width')
    const gridStroke = svg.style.getPropertyValue('--seiza-overlay-grid-stroke-width')
    const labelWeight = svg.style.getPropertyValue('--seiza-overlay-label-font-weight')
    const gridWeight = svg.style.getPropertyValue('--seiza-overlay-grid-font-weight')
    const haloWidth = svg.style.getPropertyValue('--seiza-overlay-label-halo-width')
    const objectLabels = [...svg.querySelectorAll<SVGTextElement>('.catalog-objects text')]
      .map((label) => label.textContent ?? '')
    const catalogColors = Object.fromEntries(
      [...svg.querySelectorAll<SVGGElement>('.catalog-objects > g[data-layer^="deep-sky:"]')]
        .map((group) => [
          group.dataset.layer ?? '',
          group.querySelector<SVGElement>('.seiza-overlay__marker')?.getAttribute('stroke') ?? '',
        ]),
    )
    const catalogColorOverrides = [...svg.querySelectorAll<SVGGElement>('.catalog-objects > g[data-layer^="deep-sky:"]')]
      .map((group) => group.style.getPropertyValue('--seiza-overlay-deep-sky-color'))
    const outlineCount = svg.querySelectorAll('.seiza-overlay__marker--outline').length
    const sharplessEllipseCount = svg.querySelectorAll('[data-layer="deep-sky:sharpless-vdb"] ellipse').length
    host.remove()
    return { labels, markerStroke, gridStroke, labelWeight, gridWeight, haloWidth, objectLabels, catalogColors, catalogColorOverrides, outlineCount, sharplessEllipseCount }
  })
  expect(renderedOverlay.markerStroke).toBe('0.7')
  expect(renderedOverlay.gridStroke).toBe('0.65')
  expect(renderedOverlay.labelWeight).toBe('400')
  expect(renderedOverlay.gridWeight).toBe('500')
  expect(renderedOverlay.haloWidth).toBe('0.1em')
  expect(renderedOverlay.objectLabels).toContain('M 31 · Andromeda Galaxy')
  expect(renderedOverlay.objectLabels).not.toContain('PGC 12345')
  expect(renderedOverlay.catalogColors).toMatchObject({
    'deep-sky:messier': '#f2ca72',
    'deep-sky:ngc': '#55cfff',
    'deep-sky:ic': '#72dfb9',
    'deep-sky:sharpless-vdb': '#ee9a78',
    'deep-sky:dark-nebulae': '#b4a3f0',
  })
  expect(renderedOverlay.catalogColorOverrides.every((color) => color === '')).toBe(true)
  expect(renderedOverlay.outlineCount).toBe(0)
  expect(renderedOverlay.sharplessEllipseCount).toBe(1)
  expect(renderedOverlay.labels.length).toBeGreaterThan(0)
  for (const label of renderedOverlay.labels) {
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
