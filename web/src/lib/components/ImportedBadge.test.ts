import { afterEach, describe, expect, it } from 'vitest';

import ImportedBadge from './ImportedBadge.svelte';
import { renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | null = null;

afterEach(async () => {
	await rendered?.destroy();
	rendered = null;
});

// card_04088376105b: an imported issue, comment or pull request belongs to the
// account that ran the import, and names who wrote it on the source beside it.
describe('imported badge', () => {
	it('names the platform and the source author', async () => {
		rendered = await renderComponent(ImportedBadge, { login: 'alice', service: 'github' });
		const text = rendered.container.textContent ?? '';
		expect(text).toContain('GitHub');
		expect(text).toContain('@alice');
		// A login on another platform is not an account here: no profile link.
		expect(rendered.container.querySelector('a')).toBeNull();
	});

	it('names the server an import came from when it is not the public one', async () => {
		rendered = await renderComponent(ImportedBadge, {
			login: 'alice',
			service: 'gitlab:git.example.com:8443',
		});
		expect(rendered.container.textContent).toContain('GitLab · git.example.com:8443');
	});

	it('renders nothing for what was written on this instance', async () => {
		rendered = await renderComponent(ImportedBadge, { login: null, service: null });
		expect(rendered.container.textContent?.trim()).toBe('');
	});
});
