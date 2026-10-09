import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import LfsLocksPage from '../../routes/[owner]/[repo]/settings/lfs-locks/+page.svelte';
import { setTestPage } from '../test/app';
import { ApiError, lfsLocks, resetTestClient } from '../test/client';
import { answerConfirm, button, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_8062fa65ca75: the repository settings list the files locked with
// `git lfs lock`, and an administrator can take a lock off.

function lock(id: string, path: string, owner: string) {
	return { id, path, owner: { name: owner }, locked_at: '2026-10-07T08:00:00Z' };
}

let rendered: RenderedComponent | undefined;

async function open() {
	setTestPage('/alice/demo/settings/lfs-locks', { owner: 'alice', repo: 'demo' });
	rendered = await renderComponent(LfsLocksPage);
	await settle();
	return rendered.container;
}

beforeEach(() => {
	resetTestClient();
});

afterEach(async () => {
	vi.restoreAllMocks();
	await rendered?.destroy();
	rendered = undefined;
});

describe('LFS locks settings tab', () => {
	it('lists each lock with its path, holder and time', async () => {
		lfsLocks.list.mockResolvedValue({
			locks: [lock('1', 'maps/castle.level', 'bob'), lock('2', 'art/hero.psd', 'carol')],
			next_cursor: '',
		});

		const page = await open();

		expect(lfsLocks.list).toHaveBeenCalledWith('alice', 'demo', undefined);
		const rows = Array.from(page.querySelectorAll('tbody tr')).map((row) => row.textContent);
		expect(rows).toHaveLength(2);
		expect(rows[0]).toContain('maps/castle.level');
		expect(rows[0]).toContain('bob');
		expect(rows[1]).toContain('art/hero.psd');
	});

	it('force-unlocks a lock and drops it from the list', async () => {
		lfsLocks.list.mockResolvedValue({
			locks: [lock('7', 'maps/castle.level', 'bob')],
			next_cursor: '',
		});
		lfsLocks.forceUnlock.mockResolvedValue({ lock: lock('7', 'maps/castle.level', 'bob') });

		const page = await open();
		button(page, 'Force unlock').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(lfsLocks.forceUnlock).not.toHaveBeenCalled();
		expect(await answerConfirm()).toContain('maps/castle.level');
		expect(lfsLocks.forceUnlock).toHaveBeenCalledWith('alice', 'demo', '7');
		expect(page.querySelector('tbody')).toBeNull();
		expect(page.textContent).toContain('Unlocked maps/castle.level.');
	});

	it('keeps the lock and says why when the server refuses', async () => {
		lfsLocks.list.mockResolvedValue({
			locks: [lock('7', 'maps/castle.level', 'bob')],
			next_cursor: '',
		});
		lfsLocks.forceUnlock.mockRejectedValue(
			new ApiError('repository admin access required', 403, 'FORBIDDEN', null),
		);

		const page = await open();
		button(page, 'Force unlock').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		await answerConfirm();

		expect(page.querySelector('[role="alert"]')?.textContent).toContain(
			'repository admin access required',
		);
		expect(page.querySelectorAll('tbody tr')).toHaveLength(1);
	});

	it('pages through locks and says so when there are none', async () => {
		lfsLocks.list
			.mockResolvedValueOnce({ locks: [lock('1', 'a.level', 'bob')], next_cursor: '1' })
			.mockResolvedValueOnce({ locks: [lock('2', 'b.level', 'bob')], next_cursor: '' });

		const page = await open();
		button(page, 'Load more').dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();

		expect(lfsLocks.list).toHaveBeenLastCalledWith('alice', 'demo', '1');
		expect(page.querySelectorAll('tbody tr')).toHaveLength(2);
		expect(() => button(page, 'Load more')).toThrow();

		await rendered?.destroy();
		lfsLocks.list.mockResolvedValue({ locks: [], next_cursor: '' });
		const empty = await open();
		expect(empty.textContent).toContain('No files are locked.');
	});
});
