import { describe, expect, it } from 'vitest';

// The page's own source, pulled in by vite so the check below needs no node
// filesystem API (and no `@types/node` for `npm run check`).
import settingsPageSource from '../../routes/[owner]/[repo]/settings/mirror/+page.svelte?raw';
import { buildMirrorPayload, type MirrorFormState } from './mirrorForm';

function formState(overrides: Partial<MirrorFormState> = {}): MirrorFormState {
  return {
    url: 'https://example.com/upstream.git',
    username: 'sync-bot',
    password: '',
    clearPassword: false,
    intervalHours: 24,
    ...overrides
  };
}

/**
 * What the network actually carries. The bug this file guards against is
 * invisible on the object itself — `{password: undefined}` still has the key —
 * and only shows up once `JSON.stringify` has dropped it, which is what
 * `mirrors.create` / `.update` do with the body.
 */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildMirrorPayload', () => {
  it('sends an empty password when the operator asks to remove the stored one', () => {
    const body = wire(buildMirrorPayload(formState({ clearPassword: true }), true));

    // The API's own spelling of "clear it": an absent key would mean "keep it",
    // and there is no other way to take an access token off a live mirror.
    expect(body).toHaveProperty('password', '');
  });

  it('leaves the password out when a stored credential is simply untouched', () => {
    const body = wire(buildMirrorPayload(formState(), true));

    // The one state the form cannot display, so the one it must not overwrite:
    // saving a new sync interval has to leave the credential where it is.
    expect(body).not.toHaveProperty('password');
    expect(body).toMatchObject({ sync_interval_seconds: 86400 });
  });

  it('sends an empty password when there is nothing stored to keep', () => {
    const body = wire(buildMirrorPayload(formState(), false));

    expect(body).toHaveProperty('password', '');
  });

  it('sends the password the operator typed, over a stored one', () => {
    const body = wire(buildMirrorPayload(formState({ password: '  hunter2  ' }), true));

    expect(body).toHaveProperty('password', 'hunter2');
  });

  it('prefers the removal over anything left in the box', () => {
    const body = wire(
      buildMirrorPayload(formState({ password: 'hunter2', clearPassword: true }), true)
    );

    expect(body).toHaveProperty('password', '');
  });

  it('sends an emptied username as "", so the remote can go back to anonymous', () => {
    const body = wire(buildMirrorPayload(formState({ username: '  ' }), true));

    expect(body).toHaveProperty('username', '');
  });

  it('never puts NaN on the wire for a blanked interval', () => {
    // `NaN` serialises as `null`, which the API reads as "no interval given" —
    // the same silent no-op as a dropped key.
    for (const intervalHours of ['', 'abc', Number.NaN]) {
      expect(wire(buildMirrorPayload(formState({ intervalHours }), true))).toMatchObject({
        sync_interval_seconds: 3600
      });
    }
  });

  it('keeps the one-hour floor the API enforces', () => {
    expect(wire(buildMirrorPayload(formState({ intervalHours: 0 }), true))).toMatchObject({
      sync_interval_seconds: 3600
    });
  });

  it('is the builder the settings form itself uses', () => {
    const page = settingsPageSource;

    expect(page).toContain('buildMirrorPayload');
    // A second, page-local body builder would make every assertion above a
    // statement about dead code.
    expect(page).not.toMatch(/password:\s*trimmedPassword/);
    // And the clear branch is unreachable without a control that sets it.
    expect(page).toContain('bind:checked={clearPassword}');
    expect(page).toContain('mirror?.has_credentials');
  });
});
