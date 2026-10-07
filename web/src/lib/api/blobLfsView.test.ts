import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import BlobPage from '../../routes/[owner]/[repo]/blob/[...path]/+page.svelte';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { button, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_5d34838459e4: a file stored in Git LFS is shown as the file, never as
// the three lines of pointer text the blob API returns for it.

const POINTER =
	'version https://git-lfs.github.com/spec/v1\noid sha256:' + 'a'.repeat(64) + '\nsize 2048\n';

function lfsBlob(path: string, available: boolean, size = 2048) {
	return {
		path,
		sha: 'b'.repeat(40),
		size: POINTER.length,
		content: POINTER,
		encoding: 'utf-8',
		is_binary: false,
		name: path.split('/').pop(),
		lfs: { oid: 'a'.repeat(64), size, available },
	};
}

let rendered: RenderedComponent | undefined;

async function open(path: string) {
	setTestPage(`/alice/demo/blob/${path}?ref=main`, { owner: 'alice', repo: 'demo', path });
	rendered = await renderComponent(BlobPage);
	await settle();
	return rendered.container;
}

beforeEach(() => {
	resetTestClient();
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		default_branch: 'main',
		stars_count: 0,
		is_private: false,
		created_at: '2026-10-07T00:00:00Z',
	});
	repos.rawUrl.mockImplementation(
		(owner: string, repo: string, path: string, ref?: string) =>
			`/api/v1/repos/${owner}/${repo}/raw/${path}?ref=${ref}`,
	);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('blob page for a file stored in Git LFS', () => {
	it('shows an LFS image from the raw route with an LFS badge and the object size', async () => {
		repos.blob.mockResolvedValue(lfsBlob('art/hero.png', true));

		const page = await open('art/hero.png');

		const image = page.querySelector<HTMLImageElement>('.image-view img');
		expect(image?.getAttribute('src')).toBe('/api/v1/repos/alice/demo/raw/art/hero.png?ref=main');
		expect(page.querySelector('.lfs-badge')?.textContent).toBe('LFS');
		expect(page.querySelector('.file-size')?.textContent).toContain('2.0');
		expect(page.textContent).not.toContain('git-lfs.github.com/spec');
		expect(repos.rawBytes).not.toHaveBeenCalled();
	});

	it('offers a binary LFS object as a download instead of the pointer', async () => {
		repos.blob.mockResolvedValue(lfsBlob('build/game.pak', true));
		repos.rawBytes.mockResolvedValue(new Uint8Array([0x50, 0x4b, 0x00, 0x03]).buffer);
		repos.downloadRaw.mockResolvedValue(undefined);

		const page = await open('build/game.pak');

		expect(repos.rawBytes).toHaveBeenCalledWith('alice', 'demo', 'build/game.pak', 'main');
		expect(page.querySelector('.code-view')).toBeNull();
		expect(page.textContent).not.toContain('git-lfs.github.com/spec');
		const download = page.querySelector<HTMLButtonElement>('.file-content button');
		expect(download?.textContent?.trim()).toBe('Download');
		download!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		expect(repos.downloadRaw).toHaveBeenCalledWith('alice', 'demo', 'build/game.pak', 'main');
		// Pointer text must never reach the editor either.
		expect(page.querySelector('a[href*="/edit/"]')).toBeNull();
	});

	it('says the object is not on the server when the repository does not have it', async () => {
		repos.blob.mockResolvedValue(lfsBlob('art/missing.png', false));

		const page = await open('art/missing.png');

		expect(page.querySelector('.lfs-missing')?.textContent).toContain(
			'has not been uploaded to the server',
		);
		expect(page.querySelector('.image-view')).toBeNull();
		expect(page.textContent).not.toContain('git-lfs.github.com/spec');
		expect(() => button(page, 'Download')).toThrow();
		expect(repos.rawBytes).not.toHaveBeenCalled();
	});

	it('shows a small text LFS object as text', async () => {
		repos.blob.mockResolvedValue(lfsBlob('data/table.csv', true, 12));
		repos.rawBytes.mockResolvedValue(new TextEncoder().encode('a,b\n1,2\n').buffer);

		const page = await open('data/table.csv');

		expect(page.querySelector('.code-view')?.textContent).toContain('1,2');
		expect(page.textContent).not.toContain('git-lfs.github.com/spec');
	});
});
