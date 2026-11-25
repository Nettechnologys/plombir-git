import { afterEach, beforeEach, expect, it } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import { setTestPage } from '../test/app';
import { releases, repos, resetTestClient } from '../test/client';
import { click, element, input, renderComponent, settle, type RenderedComponent } from '../test/render';
import CommitsPage from '../../routes/[owner]/[repo]/commits/+page.svelte';

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

const commit = (sha: string, message: string, date = '2026-11-20T00:00:00Z') => ({
	sha: sha.repeat(40).slice(0, 40),
	message,
	author: 'alice',
	date,
});

it('shows the last commit of every entry and the newest commit above the list', async () => {
	repos.treeCommits.mockResolvedValue({
		entries: { docs: commit('b', 'Write the guide'), 'CHANGELOG.md': commit('c', 'Release notes') },
		complete: true,
	});
	repos.commitCount.mockResolvedValue({ count: 12, complete: true });
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	await settle();

	const docs = element(rendered.container, '.entry:has(a[href="/alice/demo?path=docs"])');
	const message = element<HTMLAnchorElement>(docs, 'a.entry-commit');
	expect(message.textContent).toBe('Write the guide');
	expect(message.getAttribute('href')).toBe(`/alice/demo/commits/${'b'.repeat(40)}`);
	expect(element(docs, '.entry-date').textContent).not.toBe('');
	// An entry the walk did not place shows no commit rather than a wrong one.
	const src = element(rendered.container, '.entry:has(a[href="/alice/demo?path=src"])');
	expect(src.querySelector('a.entry-commit')).toBeNull();

	const latest = element(rendered.container, '.latest-commit');
	expect(latest.textContent).toContain('alice');
	expect(latest.textContent).toContain('init');
	expect(latest.textContent).toContain('a'.repeat(7));
	const history = element<HTMLAnchorElement>(latest, '.history-link');
	expect(history.getAttribute('href')).toBe('/alice/demo/commits');
	expect(history.textContent?.replace(/\s+/g, ' ').trim()).toBe('12 commits');
});

it('links a directory to its own history and does not count the whole repository', async () => {
	setTestPage('/alice/demo?ref=dev&path=src%2Flib', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	await settle();
	expect(repos.treeCommits).toHaveBeenCalledWith('alice', 'demo', 'dev', 'src/lib');
	expect(repos.commitCount).not.toHaveBeenCalled();
	const history = element<HTMLAnchorElement>(rendered.container, '.history-link');
	expect(history.getAttribute('href')).toBe('/alice/demo/commits?ref=dev&path=src%2Flib');
	expect(history.textContent?.trim()).toBe('History');
	// The About column belongs to the root only.
	expect(rendered.container.querySelector('.repo-about')).toBeNull();
});

it('keeps the listing when the extra parts fail to load', async () => {
	repos.treeCommits.mockRejectedValue(new Error('walk failed'));
	repos.commitCount.mockRejectedValue(new Error('count failed'));
	repos.languages.mockRejectedValue(new Error('languages failed'));
	releases.list.mockRejectedValue(new Error('releases failed'));
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	await settle();
	expect(names()).toEqual(['docs', 'src', '.dockerignore', 'CHANGELOG.md', 'Dockerfile']);
	expect(rendered.container.querySelector('.error-banner')).toBeNull();
	expect(element(rendered.container, '.history-link').textContent?.trim()).toBe('History');
});

it('describes the repository with its latest release and languages', async () => {
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		description: 'A demo repository',
		default_branch: 'main',
		stars_count: 1,
		forks_count: 3,
		is_private: false,
		created_at: '2026-08-31T00:00:00Z',
		viewer_permission: 'read',
	});
	releases.list.mockResolvedValue({
		data: [
			{ id: 9, tag_name: 'v2.0-rc', title: 'RC', is_prerelease: true, is_draft: false, created_at: '2026-11-22T00:00:00Z' },
			{ id: 7, tag_name: 'v1.0', title: '', is_prerelease: false, is_draft: false, created_at: '2026-11-01T00:00:00Z' },
		],
		pagination: { total: 2, total_pages: 1, page: 1, per_page: 10 },
	});
	repos.languages.mockResolvedValue({
		languages: [
			{ name: 'Rust', bytes: 7000 },
			{ name: 'Svelte', bytes: 2990 },
			{ name: 'Shell', bytes: 10 },
		],
		complete: true,
	});
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	await settle();

	const about = element(rendered.container, '.repo-about');
	expect(about.textContent).toContain('A demo repository');
	expect(about.textContent?.replace(/\s+/g, ' ')).toContain('1 star');
	expect(about.textContent?.replace(/\s+/g, ' ')).toContain('3 forks');
	const release = element<HTMLAnchorElement>(about, '.about-release');
	expect(release.getAttribute('href')).toBe('/alice/demo/releases?release=7');
	expect(release.textContent).toContain('v1.0');
	expect(element(about, '.about-count').textContent).toBe('2');
	const languages = [...about.querySelectorAll('.language-list li')].map((li) => li.textContent?.replace(/\s+/g, ' ').trim());
	expect(languages).toEqual(['Rust 70.0%', 'Svelte 29.9%', 'Other 0.1%']);
	expect(repos.languages).toHaveBeenCalledWith('alice', 'demo', undefined);
});

it('offers an outline of the README headings', async () => {
	repos.tree.mockResolvedValue({ entries: [{ name: 'README.md', kind: 'blob', size: 10 }] });
	repos.blob.mockResolvedValue({ content: '# Title\n\ntext\n\n## Install\n\n## Usage\n' });
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	await settle();
	const toggle = element<HTMLButtonElement>(rendered.container, '.outline-toggle');
	await click(toggle);
	const items = [...rendered.container.querySelectorAll('.outline-item')].map((item) => item.textContent);
	expect(items).toEqual(['Title', 'Install', 'Usage']);
	const scrolled: string[] = [];
	for (const heading of rendered.container.querySelectorAll<HTMLElement>('.markdown-body h1, .markdown-body h2')) {
		heading.scrollIntoView = () => scrolled.push(heading.textContent ?? '');
	}
	await click(rendered.container.querySelectorAll('.outline-item')[1]);
	expect(scrolled).toEqual(['Install']);
	expect(rendered.container.querySelector('.outline-list')).toBeNull();
});

it('narrows the commit history to the path in the URL', async () => {
	repos.log.mockResolvedValue({ commits: [commit('d', 'Touch lib')] });
	setTestPage('/alice/demo/commits?ref=dev&path=src%2Flib', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(CommitsPage);
	await settle();
	expect(repos.log).toHaveBeenCalledWith('alice', 'demo', 'dev', 'src/lib', 50);
	expect(element(rendered.container, '.history-path').textContent).toBe('History for src/lib');
});

it('finds files when typing starts without a focus event', async () => {
	// A page that gained the text some other way than a focus — autofill, a
	// focus the browser did not report — still has to search the files.
	repos.files.mockResolvedValue({ paths: ['README.md', 'src/lib/api/mod.rs'], complete: true });
	setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(RepositoryPage);
	await input(element<HTMLInputElement>(rendered.container, '.go-to-file-input'), 'mod');
	await settle();
	const items = [...rendered.container.querySelectorAll('.go-to-file-item:not(.go-to-file-search)')];
	expect(items.map((item) => item.textContent)).toEqual(['src/lib/api/mod.rs']);
});
