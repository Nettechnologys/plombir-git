import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import PushMirrorsPage from '../../routes/[owner]/[repo]/settings/push-mirrors/+page.svelte';
import { setTestPage } from '../test/app';
import { pushMirrors, resetTestClient } from '../test/client';
import {
	answerConfirm,
	button,
	check,
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

const mirror = (overrides: Record<string, unknown> = {}) => ({
	id: 7,
	url: 'https://github.example/alice/demo.git',
	username: 'mirror-bot',
	has_credentials: true,
	sync_on_commit: true,
	sync_interval_seconds: 28_800,
	force: false,
	enabled: true,
	next_sync_at: null,
	pending_since: null,
	last_sync_at: null,
	last_success_at: '2026-10-15T10:00:00Z',
	last_sync_error: null,
	created_at: '2026-10-15T09:00:00Z',
	updated_at: '2026-10-15T09:00:00Z',
	...overrides,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings/push-mirrors', { owner: 'alice', repo: 'demo' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function open() {
	rendered = await renderComponent(PushMirrorsPage);
	await settle();
	return rendered.container;
}

describe('push mirror settings', () => {
	it('adds a mirror with what the form says, force off unless ticked', async () => {
		pushMirrors.list.mockResolvedValueOnce([]).mockResolvedValueOnce([mirror()]);
		pushMirrors.create.mockResolvedValue(mirror());
		const container = await open();
		expect(element(container, '.empty-state')).toBeTruthy();

		await input(element(container, '#push-mirror-url'), ' https://github.example/alice/demo.git ');
		await input(element(container, '#push-mirror-username'), 'mirror-bot');
		await input(element(container, '#push-mirror-password'), 'token');
		await input(element(container, '#push-mirror-interval'), '0');
		await submit(element<HTMLFormElement>(container, 'form.mirror-form'));

		expect(pushMirrors.create).toHaveBeenCalledWith('alice', 'demo', {
			url: 'https://github.example/alice/demo.git',
			username: 'mirror-bot',
			password: 'token',
			sync_on_commit: true,
			sync_interval_seconds: 0,
			force: false,
		});
		expect(container.querySelector('.mirror-url')?.textContent).toBe(
			'https://github.example/alice/demo.git',
		);
	});

	it('sends force only when the owner ticks it', async () => {
		pushMirrors.list.mockResolvedValue([]);
		pushMirrors.create.mockResolvedValue(mirror({ force: true }));
		const container = await open();

		await input(element(container, '#push-mirror-url'), 'https://github.example/alice/demo.git');
		await check(element(container, '#push-mirror-force'), true);
		await submit(element<HTMLFormElement>(container, 'form.mirror-form'));

		expect(pushMirrors.create.mock.calls[0][2].force).toBe(true);
	});

	it('pushes now, switches off and deletes the mirror it is shown next to', async () => {
		pushMirrors.list.mockResolvedValue([mirror()]);
		pushMirrors.sync.mockResolvedValue({ status: 'sync_queued' });
		pushMirrors.update.mockResolvedValue(mirror({ enabled: false }));
		pushMirrors.remove.mockResolvedValue(undefined);
		const container = await open();

		await click(button(container, 'Push now'));
		expect(pushMirrors.sync).toHaveBeenCalledWith('alice', 'demo', 7);

		await click(button(container, 'Disable'));
		expect(pushMirrors.update).toHaveBeenCalledWith('alice', 'demo', 7, { enabled: false });

		await click(button(container, 'Delete'));
		const message = await answerConfirm(true);
		expect(message).toContain('https://github.example/alice/demo.git');
		expect(pushMirrors.remove).toHaveBeenCalledWith('alice', 'demo', 7);
	});

	it('shows a mirror that is behind and why its last push failed', async () => {
		pushMirrors.list.mockResolvedValue([
			mirror({
				pending_since: '2026-10-15T11:00:00Z',
				last_sync_error: 'push mirror remote URL failed transport/SSRF validation',
			}),
		]);
		const container = await open();

		expect(container.querySelector('.lagging')).not.toBeNull();
		expect(element(container, '.error-detail').textContent).toContain('SSRF validation');
	});
});
