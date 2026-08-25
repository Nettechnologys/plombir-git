import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import SettingsPage from '../../routes/[owner]/[repo]/settings/mirror/+page.svelte';
import { buildMirrorPayload, type MirrorFormState } from './mirrorForm';
import { setTestPage } from '../test/app';
import { mirrors, resetTestClient } from '../test/client';
import { check, element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const configuredMirror = {
	id: 7,
	url: 'https://example.com/upstream.git',
	username: 'sync-bot',
	has_credentials: true,
	sync_interval_seconds: 86_400,
	status: 'idle',
	last_sync_at: null,
	next_sync_at: null,
	last_sync_error: null,
};

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings/mirror', { owner: 'alice', repo: 'demo' });
	mirrors.get.mockResolvedValue(configuredMirror);
	mirrors.update.mockResolvedValue({ ...configuredMirror, username: '' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

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

	it('lets the rendered form explicitly clear a stored credential', async () => {
		rendered = await renderComponent(SettingsPage);
		await input(element(rendered.container, '#mirror-username'), '   ');
		await check(element(rendered.container, '#mirror-clear-password'), true);
		await submit(element(rendered.container, '.mirror-form'));

		expect(mirrors.update).toHaveBeenCalledWith('alice', 'demo', {
			url: 'https://example.com/upstream.git',
			username: '',
			password: '',
			sync_interval_seconds: 86_400,
		});
	});
});
