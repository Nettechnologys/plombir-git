import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import SettingsPage from '../../routes/[owner]/[repo]/settings/tags/+page.svelte';
import { buildTagProtectionPayload, type TagProtectionFormState } from './tagProtectionForm';
import { setTestPage } from '../test/app';
import { resetTestClient, tagProtections } from '../test/client';
import { button, click, element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings/tags', { owner: 'alice', repo: 'demo' });
	tagProtections.list.mockResolvedValue([
		{ id: 7, pattern: 'v*', allowed_users: [{ username: 'alice' }] },
	]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

function formState(overrides: Partial<TagProtectionFormState> = {}): TagProtectionFormState {
  return { pattern: 'v*', allowed_users: 'alice, bob', ...overrides };
}

/**
 * What the network actually carries — `JSON.stringify` is what the client does
 * with the body, and it drops any key whose value is `undefined`.
 */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildTagProtectionPayload', () => {
  it('sends the allow-list on create, so a protected pattern can have an exception', () => {
    const body = wire(buildTagProtectionPayload(formState(), true));

    expect(body).toEqual({ pattern: 'v*', allowed_users: ['alice', 'bob'] });
  });

  it('sends the allow-list on update, so an exception can be granted after the fact', () => {
    const body = wire(buildTagProtectionPayload(formState({ allowed_users: 'carol' }), false));

    expect(body).toHaveProperty('allowed_users');
    expect(body.allowed_users).toEqual(['carol']);
  });

  it('sends an emptied allow-list as [], so a granted exception can be revoked', () => {
    const body = wire(buildTagProtectionPayload(formState({ allowed_users: '  ,  ' }), false));

    expect(body).toHaveProperty('allowed_users');
    expect(body.allowed_users).toEqual([]);
  });

  it('leaves the pattern out of an update, which PATCH cannot change anyway', () => {
    const body = wire(buildTagProtectionPayload(formState(), false));

    expect(Object.keys(body)).toEqual(['allowed_users']);
  });

  it('trims the pattern the operator typed', () => {
    const body = wire(buildTagProtectionPayload(formState({ pattern: '  release/**  ' }), true));

    expect(body).toHaveProperty('pattern', 'release/**');
  });

	it('submits the rendered edit form with the operator-controlled allow-list', async () => {
		rendered = await renderComponent(SettingsPage);
		await click(button(rendered.container, 'Edit'));
		await input(element(rendered.container, '#tag-allowed-users'), ' bob, carol ');
		await submit(element(rendered.container, 'form'));

		expect(tagProtections.update).toHaveBeenCalledWith('alice', 'demo', 7, {
			allowed_users: ['bob', 'carol'],
		});
	});
});
