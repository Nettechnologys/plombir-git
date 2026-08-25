import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import SettingsPage from '../../routes/[owner]/[repo]/settings/labels/+page.svelte';
import { buildLabelPayload, type LabelFormState } from './labelForm';
import { setTestPage } from '../test/app';
import { labels, resetTestClient } from '../test/client';
import { click, input, renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings/labels', { owner: 'alice', repo: 'demo' });
	labels.list.mockResolvedValue([
		{ id: 7, name: 'bug', color: '#ff0000', description: 'Something is broken' },
	]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

function formState(overrides: Partial<LabelFormState> = {}): LabelFormState {
  return { name: 'bug', color: '#ff0000', description: 'Something is broken', ...overrides };
}

/** What the network actually carries — `JSON.stringify` drops an `undefined` value. */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildLabelPayload', () => {
  it('sends an emptied description as null, so it can actually be cleared', () => {
    const body = wire(buildLabelPayload(formState({ description: '   ' })));

    // `null` is the API's "clear it"; a dropped key is its "leave it alone",
    // and a dropped key is what an emptied field used to become.
    expect(body).toHaveProperty('description');
    expect(body.description).toBeNull();
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const body = wire(buildLabelPayload(formState({ name: '', description: '' })));

    expect(Object.keys(body).sort()).toEqual(['color', 'description', 'name']);
  });

  it('still carries what the operator typed', () => {
    const body = wire(buildLabelPayload(formState({ name: '  wontfix  ' })));

    expect(body).toEqual({
      name: 'wontfix',
      color: '#ff0000',
      description: 'Something is broken'
    });
  });

	it('submits the rendered edit form through the canonical payload builder', async () => {
		rendered = await renderComponent(SettingsPage, { data: {} });
		await click(rendered.container.querySelector('[title="Edit"]')!);

		await input(rendered.container.querySelector('#label-desc')!, '   ');
		await click(rendered.container.querySelector('.form-actions .btn-primary')!);
		await settle();

		expect(labels.update).toHaveBeenCalledWith('alice', 'demo', 7, {
			name: 'bug',
			color: '#ff0000',
			description: null,
		});
	});

	it('submits the rendered create form through the labels client', async () => {
		rendered = await renderComponent(SettingsPage, { data: {} });
		await click(rendered.container.querySelector('.page-header .btn-primary')!);
		await input(rendered.container.querySelector('#label-name')!, ' feature ');
		await input(rendered.container.querySelector('#label-desc')!, ' New work ');
		await click(rendered.container.querySelector('.form-actions .btn-primary')!);
		await settle();

		expect(labels.create).toHaveBeenCalledWith('alice', 'demo', {
			name: 'feature',
			color: '#ff0000',
			description: 'New work',
		});
	});
});
