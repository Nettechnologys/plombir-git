import { afterEach, beforeEach, expect, it } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { element, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		default_branch: 'main',
		stars_count: 0,
		is_private: false,
		created_at: '2026-08-31T00:00:00Z',
		viewer_permission: 'write',
	});
	repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
	repos.log.mockResolvedValue({ commits: [{ sha: 'a'.repeat(40), message: 'init', author: 'alice', date: '2026-11-24T00:00:00Z' }] });
	// Git's own byte order, which the page used to show unchanged.
	repos.tree.mockResolvedValue({
		entries: [
			{ name: '.dockerignore', kind: 'blob', size: 10 },
			{ name: 'CHANGELOG.md', kind: 'blob', size: 10 },
			{ name: 'Dockerfile', kind: 'blob', size: 10 },
			{ name: 'docs', kind: 'tree' },
			{ name: 'src', kind: 'tree' },
		],
	});
	repos.blob.mockResolvedValue({ content: '' });
});

afterEach(() => {
	rendered?.destroy();
	rendered = undefined;
});

const names = () =>
	[...rendered!.container.querySelectorAll('.tree-panel .entry-name')].map((node) => node.textContent?.trim());

it('lists folders before files and shows no breadcrumb at the root', async () => {
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	expect(names()).toEqual(['docs', 'src', '.dockerignore', 'CHANGELOG.md', 'Dockerfile']);
	expect(rendered.container.querySelector('.breadcrumb')).toBeNull();
});

it('links every step of the path inside a directory', async () => {
	setTestPage('/alice/demo?path=src/lib/api', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	const crumbs = element(rendered.container, 'nav.breadcrumb');
	expect([...crumbs.querySelectorAll('a')].map((a) => [a.textContent, a.getAttribute('href')])).toEqual([
		['demo', '/alice/demo'],
		['src', '/alice/demo?path=src'],
		['lib', '/alice/demo?path=src%2Flib'],
	]);
	expect(element(crumbs, '[aria-current="page"]').textContent).toBe('api');
});

it('draws New file as an ordinary button and the Code button with a single caret', async () => {
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	const newFile = element(rendered.container, '.new-file-link');
	expect(newFile.classList.contains('btn-outline')).toBe(true);
	expect(newFile.classList.contains('btn-primary')).toBe(false);
	expect(newFile.textContent).not.toContain('➕');
	expect(newFile.querySelector('svg')).not.toBeNull();

	const code = element(rendered.container, '.btn-code');
	expect(code.textContent).not.toContain('⌄');
	expect(code.querySelectorAll('.caret')).toHaveLength(1);
});
