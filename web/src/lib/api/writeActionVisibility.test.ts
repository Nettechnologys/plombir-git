import { afterEach, beforeEach, describe, expect, it } from 'vitest';

// card_3625a7b89abb, the sideways sweep: these pages offered their write
// controls to every visitor, so a reader's click could only end in a 403. They
// now follow `viewer_permission` from `GET /repos/{owner}/{name}` — `write` or
// `admin` sees the controls, `read` and an anonymous reader (no field) do not.
import LabelsPage from '../../routes/[owner]/[repo]/settings/labels/+page.svelte';
import MilestonesPage from '../../routes/[owner]/[repo]/milestones/+page.svelte';
import EditReleasePage from '../../routes/[owner]/[repo]/releases/edit/[id]/+page.svelte';
import NewReleasePage from '../../routes/[owner]/[repo]/releases/new/+page.svelte';
import ReleasesPage from '../../routes/[owner]/[repo]/releases/+page.svelte';
import WikiIndexPage from '../../routes/[owner]/[repo]/wiki/+page.svelte';
import WikiPage from '../../routes/[owner]/[repo]/wiki/[title]/+page.svelte';
import { setTestPage } from '../test/app';
import { instance, labels, milestones, releases, repos, resetTestClient, wiki } from '../test/client';
import { renderComponent, type RenderedComponent } from '../test/render';

type Level = 'admin' | 'write' | 'read' | null;

let rendered: RenderedComponent | undefined;

function viewerIs(level: Level) {
	repos.get.mockResolvedValue(
		level === null ? { name: 'demo', default_branch: 'main' } : { name: 'demo', default_branch: 'main', viewer_permission: level },
	);
}

beforeEach(() => {
	resetTestClient();
	instance.get.mockResolvedValue({ attestation_enabled: false });
	releases.list.mockResolvedValue({
		data: [
			{ id: 7, tag_name: 'v1.0.0', title: 'Version 1', body: '', is_prerelease: false, is_draft: false, created_at: '2026-08-15T12:00:00Z' },
		],
		pagination: { total_pages: 1 },
	});
	releases.listAssets.mockResolvedValue([
		{
			id: 3,
			release_id: 7,
			filename: 'tool.tar.gz',
			size: 10,
			content_type: 'application/gzip',
			download_count: 0,
			uploader_id: 1,
			created_at: '2026-08-15T12:00:00Z',
			sha256: null,
		},
	]);
	labels.list.mockResolvedValue([{ id: 1, name: 'bug', color: '#ff0000', description: '' }]);
	milestones.list.mockResolvedValue([{ id: 7, title: 'v1.0', description: '', due_date: null, state: 'open' }]);
	wiki.list.mockResolvedValue([{ title: 'Home', updated_at: '2026-08-15T12:00:00Z' }]);
	wiki.get.mockResolvedValue({ title: 'Home', content: '# Home', updated_at: '2026-08-15T12:00:00Z' });
	repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
	repos.tags.mockResolvedValue([]);
	releases.get.mockResolvedValue({ id: 7, tag_name: 'v1.0.0', title: 'Version 1', body: '', is_draft: false, is_prerelease: false });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
});

function buttonsLabelled(container: ParentNode, label: string): HTMLButtonElement[] {
	return Array.from(container.querySelectorAll<HTMLButtonElement>('button')).filter(
		(candidate) => candidate.textContent?.trim() === label || candidate.title === label,
	);
}

describe.each([
	['write', true],
	['admin', true],
	['read', false],
	[null, false],
] as const)('viewer_permission %s', (level, writes) => {
	it(`${writes ? 'offers' : 'withholds'} the release write controls`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(ReleasesPage);

		expect(rendered.container.textContent).toContain('Version 1');
		expect(rendered.container.textContent).toContain('tool.tar.gz');
		expect(Boolean(rendered.container.querySelector('a[href="/alice/demo/releases/new"]'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('a[href="/alice/demo/releases/edit/7"]'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.asset-upload'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.asset-delete'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.action-link.danger'))).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} the new- and edit-release forms`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/releases/new', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(NewReleasePage);

		expect(Boolean(rendered.container.querySelector('form.release-form'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.write-required'))).toBe(!writes);
		await rendered.destroy();

		setTestPage('/alice/demo/releases/edit/7', { owner: 'alice', repo: 'demo', id: '7' });
		rendered = await renderComponent(EditReleasePage);

		expect(Boolean(rendered.container.querySelector('form.release-form'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.write-required'))).toBe(!writes);
	});

	it(`${writes ? 'offers' : 'withholds'} the label controls`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/settings/labels', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(LabelsPage);

		expect(rendered.container.textContent).toContain('bug');
		expect(Boolean(rendered.container.querySelector('.page-header .btn-primary'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.label-actions'))).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} the milestone controls`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/milestones', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(MilestonesPage);

		expect(rendered.container.textContent).toContain('v1.0');
		expect(Boolean(rendered.container.querySelector('section.editor'))).toBe(writes);
		expect(Boolean(rendered.container.querySelector('.milestone-actions'))).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} the wiki controls`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/wiki', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(WikiIndexPage);

		expect(rendered.container.textContent).toContain('Home');
		expect(buttonsLabelled(rendered.container, 'New Page').length > 0).toBe(writes);
		await rendered.destroy();

		setTestPage('/alice/demo/wiki/Home', { owner: 'alice', repo: 'demo', title: 'Home' });
		rendered = await renderComponent(WikiPage);

		expect(rendered.container.querySelector('.wiki-content, .markdown-body')).not.toBeNull();
		expect(buttonsLabelled(rendered.container, 'Edit').length > 0).toBe(writes);
		expect(buttonsLabelled(rendered.container, 'Delete').length > 0).toBe(writes);
	});
});
