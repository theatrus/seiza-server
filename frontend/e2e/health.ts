import type { Page } from '@playwright/test'

export async function mockHealth(page: Page) {
  await page.route('**/api/v1/health', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      status: 'ready',
      versions: { seiza_server: '0.2.0', seiza: '0.8.1' },
      solver_ready: true,
      queue_depth: 0,
      auth_mode: 'public',
      public_solve_access: { ui: true, api: true },
      job_backend: 'sqlx',
      queue_transport: 'local',
      embedded_workers: 1,
    }),
  }))
}
