import { expect, test } from '@playwright/test'

test('documents the public, catalog, compatibility, and worker APIs', async ({ page }) => {
  await page.goto('/')
  await page.getByRole('link', { name: 'API', exact: true }).click()

  await expect(page).toHaveURL('/docs/api')
  await expect(page.getByRole('heading', { name: 'Plate solving for software, scripts, and observatories.' })).toBeVisible()
  await expect(page.getByText('/api/v1/solves/{public_id}', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/catalog/objects/search', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/catalog/stars/search', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/jobs/{job_id}/calibration', { exact: true })).toBeVisible()
  await expect(page.getByText('/api/v1/internal/worker/claim', { exact: true })).toBeVisible()
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
  const footer = page.locator('footer')

  await expect(footer.getByRole('link', { name: 'Built by Yann Ramin' })).toHaveAttribute('href', 'https://theatr.us')
  await expect(footer.getByRole('link', { name: 'Seiza GitHub' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza')
  await expect(footer.getByRole('link', { name: 'Server GitHub' })).toHaveAttribute('href', 'https://github.com/theatrus/seiza-server')
})
