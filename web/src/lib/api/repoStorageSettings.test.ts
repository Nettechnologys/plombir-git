import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import RepositorySettingsPage from '../../routes/[owner]/[repo]/settings/+page.svelte';
import { setTestPage } from '../test/app';
import { repoStorage, repos, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';

// Security audit finding #10: the repository settings page shows what the
// repository holds against the budget every upload path enforces, so a refusal
// is a number the owner could have seen coming.

const GIB = 1024 * 1024 * 1024;

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		description: null,
		is_private: false,
		default_branch: 'main',
		created_at: '2026-08-30T00:00:00Z',
		viewer_permission: 'admin',
	});
	repos.branches.mockResolvedValue([]);
	repoStorage.get.mockResolvedValue({
		usage: {
			lfs_bytes: 2 * 1024 * 1024,
			lfs_objects: 1,
			release_bytes: 1024,
			release_assets: 1,
			attachment_bytes: 0,
			ci_cache_bytes: 1024 * 1024,
			ci_cache_entries: 1,
			package_bytes: 0,
			package_files: 0,
			oci_bytes: 0,
			oci_blobs: 0,
			total_bytes: 3 * 1024 * 1024,
		},
		limits: {
			repo_quota_bytes: 20 * GIB,
			oci_blob_max_bytes: 10 * GIB,
			ci_cache_max_entries_per_repo: 200,
			release_assets_max_per_release: 100,
		},
	});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function open(): Promise<HTMLElement> {
	rendered = await renderComponent(RepositorySettingsPage);
	await settle();
	return rendered.container;
}

describe('repository settings storage read-out', () => {
	it('shows usage against the enforced quota', async () => {
		const page = await open();

		const section = page.querySelector('.storage-section');
		expect(repoStorage.get).toHaveBeenCalledWith('alice', 'demo');
		expect(section?.textContent).toContain('3.0 MiB of 20.00 GiB used');
		expect(section?.textContent).toContain('LFS 2.0 MiB');
	});

	it('keeps the page usable when the usage read fails', async () => {
		repoStorage.get.mockRejectedValue(new Error('storage offline'));

		const page = await open();

		expect(page.querySelector('.storage-section')?.textContent).toContain('storage offline');
	});
});
