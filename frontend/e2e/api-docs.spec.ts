import { expect, test } from '@playwright/test'
import { mockHealth } from './health'

test.beforeEach(async ({ page }) => {
  await mockHealth(page)
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
  await expect(page.getByText('/api/v1/catalog/stars/search', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/jobs/{job_id}/calibration', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/internal/worker/claim', { exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'N.I.N.A., ASTAP, and persistent clients.' })).toBeVisible()
  await expect(page.getByText('No N.I.N.A. plugin is required.')).toBeVisible()
  await expect(page.getByText('seiza worker --server https://seiza.fyi', { exact: true })).toBeVisible()
  await expect(page.getByText('ASTAP path: C:\\path\\to\\seiza.exe', { exact: false })).toBeVisible()
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
  await expect(footer.getByRole('link', { name: 'Seiza GitHub' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza')
  await expect(footer.getByRole('link', { name: 'Server GitHub' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza-server')
  await expect(footer.getByLabel('Software versions')).toHaveText('Seiza Server v0.1.0 · Seiza v0.5.0')
})
