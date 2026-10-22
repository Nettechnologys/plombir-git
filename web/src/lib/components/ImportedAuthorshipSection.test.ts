import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('../api/_base.svelte', () => base);

import SecurityPage from '../../routes/settings/security/+page.svelte';
import { auth } from '../api/auth';
import { fetchUser, logout } from '../stores/auth.svelte';
import { auth as routeAuth, mfa, passkeys, resetTestClient } from '../test/client';
import { answerConfirm, button, click, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const row = {
	repo_id: 12,
	repository: 'acme/moved',
	service: 'github',
	login: 'alice-on-github',
	issues: 3,
	comments: 5,
	pull_requests: 1,
	reviews: 2,
	releases: 0,
};

beforeEach(async () => {
	vi.clearAllMocks();
	resetTestClient();
	routeAuth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	routeAuth.listSsoLinks.mockResolvedValue([]);
	mfa.backup.mockResolvedValue({ total: 0, unused: 0 });
	passkeys.list.mockResolvedValue([]);
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('imported authorship transport', () => {
	it('lists and claims through the account endpoints', () => {
		auth.listImportedAuthorship();
		expect(base.request).toHaveBeenCalledWith('/users/me/imported-authorship');
		auth.claimImportedAuthorship([12]);
		expect(base.request).toHaveBeenCalledWith('/users/me/imported-authorship/claim', {
			method: 'POST',
			body: JSON.stringify({ repositories: [12] }),
		});
	});
});

// card_2a006cf365f6: nothing moves until the person sees what would and asks.
describe('imported history on the security page', () => {
	it('shows what a linked sign-in wrote and takes nothing back on a declined confirmation', async () => {
		routeAuth.listImportedAuthorship.mockResolvedValue([row]);
		rendered = await renderComponent(SecurityPage);
		const text = rendered.container.textContent ?? '';
		expect(text).toContain('acme/moved');
		expect(text).toContain('@alice-on-github');

		await click(button(rendered.container, 'Take back'));
		await answerConfirm(false);
		expect(routeAuth.claimImportedAuthorship).not.toHaveBeenCalled();
		expect(rendered.container.textContent).toContain('acme/moved');
	});

	it('moves one repository after the person confirms it', async () => {
		routeAuth.listImportedAuthorship.mockResolvedValue([row]);
		routeAuth.claimImportedAuthorship.mockResolvedValue({
			issues: 3,
			comments: 5,
			pull_requests: 1,
			reviews: 2,
			releases: 0,
		});
		rendered = await renderComponent(SecurityPage);

		await click(button(rendered.container, 'Take back'));
		await answerConfirm(true);

		expect(routeAuth.claimImportedAuthorship).toHaveBeenCalledWith([12]);
		expect(rendered.container.textContent).toContain('11 items in acme/moved are now yours.');
	});

	it('renders no section when there is nothing to take back', async () => {
		routeAuth.listImportedAuthorship.mockResolvedValue([]);
		rendered = await renderComponent(SecurityPage);
		expect(rendered.container.textContent).not.toContain('Imported history');
	});
});
