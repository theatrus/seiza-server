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
  await page.route('**/api/v1/health', (route) => route.fulfill({
    contentType: 'application/json',
    body: JSON.stringify({
      status: 'ready',
      versions: { seiza_server: '0.2.0', seiza: '0.8.0' },
      solver_ready: true,
      queue_depth: 0,
      auth_mode: 'accounts',
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
        sessions: [],
      }),
    })
  })
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

  await page.goto('http://localhost:4173/account')
  await page.getByLabel('Passkey name').fill('Observatory laptop')
  await page.getByRole('button', { name: 'Add a passkey' }).click()
  await expect(page.getByText('Observatory laptop')).toBeVisible()

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
