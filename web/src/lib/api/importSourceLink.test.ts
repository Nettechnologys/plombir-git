import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import ImportsPage from '../../routes/imports/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, imports, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';

// card_2d0bf653507d: the import history bound every stored `source_url` to an
// `href`. The backend refuses non-git schemes before writing a task now, but a
// row written before that rule is still listed.

const timestamp = '2026-10-14T12:00:00Z';

function importTask(id: number, sourceUrl: string) {
	return {
		id,
		platform: 'git',
		source_url: sourceUrl,
		target_owner: 'alice',
		target_name: `import-${id}`,
		status: 'failed',
		progress: 0,
		stage: null,
		error: null,
		created_at: timestamp,
		updated_at: timestamp,
	};
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('import history source column', () => {
	it('links an http(s) source and shows any other stored value as text', async () => {
		auth.me.mockResolvedValue({
			id: 7,
			username: 'alice',
			email: 'alice@example.com',
			is_admin: false,
			display_name: null,
		});
		await fetchUser();
		imports.list.mockResolvedValue([
			importTask(1, 'https://github.com/acme/widgets'),
			importTask(2, 'javascript:alert(document.cookie)'),
			importTask(3, 'java\nscript:alert(1)'),
			importTask(4, 'git://git.example.com/acme/widgets.git'),
		]);
		setTestPage('/imports', {});
		rendered = await renderComponent(ImportsPage);
		await settle();

		const sources = Array.from(rendered.container.querySelectorAll('tbody tr .source'));
		expect(sources).toHaveLength(4);
		const links = sources.map((source) => source.querySelector('a')?.getAttribute('href') ?? null);
		expect(links).toEqual(['https://github.com/acme/widgets', null, null, null]);
		expect(sources[1].textContent).toContain('javascript:alert(document.cookie)');
		expect(sources[3].textContent).toContain('git://git.example.com/acme/widgets.git');
	});
});
