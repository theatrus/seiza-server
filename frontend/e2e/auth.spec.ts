import { expect, test } from '@playwright/test'

const accountId = '019f7d31-8f00-7000-8000-000000000001'
const passkeyId = '019f7d31-8f00-7000-8000-000000000002'

function base64Url(bytes: number[]) {
  return Buffer.from(bytes).toString('base64url')
}

test('registers and signs in with a discoverable virtual passkey', async ({ page, browserName }) => {
  test.skip(browserName !== 'chromium', 'Playwright virtual authenticators use Chromium CDP')
  const cdp = await page.context().newCDPSession(page)
  await cdp.send('WebAuthn.enable')
  await cdp.send('WebAuthn.addVirtualAuthenticator', {
    options: {
      protocol: 'ctap2',
      transport: 'internal',
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  })
  await page.context().addCookies([{
    name: 'seiza_csrf',
    value: 'test-csrf',
    domain: 'localhost',
    path: '/',
  }])

  let signedIn = true
  let hasPasskey = false
  let hasApiKey = false
  await page.route('**/api/v1/health', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      status: 'ready',
      versions: { seiza_server: '0.2.0', seiza: '0.8.1' },
      solver_ready: true,
      queue_depth: 0,
      auth_mode: 'accounts',
      public_solve_access: { ui: true, api: true },
      job_backend: 'sqlx',
      queue_transport: 'local',
      embedded_workers: 1,
    }),
  }))
  await page.route('**/api/v1/account', (route) => {
    if (!signedIn) return route.fulfill({ status: 401, contentType: 'application/json', body: '{}' })
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        account: {
          id: accountId,
          email: 'astronomer@example.com',
          email_verified_at: '2026-07-18T18:00:00Z',
          created_at: '2026-07-18T18:00:00Z',
        },
        csrf_token: 'test-csrf',
        passkey_setup_required: !hasPasskey,
        passkeys: hasPasskey ? [{
          id: passkeyId,
          label: 'Observatory laptop',
          created_at: '2026-07-18T18:01:00Z',
          last_used_at: null,
        }] : [],
        api_keys: hasApiKey ? [{
          id: '019f7d31-8f00-7000-8000-000000000005',
          name: 'Observatory',
          display_prefix: 'seiza_key_account_key…',
          scopes: ['solve:read', 'solve:submit'],
          queue_weight: 1,
          created_at: '2026-07-18T18:02:00Z',
          expires_at: null,
          last_used_at: null,
        }] : [],
        sessions: [],
      }),
    })
  })
  await page.route('**/api/v1/account/solves', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      solves: [{
        id: '019f7d31-8f00-7000-8000-000000000006',
        status: 'succeeded',
        original_filename: 'm51-luminance.fits',
        created_at: '2026-07-18T17:30:00Z',
        started_at: '2026-07-18T17:30:01Z',
        completed_at: '2026-07-18T17:30:03Z',
        solve_time_ms: 2000,
      }],
    }),
  }))
  await page.route('**/api/v1/account/passkeys/registration/start', (route) => {
    expect(route.request().headers()['x-csrf-token']).toBe('test-csrf')
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        challenge_id: '019f7d31-8f00-7000-8000-000000000003',
        options: {
          publicKey: {
            rp: { id: 'localhost', name: 'Seiza' },
            user: {
              id: base64Url(Array.from({ length: 16 }, (_, index) => index + 1)),
              name: 'astronomer@example.com',
              displayName: 'astronomer@example.com',
            },
            challenge: base64Url(Array.from({ length: 32 }, (_, index) => index + 10)),
            pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
            timeout: 60_000,
            authenticatorSelection: { userVerification: 'required' },
            attestation: 'none',
          },
        },
      }),
    })
  })
  await page.route('**/api/v1/account/passkeys/registration/complete', async (route) => {
    const payload = route.request().postDataJSON()
    expect(payload.label).toBe('Observatory laptop')
    expect(payload.credential.type).toBe('public-key')
    expect(payload.credential.rawId).toBeTruthy()
    expect(payload.credential.response.attestationObject).toBeTruthy()
    hasPasskey = true
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        passkey: {
          id: passkeyId,
          label: payload.label,
          created_at: '2026-07-18T18:01:00Z',
          last_used_at: null,
        },
      }),
    })
  })
  await page.route('**/api/v1/account/api-keys', async (route) => {
    if (route.request().method() !== 'POST') return route.continue()
    expect(route.request().headers()['x-csrf-token']).toBe('test-csrf')
    const payload = route.request().postDataJSON()
    expect(payload.name).toBe('Observatory')
    expect(payload.scopes).toEqual(['solve:read', 'solve:submit'])
    hasApiKey = true
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({
        api_key: { id: '019f7d31-8f00-7000-8000-000000000005', name: payload.name },
        token: 'seiza_key_account_key_secret-shown-once',
      }),
    })
  })

  await page.goto('http://localhost:4173/account')
  await expect(page.getByRole('heading', { name: 'Your recent fields' })).toBeVisible()
  await expect(page.getByText('m51-luminance.fits')).toBeVisible()
  await expect(page.getByRole('link', { name: 'View result' })).toHaveAttribute('href', '/solutions/019f7d31-8f00-7000-8000-000000000006')
  await page.getByLabel('Passkey name').fill('Observatory laptop')
  await page.getByRole('button', { name: 'Add a passkey' }).click()
  await expect(page.getByText('Observatory laptop')).toBeVisible()
  await page.getByText('Create an API key').click()
  await page.getByRole('button', { name: 'Create key' }).click()
  await expect(page.getByText('Copy this key now—it will not be shown again.')).toBeVisible()
  await expect(page.getByText('seiza_key_account_key_secret-shown-once')).toBeVisible()

  signedIn = false
  await page.goto('http://localhost:4173/signin')
  await page.route('**/api/v1/auth/passkeys/authentication/start', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      challenge_id: '019f7d31-8f00-7000-8000-000000000004',
      options: {
        publicKey: {
          challenge: base64Url(Array.from({ length: 32 }, (_, index) => index + 50)),
          timeout: 60_000,
          rpId: 'localhost',
          userVerification: 'required',
        },
        mediation: 'conditional',
      },
    }),
  }))
  await page.route('**/api/v1/auth/passkeys/authentication/complete', async (route) => {
    const payload = route.request().postDataJSON()
    expect(payload.credential.type).toBe('public-key')
    expect(payload.credential.response.authenticatorData).toBeTruthy()
    expect(payload.credential.response.signature).toBeTruthy()
    expect(payload.credential.response.userHandle).toBeTruthy()
    signedIn = true
    return route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ status: 'success' }),
    })
  })
  await page.getByRole('button', { name: 'Use a passkey' }).click()
  await expect(page.getByRole('heading', { name: 'astronomer@example.com' })).toBeVisible()
})

