import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	request: vi.fn(),
}));
vi.mock('./_base.svelte', () => base);

import DeployKeysPage from '../../routes/[owner]/[repo]/settings/deploy-keys/+page.svelte';
import { setTestPage } from '../test/app';
import { deployKeys as routeDeployKeys, resetTestClient } from '../test/client';
import { element, input, renderComponent, submit, type RenderedComponent } from '../test/render';
import { deployKeys } from './deployKeys';
import { repoPath } from './repoPath';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	base.request.mockResolvedValue([]);
	routeDeployKeys.list.mockImplementation(deployKeys.list);
	routeDeployKeys.create.mockImplementation(deployKeys.create);
	// SvelteKit decodes each route parameter before the page reads it.
	setTestPage('/alice/app%2Fkeys%2F5%3F/settings/deploy-keys', { owner: 'alice', repo: 'app/keys/5?' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
});

describe('repository API path segments', () => {
	it('keeps decoded route parameters in the target repository on page load and mutation', async () => {
		rendered = await renderComponent(DeployKeysPage);
		const path = '/repos/alice/app%2Fkeys%2F5%3F/keys';
		expect(base.request).toHaveBeenCalledWith(path);

		await input(element(rendered.container, '#deploy-key-title'), 'automation');
		await input(element(rendered.container, '#deploy-public-key'), 'ssh-ed25519 AAAA');
		await submit(element<HTMLFormElement>(rendered.container, 'form'));

		expect(base.request).toHaveBeenCalledWith(path, expect.objectContaining({ method: 'POST' }));
		expect(base.request.mock.calls.some(([url]) => url === '/repos/alice/app/keys/5')).toBe(false);
	});

	it('encodes each segment and refuses dot segments before fetch', () => {
		expect(repoPath('a/b', 'r?x#%')).toBe('/repos/a%2Fb/r%3Fx%23%25');
		expect(() => repoPath('alice', '..')).toThrow('Invalid URL path segment');
		expect(() => repoPath('.', 'repo')).toThrow('Invalid URL path segment');
	});
});
