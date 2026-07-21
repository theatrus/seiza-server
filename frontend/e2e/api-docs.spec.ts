import { expect, test } from '@playwright/test'
import { mockHealth } from './health'

test.beforeEach(async ({ page }) => {
  await mockHealth(page)
})

test('advertises macOS, Python, N.I.N.A., and Siril integrations on the home page', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('heading', { name: 'A native Mac app, Python bindings, and tools you already use.' })).toBeVisible()
  await expect(page.getByText('signed, notarized app for Apple silicon and Intel Macs', { exact: false })).toBeVisible()
  await expect(page.getByText('pip install seiza', { exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Seiza for macOS' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza-mac')
  await expect(page.getByRole('link', { name: 'Python bindings' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza/tree/main/seiza-py')
  await expect(page.getByRole('link', { name: 'Explore every integration' })).toHaveAttribute('href', '/docs/api#integrations')
})

test('advertises optional satellite lookup with separate pixel evidence', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('heading', { name: 'Catalog the field—and predict satellite crossings.' })).toBeVisible()
  await expect(page.getByText('Satellite lookup is optional and off by default', { exact: false })).toBeVisible()
  await expect(page.getByText('checks predicted corridors for matching trail pixels', { exact: false })).toBeVisible()
  await expect(page.getByText('does not prove the candidate satellite identity', { exact: false })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Read the annotation contract' })).toHaveAttribute('href', '/docs/api#responses')
})

test('links the data-source acknowledgements from the home hero and about section', async ({ page }) => {
  await page.goto('/')

  await expect(page.getByRole('link', { name: 'See our data sources' })).toHaveAttribute('href', '/data-sources')
  await expect(page.getByRole('link', { name: 'Data sources & acknowledgements' })).toHaveAttribute('href', '/data-sources')
})

test('publishes only indexable public pages in the sitemap', async ({ request }) => {
  const response = await request.get('/sitemap.xml')
  expect(response.ok()).toBe(true)
  const sitemap = await response.text()

  expect(sitemap).toContain('<loc>https://seiza.fyi/</loc>')
  expect(sitemap).toContain('<loc>https://seiza.fyi/solve</loc>')
  expect(sitemap).toContain('<loc>https://seiza.fyi/docs/api</loc>')
  expect(sitemap).toContain('<loc>https://seiza.fyi/data-sources</loc>')
  expect(sitemap).not.toContain('/solutions/')
})

test('credits the upstream catalogues and links their primary sources', async ({ page }) => {
  await page.goto('/solve')
  await page.locator('footer').getByRole('link', { name: 'Data sources' }).click()

  await expect(page).toHaveURL('/data-sources')
  await expect(page.getByRole('heading', { name: 'Built on generations of sky surveys.' })).toBeVisible()
  await expect(page.getByText('Credit: ESA/Gaia/DPAC.')).toBeVisible()
  await expect(page.getByRole('link', { name: /Tycho-2 · CDS I\/259/ })).toHaveAttribute('href', 'https://cdsarc.cds.unistra.fr/viz-bin/cat/I/259')
  await expect(page.getByRole('link', { name: /OpenNGC project/ })).toHaveAttribute('href', 'https://github.com/mattiaverga/OpenNGC')
  await expect(page.getByRole('link', { name: /General Catalogue of Variable Stars/ })).toBeVisible()
  await expect(page.getByRole('link', { name: /Washington Double Star Catalog/ })).toBeVisible()
  await expect(page.getByRole('link', { name: /Galactic supernova remnants/ })).toBeVisible()
  await expect(page.getByRole('link', { name: /Latest Supernovae/ })).toHaveAttribute('href', 'https://www.rochesterastronomy.org/snimages/snactive.html')
  await expect(page.getByRole('link', { name: /Minor Planet Center/ }).first()).toHaveAttribute('href', 'https://www.minorplanetcenter.net/')
  await expect(page.getByRole('link', { name: /Small-body orbits/ })).toHaveAttribute('href', 'https://ssd.jpl.nasa.gov/sb/orbits.html')
  await expect(page.getByRole('link', { name: 'IAU SatChecker' })).toHaveAttribute('href', 'https://satchecker.cps.iau.org/')
  await expect(page.getByRole('link', { name: 'Seiza rolling mirror' })).toHaveAttribute('href', 'https://downloads.seiza.fyi/satellites/v1/manifest.json')
  await expect(page.getByText(/Seiza’s Apache-2.0 license covers Seiza software, not third-party catalogue data/)).toBeVisible()
})

test('keeps the data-source credits readable on a narrow screen', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/data-sources')

  await expect(page.getByRole('heading', { name: 'Transient and Solar System data.' })).toBeVisible()
  const dimensions = await page.evaluate(() => ({
    viewport: document.documentElement.clientWidth,
    page: document.documentElement.scrollWidth,
  }))
  expect(dimensions.page).toBeLessThanOrEqual(dimensions.viewport)
})

test('documents the public, catalog, compatibility, and worker APIs', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('link', { name: 'API', exact: true }).click()

  await expect(page).toHaveURL('/docs/api')
  await expect(page.getByRole('heading', { name: 'Plate solving for software, scripts, and observatories.' })).toBeVisible()
  await expect(page.getByText('/api/v1/solves/{public_id}', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/solves/{public_id}/validation-donation', { exact: true })).toBeVisible()
  await expect(page.getByText('"solve_is_invalid":true', { exact: false })).toBeVisible()
  await expect(page.getByText('/api/v1/catalog/objects/search', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/catalog/objects/details/{canonical_id}', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/catalog/stars/search', { exact: true })).toBeVisible()
  await expect(page.getByText('Predicted satellite tracks')).toBeVisible()
  await expect(page.getByText('/api/v1/auth/logout-all', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/account/api-keys', { exact: true }).first()).toBeVisible()
  await expect(page.getByText('/api/jobs/{job_id}/calibration', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/internal/worker/claim', { exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'macOS, Python, N.I.N.A., Siril, and persistent clients.' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Seiza for macOS: native browsing and plate solving' })).toBeVisible()
  await expect(page.getByText('Apple silicon and Intel on macOS 15 or later', { exact: false })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Seiza for macOS releases' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza-mac/releases/latest')
  await expect(page.getByRole('heading', { name: 'Python: install the Seiza engine with pip' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'pip install seiza' })).toHaveAttribute('href', 'https://pypi.org/project/seiza/')
  await expect(page.getByText('Binary ABI3 wheels support every CPython from 3.9 onward', { exact: false })).toBeVisible()
  await expect(page.getByRole('link', { name: 'seiza-py guide and source' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza/tree/main/seiza-py')
  await expect(page.getByText('seiza-cli-…-windows-x86_64.msi', { exact: true })).toBeVisible()
  await expect(page.getByText('Start → Seiza → Seiza Catalog Setup', { exact: false })).toBeVisible()
  await expect(page.getByText('seiza setup', { exact: true }).first()).toBeVisible()
  await expect(page.getByText('C:\\Seiza\\seiza.exe download-data prebuilt --output C:\\seiza-data', { exact: false })).toBeVisible()
  await expect(page.getByText('setx SEIZA_CATALOG_DIR C:\\seiza-data', { exact: false })).toBeVisible()
  await expect(page.getByText('ASTAP-compatible and solve-field-compatible modes select the appropriate available star catalog and blind index automatically.', { exact: false })).toBeVisible()
  await expect(page.getByText('seiza install-solve-field --dir <directory>', { exact: true })).toBeVisible()
  await expect(page.getByText('$sirilDir = "$env:LOCALAPPDATA\\Seiza\\siril-asnet"', { exact: false })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Siril’s local Astrometry.net documentation' })).toHaveAttribute('href', 'https://siril.readthedocs.io/en/stable/astrometry/platesolving.html')
  await expect(page.getByText('seiza worker --server https://seiza.fyi', { exact: true })).toBeVisible()
  await expect(page.getByText('ASTAP path: C:\\Program Files\\Seiza\\seiza.exe', { exact: false })).toBeVisible()
  await expect(page.getByText('Your images remain yours.')).toBeVisible()
  await expect(page.getByText('seiza-validation-image-grant-v2')).toBeVisible()
  await expect(page.getByText(/only to test, validate, debug, and improve the Seiza plate solver/)).toBeVisible()
  const copyButton = page.locator('[data-copy-example]').first()
  await expect(copyButton).toBeVisible()
  await copyButton.click()
  await expect(copyButton).not.toHaveText('Copy')
  expect(['Copied', 'Selected']).toContain(await copyButton.textContent())
})

test('keeps the API reference readable on a narrow screen', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto('/docs/api')

  await expect(page.getByRole('heading', { name: 'Multipart upload, then poll.' })).toBeVisible()
  const dimensions = await page.evaluate(() => ({
    viewport: document.documentElement.clientWidth,
    page: document.documentElement.scrollWidth,
  }))
  expect(dimensions.page).toBeLessThanOrEqual(dimensions.viewport)
})

test('links the author and both source repositories from the footer', async ({ page }) => {
  await page.goto('/solve')
  await expect(page.getByText('Your image remains yours.')).toBeVisible()
  const footer = page.locator('footer')

  await expect(footer.getByRole('link', { name: 'Built by Yann Ramin' })).toHaveAttribute('href', 'https://theatr.us')
  await expect(footer.getByRole('link', { name: 'Data sources' })).toHaveAttribute('href', '/data-sources')
  await expect(footer.getByRole('link', { name: 'Seiza GitHub' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza')
  await expect(footer.getByRole('link', { name: 'Server GitHub' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza-server')
  await expect(footer.getByLabel('Software versions')).toHaveText('Seiza Server v0.3.0 · Seiza v0.8.1')
})
