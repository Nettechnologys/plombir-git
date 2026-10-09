import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => ({ id: 1, username: 'root' }),
	isAdmin: () => true,
	isAuthReady: () => true,
	isLoggedIn: () => true,
	fetchUser: vi.fn(async () => undefined),
	forgetDeletedAccount: vi.fn(),
	adoptConfirmedSession: vi.fn(async () => undefined),
}));

import ProfilePage from '../../routes/settings/profile/+page.svelte';
import AdminUsersPage from '../../routes/admin/users/+page.svelte';
import VerifyEmailPage from '../../routes/verify-email/+page.svelte';
import { adoptConfirmedSession, forgetDeletedAccount } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { admin, auth, instance, resetTestClient } from '../test/client';
import {
	answerConfirm,
	button,
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

const me = {
	id: 7,
	username: 'alice',
	email: 'alice@example.com',
	is_admin: false,
	display_name: 'Alice',
	avatar_url: null,
	bio: null,
	auth_provider: 'local',
};

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	auth.me.mockResolvedValue(me);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
	window.history.replaceState({}, '', '/');
});

// card_ca894e30ac80: a signed-in account changes its own profile, picture,
// password and address, and deletes itself — every one of those through the
// settings page, not a database edit.
describe('profile and account settings', () => {
	async function openProfile() {
		setTestPage('/settings/profile', {});
		rendered = await renderComponent(ProfilePage);
		await settle();
		expect(auth.me).toHaveBeenCalled();
		return rendered.container;
	}

	it('saves the profile, clearing what was left blank', async () => {
		auth.updateProfile.mockResolvedValue({ ...me, display_name: 'Alice A.' });
		const page = await openProfile();
		await input(element<HTMLInputElement>(page, '.profile-form input'), ' Alice A. ');
		await submit(element<HTMLFormElement>(page, 'form.profile-form'));
		expect(auth.updateProfile).toHaveBeenCalledWith({ display_name: 'Alice A.', bio: null });
		expect(page.textContent).toContain('Profile saved.');
	});

	it('changes the password with the current one', async () => {
		auth.changePassword.mockResolvedValue({ token: 't', user_id: 7, username: 'alice' });
		const page = await openProfile();
		const fields = page.querySelectorAll<HTMLInputElement>('.password-form input');
		await input(fields[0], 'Old-pass1');
		await input(fields[1], 'New-pass22');
		await input(fields[2], 'New-pass22');
		await submit(element<HTMLFormElement>(page, 'form.password-form'));
		expect(auth.changePassword).toHaveBeenCalledWith('Old-pass1', 'New-pass22');
		expect(page.textContent).toContain('Every other session was signed out');
	});

	it('does not send two new passwords that differ', async () => {
		const page = await openProfile();
		const fields = page.querySelectorAll<HTMLInputElement>('.password-form input');
		await input(fields[0], 'Old-pass1');
		await input(fields[1], 'New-pass22');
		await input(fields[2], 'New-pass23');
		await submit(element<HTMLFormElement>(page, 'form.password-form'));
		expect(auth.changePassword).not.toHaveBeenCalled();
		expect(page.textContent).toContain('The two new passwords differ.');
	});

	it('asks for a confirmation link to the new address', async () => {
		auth.requestEmailChange.mockResolvedValue({
			status: 'confirmation_sent',
			message: 'Follow the link we mailed to the new address to finish the change.',
		});
		const page = await openProfile();
		const fields = page.querySelectorAll<HTMLInputElement>('.email-form input');
		await input(fields[0], ' new@example.com ');
		await input(fields[1], 'Old-pass1');
		await submit(element<HTMLFormElement>(page, 'form.email-form'));
		expect(auth.requestEmailChange).toHaveBeenCalledWith('new@example.com', 'Old-pass1');
		expect(page.textContent).toContain('Follow the link we mailed');
	});

	// card_2296f052332b: an address typed into an open registration is
	// confirmed from here, and an instance without mail says it cannot.
	it('offers to confirm an unconfirmed address when the instance can mail', async () => {
		instance.get.mockResolvedValue({ email_confirmation: true });
		auth.requestEmailVerification.mockResolvedValue({
			status: 'confirmation_sent',
			message: 'Follow the link we mailed to your address to confirm it.',
		});
		const page = await openProfile();
		expect(page.textContent).toContain('Not confirmed yet.');
		await click(element(page, '.confirm-current-email'));
		expect(auth.requestEmailVerification).toHaveBeenCalledOnce();
		expect(page.textContent).toContain('Follow the link we mailed to your address');
	});

	it('says an instance without mail confirms no address', async () => {
		instance.get.mockResolvedValue({ email_confirmation: false });
		const page = await openProfile();
		expect(page.querySelector('.confirm-current-email')).toBeNull();
		expect(page.textContent).toContain('this instance cannot send mail');
	});

	it('shows a confirmed address as confirmed', async () => {
		instance.get.mockResolvedValue({ email_confirmation: true });
		auth.me.mockResolvedValue({ ...me, email_verified_at: '2026-10-09T12:00:00Z' });
		const page = await openProfile();
		expect(page.querySelector('.confirm-current-email')).toBeNull();
		expect(element(page, '.email-status.verified').textContent).toContain('Confirmed');
	});

	it('uploads and removes a picture', async () => {
		auth.uploadAvatar.mockResolvedValue({ avatar_url: '/api/v1/avatars/alice?v=abc' });
		auth.deleteAvatar.mockResolvedValue(undefined);
		const page = await openProfile();
		const file = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'me.png', { type: 'image/png' });
		const picker = element<HTMLInputElement>(page, '.avatar-input');
		Object.defineProperty(picker, 'files', { value: [file] });
		picker.dispatchEvent(new Event('change', { bubbles: true }));
		await settle();
		expect(auth.uploadAvatar).toHaveBeenCalledWith(file);
		expect(element<HTMLImageElement>(page, 'img.avatar').getAttribute('src')).toBe(
			'/api/v1/avatars/alice?v=abc',
		);
		await click(element(page, '.remove-avatar'));
		expect(auth.deleteAvatar).toHaveBeenCalledOnce();
		expect(page.querySelector('img.avatar')).toBeNull();
	});

	it('deletes the account with the password, and forgets the session', async () => {
		auth.deleteAccount.mockResolvedValue({ deleted: true });
		const page = await openProfile();
		await input(element<HTMLInputElement>(page, '.delete-form input'), 'Old-pass1');
		await submit(element<HTMLFormElement>(page, 'form.delete-form'));
		expect(auth.deleteAccount).not.toHaveBeenCalled();
		expect(await answerConfirm()).toContain('every repository it owns');
		expect(auth.deleteAccount).toHaveBeenCalledWith({ password: 'Old-pass1' });
		expect(forgetDeletedAccount).toHaveBeenCalled();
	});

	it('confirms an identity-provider account by its username instead', async () => {
		auth.me.mockResolvedValue({ ...me, auth_provider: 'ldap' });
		auth.deleteAccount.mockResolvedValue({ deleted: true });
		const page = await openProfile();
		expect(page.querySelector('.password-form')).toBeNull();
		await input(element<HTMLInputElement>(page, '.delete-form input'), 'alice');
		await submit(element<HTMLFormElement>(page, 'form.delete-form'));
		await answerConfirm();
		expect(auth.deleteAccount).toHaveBeenCalledWith({ confirm_username: 'alice' });
	});
});

