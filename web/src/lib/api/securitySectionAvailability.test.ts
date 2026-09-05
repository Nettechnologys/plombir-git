import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import SecurityPage from '../../routes/settings/security/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth, mfa, passkeys, resetTestClient } from '../test/client';
import {
	button,
	click,
	renderComponent,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;
let warn: ReturnType<typeof vi.spyOn> | undefined;

beforeEach(async () => {
	vi.clearAllMocks();
	resetTestClient();
	warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
	auth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	auth.listSsoLinks.mockResolvedValue([]);
	mfa.backup.mockResolvedValue({ total: 0, unused: 0 });
	passkeys.list.mockResolvedValue([]);
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	warn?.mockRestore();
	warn = undefined;
	await logout();
});

function statusBadge(index: number): string {
	const badges = Array.from(rendered!.container.querySelectorAll('.status'));
	const badge = badges[index];
	if (!badge) throw new Error(`Rendered DOM is missing status badge ${index}`);
	return badge.textContent?.trim() ?? '';
}

function hasButton(label: string): boolean {
	return Array.from(rendered!.container.querySelectorAll('button')).some(
		(candidate) => candidate.textContent?.trim() === label,
	);
}

// `GET /users/mfa/backup` answering 5xx used to leave `backupStatus` at the same
// `null` an account with no codes holds, so the section stated "MFA is not
// enabled" and offered "Set up MFA" — and `POST /users/mfa/setup` overwrites
// `users.totp_secret` unconditionally. A failed read therefore both claimed the
// account was unprotected and pointed at the one button that would make that
// claim true (card_59b36db201a2, card_08400088bb40).
describe('Security page MFA state availability', () => {
	it('does not draw a failed MFA read as "MFA is not enabled"', async () => {
		mfa.backup.mockRejectedValue(new Error('HTTP 500'));

		rendered = await renderComponent(SecurityPage);

		expect(rendered.container.textContent).not.toContain('MFA is not enabled for this account.');
		expect(statusBadge(0)).toBe('Unknown');
		expect(warn).toHaveBeenCalled();
	});

	it('offers no MFA setup from a state it could not read', async () => {
		mfa.backup.mockRejectedValue(new Error('HTTP 500'));

		rendered = await renderComponent(SecurityPage);

		expect(hasButton('Set up MFA')).toBe(false);
		expect(mfa.setup).not.toHaveBeenCalled();
	});

	it('still states an honest "no codes" as MFA being off, and offers setup', async () => {
		mfa.backup.mockResolvedValue({ total: 0, unused: 0 });

		rendered = await renderComponent(SecurityPage);

		expect(rendered.container.textContent).toContain('MFA is not enabled for this account.');
		expect(statusBadge(0)).toBe('Disabled');

		await click(button(rendered.container, 'Set up MFA'));
		expect(mfa.setup).toHaveBeenCalledOnce();
	});

	it('re-reads the MFA state instead of guessing it', async () => {
		mfa.backup
			.mockRejectedValueOnce(new Error('HTTP 500'))
			.mockResolvedValueOnce({ total: 8, unused: 7 });

		rendered = await renderComponent(SecurityPage);
		await click(button(rendered.container, 'Retry reading MFA state'));

		expect(mfa.backup).toHaveBeenCalledTimes(2);
		expect(statusBadge(0)).toBe('Enabled');
		expect(rendered.container.textContent).toContain('unused backup codes');
	});
});

describe('Security page passkey and linked-account availability', () => {
	it('does not draw a failed passkey read as "no passkeys"', async () => {
		passkeys.list.mockRejectedValue(new Error('HTTP 500'));

		rendered = await renderComponent(SecurityPage);

		expect(rendered.container.textContent).not.toContain('No passkeys registered yet.');
		expect(statusBadge(1)).toBe('Unknown');
		expect(warn).toHaveBeenCalled();
	});

	it('does not draw a failed linked-account read as "none linked"', async () => {
		auth.listSsoLinks.mockRejectedValue(new Error('HTTP 500'));

		rendered = await renderComponent(SecurityPage);

		expect(rendered.container.textContent).not.toContain(
			'No external accounts are linked.',
		);
		expect(statusBadge(2)).toBe('Unknown');
		expect(warn).toHaveBeenCalled();
	});

	it('keeps the sections that did answer when one read is refused', async () => {
		mfa.backup.mockRejectedValue(new Error('HTTP 500'));
		passkeys.list.mockResolvedValue([
			{ id: 4, name: 'YubiKey', created_at: '2026-08-15T12:00:00Z', last_used_at: null },
		]);
		auth.listSsoLinks.mockResolvedValue([
			{
				slug: 'corp',
				name: 'Corporate SSO',
				provider_username: 'alice@corp',
				email: 'alice@example.com',
				linked_at: '2026-08-15T12:00:00Z',
				provider_enabled: true,
			},
		]);

		rendered = await renderComponent(SecurityPage);

		expect(statusBadge(0)).toBe('Unknown');
		expect(rendered.container.textContent).toContain('YubiKey');
		expect(rendered.container.textContent).toContain('Corporate SSO');
	});
});
