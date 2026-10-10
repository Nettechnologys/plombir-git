import { afterEach, beforeEach, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import SigningKeysPage from '../../routes/settings/signing-keys/+page.svelte';
import { setTestPage } from '../test/app';
import { resetTestClient, signingKeys } from '../test/client';
import {
	answerConfirm,
	change,
	click,
	element,
	input,
	renderComponent,
	submit,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/settings/signing-keys', {});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

it('registers a signing key and deletes it only after confirmation', async () => {
	const key = {
		id: 7,
		title: 'Signing laptop',
		kind: 'gpg',
		public_key: 'public key',
		fingerprint: 'ABCDEF1234',
		created_at: '2026-10-10T00:00:00Z',
	};
	signingKeys.list
		.mockResolvedValueOnce([])
		.mockResolvedValueOnce([key])
		.mockResolvedValueOnce([]);
	signingKeys.create.mockResolvedValue(key);
	signingKeys.delete.mockResolvedValue(undefined);
	rendered = await renderComponent(SigningKeysPage);

	await input(element(rendered.container, '#signing-title'), key.title);
	await change(element(rendered.container, '#signing-kind'), 'gpg');
	await input(element(rendered.container, '#signing-public-key'), key.public_key);
	await submit(element(rendered.container, 'form.create-form'));
	expect(signingKeys.create).toHaveBeenCalledWith(key.title, 'gpg', key.public_key);
	expect(rendered.container.textContent).toContain(key.fingerprint);

	await click(element(rendered.container, '.key-card .btn-danger'));
	expect(signingKeys.delete).not.toHaveBeenCalled();
	expect(await answerConfirm()).toContain(key.title);
	expect(signingKeys.delete).toHaveBeenCalledWith(key.id);
	expect(signingKeys.list).toHaveBeenCalledTimes(3);
	expect(rendered.container.textContent).not.toContain(key.fingerprint);
});