test('accounts mode keeps anonymous solves available', async ({ page }) => {
  await page.route('**/api/v1/health', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      status: 'ready',
      versions: { seiza_server: '0.2.0', seiza: '0.8.1' },
      solver_ready: true,
      queue_depth: 0,
      auth_mode: 'accounts',
      public_solve_access: { ui: true, api: false },
      job_backend: 'dynamodb',
      queue_transport: 'sqs',
      embedded_workers: 0,
    }),
  }))
  await page.route('**/api/v1/account', (route) => route.fulfill({
    status: 401,
    contentType: 'application/json',
    body: '{}',
  }))

  await page.goto('http://localhost:4173/solve')
  await expect(page.getByRole('heading', { name: 'Solve this image.' })).toBeVisible()
  await expect(page.getByText('Public solves remain available and use the normal queue.')).toBeVisible()
  await expect(page.getByLabel('FITS or image file')).toBeVisible()
})

test('public browser and API solve access are presented independently', async ({ page }) => {
  await page.route('**/api/v1/health', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      status: 'ready',
      versions: { seiza_server: '0.2.0', seiza: '0.8.1' },
      solver_ready: true,
      queue_depth: 0,
      auth_mode: 'accounts',
      public_solve_access: { ui: false, api: true },
      job_backend: 'dynamodb',
      queue_transport: 'sqs',
      embedded_workers: 0,
    }),
  }))
  await page.route('**/api/v1/account', (route) => route.fulfill({
    status: 401,
    contentType: 'application/json',
    body: '{}',
  }))

  await page.goto('http://localhost:4173/solve')
  await expect(page.getByRole('heading', { name: 'Public browser solves are disabled.' })).toBeVisible()
  await expect(page.getByText('This deployment still accepts public API submissions.')).toBeVisible()
  await expect(page.getByRole('main').getByRole('link', { name: 'Sign in' })).toBeVisible()
  await expect(page.getByRole('link', { name: 'Use the API' })).toBeVisible()
  await expect(page.getByLabel('FITS or image file')).toHaveCount(0)
})
