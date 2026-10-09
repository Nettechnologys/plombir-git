import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import EditPage from '../../routes/[owner]/[repo]/releases/edit/[id]/+page.svelte';
import { buildReleaseUpdatePayload, type ReleaseUpdateFormState } from './releaseForm';
import { navigation, setTestPage } from '../test/app';
import { releases, resetTestClient, repos as viewerRepos } from '../test/client';
import { element, input, renderComponent, submit, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	// The write controls follow `viewer_permission` (card_3625a7b89abb).
	viewerRepos.get.mockResolvedValue({ viewer_permission: 'write' });
	setTestPage('/alice/demo/releases/edit/7', { owner: 'alice', repo: 'demo', id: '7' });
	releases.get.mockResolvedValue({
		tag_name: 'v2.0.0',
		title: 'Version 2',
		body: 'Existing notes',
		is_draft: false,
		is_prerelease: true,
	});
	releases.update.mockResolvedValue({});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

function formState(overrides: Partial<ReleaseUpdateFormState> = {}): ReleaseUpdateFormState {
  return {
    title: 'v2.0.0',
    body: 'Fixed the thing.',
    is_draft: false,
    is_prerelease: false,
    ...overrides
  };
}

/** What the network actually carries — `JSON.stringify` drops an `undefined` value. */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildReleaseUpdatePayload', () => {
  it('sends emptied release notes, so published text can be taken back down', () => {
    const body = wire(buildReleaseUpdatePayload(formState({ body: '   ' })));

    // A dropped key reads as "keep the notes", which is why deleting the text
    // and saving used to answer 200 and change nothing.
    expect(body).toHaveProperty('body', '');
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const body = wire(buildReleaseUpdatePayload(formState({ title: '', body: '' })));

    expect(Object.keys(body).sort()).toEqual(['body', 'is_draft', 'is_prerelease', 'title']);
  });

  it('still carries what the author typed, and the two flags', () => {
    const body = wire(
      buildReleaseUpdatePayload(formState({ title: '  v2.0.1  ', is_prerelease: true }))
    );

    expect(body).toEqual({
      title: 'v2.0.1',
      body: 'Fixed the thing.',
      is_draft: false,
      is_prerelease: true
    });
  });

	it('submits the rendered edit form through the canonical builder', async () => {
		rendered = await renderComponent(EditPage);
		await input(element(rendered.container, '#body'), '   ');
		await submit(element(rendered.container, '.release-form'));

		expect(releases.update).toHaveBeenCalledWith('alice', 'demo', 7, {
			title: 'Version 2',
			body: '',
			is_draft: false,
			is_prerelease: true,
		});
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo/releases');
	});
});
