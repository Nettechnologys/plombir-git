import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const session = vi.hoisted(() => ({ user: null as null | { mfa_enabled: boolean } }));

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => session.user,
}));

import SudoPrompt from '../components/SudoPrompt.svelte';
import { requestSudo, setSudoPrompt } from './_base.svelte';
import { ApiError, auth, resetTestClient, setToken } from '../test/client';
import { openDialog } from '../test/modalContract';
import { button, click, element, input, renderComponent, settle, submit, type RenderedComponent } from '../test/render';

// The one prompt behind every `sudo_required` refusal: it registers itself
// with the API client on mount, asks for the password (and the second factor
// when MFA is enrolled), stores the re-issued session and answers `true`;
// cancelling answers `false` and stores nothing.

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	resetTestClient();
	session.user = { mfa_enabled: false };
	rendered = await renderComponent(SudoPrompt);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	setSudoPrompt(null);
});

function form(): HTMLFormElement {
	return element(document.body, 'form.sudo-prompt');
}

describe('SudoPrompt', () => {
	it('confirms the password, stores the re-issued session and resolves true', async () => {
		auth.sudo.mockResolvedValue({ token: 'elevated', user_id: 1, username: 'alice' });

		const outcome = requestSudo();
		await settle();
		const dialog = openDialog(document.body);
		expect(dialog, 'the prompt did not open').not.toBeNull();
		expect(dialog!.contains(document.activeElement), 'focus did not move into the prompt').toBe(true);
		expect(document.getElementById('sudo-code')).toBeNull();

		await input(element(document.body, '#sudo-password'), 'Qz7$wRtm');
		await submit(form());

		expect(auth.sudo).toHaveBeenCalledWith({ password: 'Qz7$wRtm' });
		expect(setToken).toHaveBeenCalledWith('elevated');
		await expect(outcome).resolves.toBe(true);
		expect(openDialog(document.body)).toBeNull();
	});

	it('asks for the second factor when MFA is enrolled, as a TOTP or a backup code', async () => {
		session.user = { mfa_enabled: true };
		auth.sudo.mockResolvedValue({ token: 'elevated', user_id: 1, username: 'alice' });

		const outcome = requestSudo();
		await settle();
		await input(element(document.body, '#sudo-password'), 'Qz7$wRtm');
		await input(element(document.body, '#sudo-code'), '123456');
		await submit(form());
		expect(auth.sudo).toHaveBeenCalledWith({ password: 'Qz7$wRtm', totp_code: '123456' });
		await expect(outcome).resolves.toBe(true);

		const second = requestSudo();
		await settle();
		await click(button(document.body, 'Use a backup code instead'));
		await input(element(document.body, '#sudo-password'), 'Qz7$wRtm');
		await input(element(document.body, '#sudo-code'), 'abcd-efgh');
		await submit(form());
		expect(auth.sudo).toHaveBeenLastCalledWith({ password: 'Qz7$wRtm', backup_code: 'abcd-efgh' });
		await expect(second).resolves.toBe(true);
	});

	it('keeps the prompt open with the server message on a wrong password', async () => {
		auth.sudo.mockRejectedValueOnce(new ApiError('invalid password', 401, 'UNAUTHORIZED'));

		const outcome = requestSudo();
		await settle();
		await input(element(document.body, '#sudo-password'), 'wrong');
		await submit(form());

		expect(element(document.body, '[role="alert"]').textContent).toContain('invalid password');
		expect(openDialog(document.body)).not.toBeNull();
		expect(setToken).not.toHaveBeenCalled();

		await click(button(document.body, 'Cancel'));
		await expect(outcome).resolves.toBe(false);
	});

	it('resolves false on cancel without calling the server', async () => {
		const outcome = requestSudo();
		await settle();
		await click(button(document.body, 'Cancel'));

		await expect(outcome).resolves.toBe(false);
		expect(auth.sudo).not.toHaveBeenCalled();
		expect(openDialog(document.body)).toBeNull();
	});
});
