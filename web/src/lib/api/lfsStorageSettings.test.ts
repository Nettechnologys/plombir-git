import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import LfsStoragePage from '../../routes/[owner]/[repo]/settings/lfs-storage/+page.svelte';
import { setTestPage } from '../test/app';
import { lfsStorage, resetTestClient } from '../test/client';
import { button, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_9e4dd3f8330c: the repository settings show how much the LFS store
// holds, and let an administrator find and remove objects nothing needs.

function object(oid: string, size: number) {
	return { oid: oid.repeat(64).slice(0, 64), size, uploaded: true, created_at: '2026-10-01T00:00:00Z' };
}

let rendered: RenderedComponent | undefined;

async function open() {
	setTestPage('/alice/demo/settings/lfs-storage', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(LfsStoragePage);
	await settle();
	return rendered.container;
}

beforeEach(() => {
	resetTestClient();
	vi.spyOn(window, 'confirm').mockReturnValue(true);
	lfsStorage.usage.mockResolvedValue({ object_count: 2, total_bytes: 3 * 1024 * 1024 });
	lfsStorage.objects.mockResolvedValue({
		objects: [object('a', 1024 * 1024), object('b', 2 * 1024 * 1024)],
		next_cursor: '',
	});
});

afterEach(async () => {
	vi.restoreAllMocks();
	await rendered?.destroy();
	rendered = undefined;
});

describe('LFS storage settings tab', () => {
	it('shows the store size and its objects', async () => {
		const page = await open();

		expect(page.querySelector('.usage')?.textContent).toContain('2 objects, 3.0 MiB');
		expect(page.querySelectorAll('.object-table tbody tr')).toHaveLength(2);
		expect(lfsStorage.orphans).not.toHaveBeenCalled();
	});

	it('finds unreferenced objects and removes the selected ones', async () => {
		const orphan = object('b', 2 * 1024 * 1024);
		lfsStorage.orphans.mockResolvedValue({ objects: [orphan], grace_hours: 24 });
		lfsStorage.prune.mockResolvedValue({ deleted: [orphan.oid], kept: [] });

		const page = await open();
		button(page, 'Find unreferenced objects').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(page.querySelectorAll('.orphan-list li')).toHaveLength(1);
		expect(page.textContent).toContain('older than 24 hours');
		button(page, 'Remove selected').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(window.confirm).toHaveBeenCalled();
		expect(lfsStorage.prune).toHaveBeenCalledWith('alice', 'demo', [orphan.oid]);
		expect(page.querySelector('.outcome')?.textContent).toContain('Removed 1 objects.');
		expect(page.querySelectorAll('.orphan-list li')).toHaveLength(0);
	});

	it('names an object the server kept and why', async () => {
		const orphan = object('c', 10);
		lfsStorage.orphans.mockResolvedValue({ objects: [orphan], grace_hours: 24 });
		lfsStorage.prune.mockResolvedValue({
			deleted: [],
			kept: [{ oid: orphan.oid, reason: "a ref's history points at it" }],
		});

		const page = await open();
		button(page, 'Find unreferenced objects').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		button(page, 'Remove selected').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(page.querySelector('.kept')?.textContent).toContain("a ref's history points at it");
		expect(page.querySelectorAll('.orphan-list li')).toHaveLength(1);
	});

	it('says so when nothing can be removed', async () => {
		lfsStorage.orphans.mockResolvedValue({ objects: [], grace_hours: 24 });

		const page = await open();
		button(page, 'Find unreferenced objects').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(page.textContent).toContain('Every object is still referenced or too recent to remove.');
		expect(() => button(page, 'Remove selected')).toThrow();
	});
});

describe('LFS storage settings tab, one object', () => {
	it('asks the server to remove a single object and shows its answer', async () => {
		const listed = object('a', 1024 * 1024);
		lfsStorage.prune.mockResolvedValue({
			deleted: [],
			kept: [{ oid: listed.oid, reason: 'uploaded too recently; a push may still point a ref at it' }],
		});

		const page = await open();
		const row = page.querySelector('.object-table tbody tr')!;
		(row.querySelector('button') as HTMLButtonElement).dispatchEvent(
			new MouseEvent('click', { bubbles: true }),
		);
		await settle();

		expect(lfsStorage.prune).toHaveBeenCalledWith('alice', 'demo', [listed.oid]);
		expect(page.querySelector('.kept')?.textContent).toContain('uploaded too recently');
	});
});
