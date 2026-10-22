import { afterEach, describe, expect, it } from 'vitest';

import ImportedPeople from './ImportedPeople.svelte';
import { renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | null = null;

afterEach(async () => {
	await rendered?.destroy();
	rendered = null;
});

// card_a923b9785660: an imported issue or pull request names who it was
// assigned to, and whose review it asked for, on the platform it came from.
describe('imported people', () => {
	it('names the source and every login it named', async () => {
		rendered = await renderComponent(ImportedPeople, {
			role: 'assignees',
			people: [
				{ login: 'alice', service: 'github' },
				{ login: 'bob', service: 'github' },
			],
		});
		const text = rendered.container.textContent ?? '';
		expect(text).toContain('GitHub');
		expect(text).toContain('assigned there:');
		expect(text).toContain('@alice, @bob');
		// Logins on another platform are not accounts here: no profile link.
		expect(rendered.container.querySelector('a')).toBeNull();
	});

	it('says a reviewer was asked for, and names a self-hosted source by its host', async () => {
		rendered = await renderComponent(ImportedPeople, {
			role: 'reviewers',
			people: [{ login: 'carol', service: 'gitea:git.example.com' }],
		});
		const text = rendered.container.textContent ?? '';
		expect(text).toContain('Gitea · git.example.com');
		expect(text).toContain('review requested there:');
		expect(text).toContain('@carol');
	});

	it('renders nothing when the source named nobody', async () => {
		for (const people of [undefined, null, []]) {
			rendered = await renderComponent(ImportedPeople, { role: 'assignees', people });
			expect(rendered.container.textContent?.trim()).toBe('');
			expect(rendered.container.querySelector('.imported-people')).toBeNull();
			await rendered.destroy();
			rendered = null;
		}
	});
});
