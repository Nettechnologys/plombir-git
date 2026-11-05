import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import SecurityPage from '../../routes/settings/security/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth, mfa, passkeys, resetTestClient } from '../test/client';
import {
	answerConfirm, button, click, element, input, renderComponent, settle, submit,
	type RenderedComponent,
} from '../test/render';

const setupResponse = {
	secret: 'JBSWY3DPEHPK3PXP',
	otpauth_url: 'otpauth://totp/Test:alice?secret=JBSWY3DPEHPK3PXP',
	qr_svg: '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 200"><rect width="200" height="200" /></svg>',
};
const backupCodes = ['TEST-CODE-ONE', 'TEST-CODE-TWO'];
let rendered: RenderedComponent;

beforeEach(async () => {
	vi.clearAllMocks();
	resetTestClient();
	auth.me.mockResolvedValue({ id: 1, username: 'alice', has_password: true });
	auth.listSsoLinks.mockResolvedValue([]);
	auth.listSsoProviders.mockResolvedValue([]);
	mfa.backup.mockResolvedValue({ total: 0, unused: 0, codes: [] });
	mfa.setup.mockResolvedValue(setupResponse);
	mfa.enable.mockResolvedValue({ enabled: true, backup_codes: backupCodes });
	passkeys.list.mockResolvedValue([]);
	await fetchUser();
	rendered = await renderComponent(SecurityPage);
});

afterEach(async () => {
	await rendered?.destroy();
	await logout();
});

function dialog(): HTMLElement {
	return element(rendered.container, '[role="dialog"]');
}

async function openSetup() {
	const opener = button(rendered.container, 'Set up MFA');
	opener.focus();
	await click(opener);
	return opener;
}

async function fillSetup() {
	await input(element(dialog(), 'input[autocomplete="one-time-code"]'), '123456');
	await input(element(dialog(), 'input[type="password"]'), 'Test-password-1!');
}

describe('MFA enrollment dialogs', () => {
	it('opens the returned QR, manual secret and confirmation form in a focused modal', async () => {
		await openSetup();
		const panel = dialog();
		expect(panel.getAttribute('aria-modal')).toBe('true');
		expect(document.getElementById(panel.getAttribute('aria-labelledby')!)?.textContent)
			.toBe('Scan Authenticator QR');
		expect(panel.querySelector('.qr svg')).not.toBeNull();
		expect(panel.textContent).toContain(setupResponse.secret);
		expect(document.activeElement).toBe(panel.querySelector('input[autocomplete="one-time-code"]'));
		expect(button(panel, 'Enable MFA').disabled).toBe(true);
	});

	it.each(['Cancel', 'Escape', 'backdrop'])('cancels setup with %s without enabling MFA and clears the form', async (method) => {
		const opener = await openSetup();
		await fillSetup();
		if (method === 'Escape') {
			dialog().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
			await settle();
		} else {
			await click(method === 'backdrop'
				? element(rendered.container, '.modal-backdrop') : button(dialog(), method));
		}
		expect(rendered.container.querySelector('[role="dialog"]')).toBeNull();
		expect(rendered.container.textContent).not.toContain(setupResponse.secret);
		expect(document.activeElement).toBe(opener);
		expect(mfa.enable).not.toHaveBeenCalled();
		expect(mfa.disable).not.toHaveBeenCalled();
		await openSetup();
		expect(element<HTMLInputElement>(dialog(), 'input[autocomplete="one-time-code"]').value).toBe('');
		expect(element<HTMLInputElement>(dialog(), 'input[type="password"]').value).toBe('');
	});

	it('keeps an enable error and the QR inside the dialog so the user can retry', async () => {
		mfa.enable.mockRejectedValueOnce(new Error('invalid TOTP code'));
		await openSetup();
		await fillSetup();
		await submit(element(dialog(), 'form'));
		expect(element(dialog(), '[role="alert"]').textContent).toContain('invalid TOTP code');
		expect(dialog().querySelector('.qr svg')).not.toBeNull();
		expect(button(dialog(), 'Enable MFA').disabled).toBe(false);
	});

	it('shows newly issued backup codes in a modal after a successful confirmation', async () => {
		await openSetup();
		await fillSetup();
		mfa.backup.mockResolvedValue({ total: 2, unused: 2, codes: [] });
		await submit(element(dialog(), 'form'));
		expect(mfa.enable).toHaveBeenCalledWith('123456', 'Test-password-1!');
		expect(dialog().textContent).toContain('Backup Codes');
		for (const code of backupCodes) expect(dialog().textContent).toContain(code);
		expect(rendered.container.textContent).not.toContain(setupResponse.secret);
		expect(dialog().contains(document.activeElement)).toBe(true);
		await click(button(dialog(), 'Close'));
		expect(rendered.container.querySelector('[role="dialog"]')).toBeNull();
		for (const code of backupCodes) expect(rendered.container.textContent).not.toContain(code);
		expect(rendered.container.textContent).toContain('unused backup codes');
	});

	it('keeps the dialog open while confirmation is pending', async () => {
		let complete!: (value: unknown) => void;
		mfa.enable.mockImplementationOnce(() => new Promise((resolve) => { complete = resolve; }));
		await openSetup();
		await fillSetup();
		await submit(element(dialog(), 'form'));
		dialog().dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
		await click(element(rendered.container, '.modal-backdrop'));
		expect(dialog().querySelector('.qr svg')).not.toBeNull();
		expect(button(dialog(), 'Cancel').disabled).toBe(true);
		complete({ enabled: true, backup_codes: backupCodes });
		await settle();
		expect(dialog().textContent).toContain('Backup Codes');
	});

	it('also shows replacement backup codes in a modal after regeneration', async () => {
		await rendered.destroy();
		mfa.backup.mockResolvedValue({ total: 2, unused: 1, codes: [] });
		mfa.regenerateBackup.mockResolvedValue({ backup_codes: backupCodes });
		rendered = await renderComponent(SecurityPage);
		const form = button(rendered.container, 'Regenerate backup codes').closest('form')!;
		expect(form.querySelector('input[type="password"]')).toBeNull();
		await submit(form);
		await answerConfirm();
		expect(mfa.regenerateBackup).toHaveBeenCalledWith();
		for (const code of backupCodes) expect(dialog().textContent).toContain(code);
	});
});
