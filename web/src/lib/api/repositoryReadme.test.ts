import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { element, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const README = [
	'# slugkit',
	'',
	'```python',
	'>>> slugify("Hello, World!")',
	"'hello-world'",
	'```',
	'',
	'- **No dependencies.** Pure `stdlib`.',
	'',
	'<img src="x" onerror="alert(1)">',
].join('\n');

function showReadme(name: string, content: string) {
	repos.tree.mockResolvedValue({ entries: [{ name, kind: 'blob', size: content.length }] });
	repos.blob.mockResolvedValue({ content });
}

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		default_branch: 'main',
		stars_count: 0,
		is_private: false,
		created_at: '2026-08-31T00:00:00Z',
		viewer_permission: 'read',
	});
	repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
	repos.log.mockResolvedValue({ commits: [] });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('repository README', () => {
	it('renders a Markdown README as Markdown, fenced code included', async () => {
		showReadme('README.md', README);
		rendered = await renderComponent(RepositoryPage);
		const body = element(rendered.container, '.readme-body .markdown-body');

		expect(body.querySelector('h1')?.textContent).toBe('slugkit');
		expect(body.querySelector('pre code')?.textContent).toContain('>>> slugify("Hello, World!")');
		expect(body.querySelector('li strong')?.textContent).toBe('No dependencies.');
		expect(body.querySelector('li code')?.textContent).toBe('stdlib');
		expect(body.textContent).not.toContain('```');
		expect(body.textContent).not.toContain('**');
	});

	it('sanitizes what the README renders', async () => {
		showReadme('README.md', README);
		rendered = await renderComponent(RepositoryPage);
		const body = element(rendered.container, '.readme-body .markdown-body');

		expect(body.querySelector('[onerror]')).toBeNull();
	});

	it('shows a README without an extension as plain text', async () => {
		showReadme('README', '**not markdown**\n<b>kept as text</b>');
		rendered = await renderComponent(RepositoryPage);
		const plain = element(rendered.container, '.readme-body .readme-plain');

		expect(plain.textContent).toBe('**not markdown**\n<b>kept as text</b>');
		expect(plain.querySelector('b')).toBeNull();
	});
});