// card_9f18b657580b: with registration closed and no identity provider, the
// administrator is the one way in — create the account, hand over a password.
describe('admin user provisioning', () => {
	const adminUser = (id: number, username: string) => ({
		id,
		username,
		email: `${username}@example.com`,
		display_name: null,
		bio: null,
		is_admin: false,
		is_active: true,
		auth_provider: 'local',
		login_attempts: 0,
		locked_until: null,
		last_login_at: null,
		created_at: '2026-10-08T00:00:00Z',
	});

	beforeEach(() => {
		setTestPage('/admin/users', {});
		admin.listUsers.mockResolvedValue({
			data: [adminUser(1, 'root'), adminUser(2, 'bob')],
			pagination: { total: 2, total_pages: 1 },
		});
	});

	it('creates a user and shows its temporary password once', async () => {
		admin.createUser.mockResolvedValue({
			user: adminUser(3, 'carol'),
			temporary_password: 'abcde-FGHJK-23456-mnpqr',
		});
		rendered = await renderComponent(AdminUsersPage);
		await settle();
		await click(element(rendered.container, '.open-create-user'));
		const fields = rendered.container.querySelectorAll<HTMLInputElement>('.create-user-form input');
		await input(fields[0], ' carol ');
		await input(fields[1], 'carol@example.com');
		await submit(element<HTMLFormElement>(rendered.container, 'form.create-user-form'));
		expect(admin.createUser).toHaveBeenCalledWith({
			username: 'carol',
			email: 'carol@example.com',
			display_name: undefined,
			is_admin: false,
		});
		expect(element(rendered.container, '.temporary-password').textContent).toBe(
			'abcde-FGHJK-23456-mnpqr',
		);
		await click(button(rendered.container, 'Done'));
		expect(rendered.container.querySelector('.temporary-password')).toBeNull();
	});

	it('resets another user’s password, not the admin’s own', async () => {
		admin.resetUserPassword.mockResolvedValue({ temporary_password: 'zyxwv-TSRQP-98765-kjhgf' });
		rendered = await renderComponent(AdminUsersPage);
		await settle();
		const resets = rendered.container.querySelectorAll('.reset-password');
		expect(resets).toHaveLength(1);
		await click(resets[0]);
		expect(await answerConfirm()).toContain('bob');
		expect(admin.resetUserPassword).toHaveBeenCalledWith(2);
		expect(element(rendered.container, '.temporary-password').textContent).toBe(
			'zyxwv-TSRQP-98765-kjhgf',
		);
	});
});

// card_45f98ab2fe1a / card_ca894e30ac80: the mailed link spends its token only
// on the button, never on opening the page.
describe('confirming an address', () => {
	it('signs a confirmed registration in', async () => {
		window.history.replaceState({}, '', '/verify-email?token=abc123');
		auth.confirmEmail.mockResolvedValue({ token: 'session', user_id: 9, username: 'dave' });
		rendered = await renderComponent(VerifyEmailPage);
		expect(auth.confirmEmail).not.toHaveBeenCalled();
		await click(element(rendered.container, '.confirm-email'));
		expect(auth.confirmEmail).toHaveBeenCalledWith('abc123');
		expect(adoptConfirmedSession).toHaveBeenCalledWith('session');
	});

	it('reports a confirmed address', async () => {
		window.history.replaceState({}, '', '/verify-email?token=ghi789');
		auth.confirmEmail.mockResolvedValue({ email: 'alice@example.com', email_verified: true });
		rendered = await renderComponent(VerifyEmailPage);
		await click(element(rendered.container, '.confirm-email'));
		expect(rendered.container.textContent).toContain('This address is confirmed');
		expect(rendered.container.textContent).not.toContain('now uses this address');
	});

	it('reports a moved address', async () => {
		window.history.replaceState({}, '', '/verify-email?token=def456');
		auth.confirmEmail.mockResolvedValue({ email: 'new@example.com' });
		rendered = await renderComponent(VerifyEmailPage);
		await click(element(rendered.container, '.confirm-email'));
		expect(rendered.container.textContent).toContain('new@example.com');
	});
});
