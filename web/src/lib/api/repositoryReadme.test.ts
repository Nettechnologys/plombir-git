import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import BlobPage from '../../routes/[owner]/[repo]/blob/[...path]/+page.svelte';
import { findReadme, readmeDirectories, repositoryLinkTarget } from '../repoReadme';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { element, renderComponent, settle, type RenderedComponent } from '../test/render';

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

type Entry = { name: string; kind: 'blob' | 'tree' };

/** A repository whose directories hold `files`: `{ '': [...], '.github': [...] }`,
 *  and whose blobs are `contents`, by path. */
function showTree(files: Record<string, Entry[]>, contents: Record<string, string>) {
	repos.tree.mockImplementation(async (_owner: string, _repo: string, _ref?: string, path?: string) => ({
		entries: files[path ?? ''] ?? [],
	}));
	repos.blob.mockImplementation(async (_owner: string, _repo: string, path: string) => ({
		content: contents[path],
	}));
	repos.rawUrl.mockImplementation((owner: string, repo: string, path: string) => `/api/raw/${owner}/${repo}/${path}`);
}

const blob = (name: string): Entry => ({ name, kind: 'blob' });
const tree = (name: string): Entry => ({ name, kind: 'tree' });

// card_68a6f79c5fe4: the README was looked for in the root only. GitHub and
// Gitea also show one kept in `.github/`, `.gitea/` or `docs/`, so a
// repository that moved here from either opened with no description.
describe('where the README is found', () => {
	it('shows a README kept only in .github/, with its links leading from there', async () => {
		showTree(
			{ '': [tree('.github'), tree('src'), blob('main.go')], '.github': [blob('README.md'), blob('logo.png')] },
			{
				'.github/README.md': [
					'# from .github',
					'',
					'[Guide](../docs/guide.md#install) · [Workflows](workflows/) · [Site](https://example.com/x) · [Top](#top)',
					'',
					'![logo](logo.png)',
				].join('\n'),
			},
		);
		rendered = await renderComponent(RepositoryPage);
		await settle();
		const body = element(rendered.container, '.readme-body .markdown-body');

		expect(repos.tree).toHaveBeenCalledWith('alice', 'demo', undefined, '.github');
		expect(repos.blob).toHaveBeenCalledWith('alice', 'demo', '.github/README.md', undefined);
		expect(body.querySelector('h1')?.textContent).toBe('from .github');
		expect(element(rendered.container, '.readme-header').textContent).toContain('.github/README.md');
		expect(Array.from(body.querySelectorAll('a'), (a) => a.getAttribute('href'))).toEqual([
			'/alice/demo/blob/docs/guide.md#install',
			'/alice/demo?path=.github%2Fworkflows',
			'https://example.com/x',
			'#top',
		]);
		expect(body.querySelector('img')?.getAttribute('src')).toBe('/api/raw/alice/demo/.github/logo.png');
	});

	it('looks in .gitea/ and .github/ before the root, and in docs/ after it, as Gitea does', async () => {
		showTree(
			{
				'': [tree('docs'), tree('.gitea'), blob('README.md')],
				'.gitea': [blob('readme.TXT')],
				docs: [blob('README.md')],
			},
			{ '.gitea/readme.TXT': 'from .gitea', 'README.md': '# root', 'docs/README.md': '# docs' },
		);
		rendered = await renderComponent(RepositoryPage);
		await settle();

		expect(element(rendered.container, '.readme-body .readme-plain').textContent).toBe('from .gitea');
		expect(repos.tree).not.toHaveBeenCalledWith('alice', 'demo', undefined, 'docs');
	});

	it('falls back to docs/ when no other place has a README', async () => {
		showTree(
			{ '': [tree('docs'), tree('.github'), blob('main.go')], '.github': [tree('workflows')], docs: [blob('Readme.markdown')] },
			{ 'docs/Readme.markdown': '# from docs' },
		);
		rendered = await renderComponent(RepositoryPage);
		await settle();

		expect(element(rendered.container, '.readme-body .markdown-body h1').textContent).toBe('from docs');
	});

	it('shows no README when none of the places has one', async () => {
		showTree({ '': [tree('docs'), blob('main.go')], docs: [blob('guide.md')] }, {});
		rendered = await renderComponent(RepositoryPage);
		await settle();

		expect(rendered.container.querySelector('.readme-section')).toBeNull();
		expect(repos.blob).not.toHaveBeenCalled();
	});
});

describe('README lookup rules', () => {
	it('prefers Markdown, then .txt, then no extension, in any case', () => {
		expect(findReadme([blob('README'), blob('readme.txt'), blob('ReadMe.MD')])).toBe('ReadMe.MD');
		expect(findReadme([blob('README'), blob('README.TXT')])).toBe('README.TXT');
		expect(findReadme([blob('readme.md'), blob('README.md')])).toBe('README.md');
		expect(findReadme([tree('README.md'), blob('READMEs.md'), blob('README.rst')])).toBeNull();
	});

	it("names only the directories the root has, in Gitea's order", () => {
		expect(readmeDirectories([tree('docs'), tree('.GitHub'), tree('.gitea'), blob('.github')])).toEqual([
			'.gitea',
			'.GitHub',
			'',
			'docs',
		]);
		expect(readmeDirectories([])).toEqual(['']);
	});

	it("resolves a relative link against the file's directory, and leaves the rest", () => {
		expect(repositoryLinkTarget('.github', '../../../x.md')).toEqual({ path: 'x.md', isDirectory: false, fragment: '' });
		expect(repositoryLinkTarget('docs', './a%20b.md?plain=1#L3')).toEqual({
			path: 'docs/a b.md',
			isDirectory: false,
			fragment: '#L3',
		});
		expect(repositoryLinkTarget('docs', '..')).toEqual({ path: '', isDirectory: true, fragment: '' });
		for (const absolute of ['/alice', '//cdn.example', 'https://x', 'mailto:a@b', '#top', '?q', 'javascript:alert(1)']) {
			expect(repositoryLinkTarget('docs', absolute)).toBeNull();
		}
	});
});

describe('a Markdown file', () => {
	it('leads its relative links and images from its own directory', async () => {
		repos.blob.mockResolvedValue({
			path: 'docs/guide/intro.md',
			sha: 'b'.repeat(40),
			size: 40,
			content: '[Next](next.md) [Up](../README.md)\n\n![shot](img/shot.png)',
			encoding: 'utf-8',
			is_binary: false,
			name: 'intro.md',
		});
		repos.rawUrl.mockImplementation((owner: string, repo: string, path: string, ref?: string) =>
			`/api/raw/${owner}/${repo}/${path}?ref=${ref}`,
		);
		setTestPage('/alice/demo/blob/docs/guide/intro.md?ref=main', {
			owner: 'alice',
			repo: 'demo',
			path: 'docs/guide/intro.md',
		});
		rendered = await renderComponent(BlobPage);
		await settle();
		await new Promise((resolve) => setTimeout(resolve, 0));
		await settle();
		const body = element(rendered.container, '.markdown-body');

		expect(Array.from(body.querySelectorAll('a'), (a) => a.getAttribute('href'))).toEqual([
			'/alice/demo/blob/docs/guide/next.md?ref=main',
			'/alice/demo/blob/docs/README.md?ref=main',
		]);
		expect(body.querySelector('img')?.getAttribute('src')).toBe('/api/raw/alice/demo/docs/guide/img/shot.png?ref=main');
	});
});
