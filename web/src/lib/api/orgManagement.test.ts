import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import OrganizationPage from '../../routes/orgs/[name]/+page.svelte';
import { buildOrganizationUpdatePayload } from './orgManagement';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { ApiError, auth, orgs, repos, resetTestClient } from '../test/client';
import {
	button,
	click,
	element,
	input,
	renderComponent,
	submit,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;

const organization = {
	id: 4,
	name: 'acme',
	display_name: 'Acme',
	description: 'Old description',
	visibility: 'public',
	owner_id: 1,
	created_at: '2026-08-15T12:00:00Z',
};

const members = [
	{ id: 1, user_id: 1, username: 'alice', role: 'owner' },
	{ id: 2, user_id: 2, username: 'bob', role: 'member' },
];

const teams = [
	{ id: 5, name: 'core', description: null, permission: 'write' },
];

beforeEach(async () => {
	resetTestClient();
	setTestPage('/orgs/acme', { name: 'acme' });
	vi.stubGlobal('confirm', vi.fn(() => true));
	auth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	orgs.get.mockResolvedValue(organization);
	orgs.listMembers.mockResolvedValue(members);
	orgs.listTeams.mockResolvedValue(teams);
	orgs.listTeamMembers.mockResolvedValue([
		{ id: 8, user_id: 2, username: 'bob', role: 'member' },
	]);
	orgs.update.mockResolvedValue({
		...organization,
		display_name: 'Acme Inc',
		description: '',
		visibility: 'private',
	});
	repos.list.mockResolvedValue({ data: [] });
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

describe('organization management', () => {
  it('normalizes editable organization fields while preserving an explicit clear', () => {
    expect(
      buildOrganizationUpdatePayload({
        displayName: ' Acme ',
        description: '  ',
        visibility: 'private',
      }),
    ).toEqual({
      display_name: 'Acme',
      description: '',
      visibility: 'private',
    });
  });

	it('submits the rendered organization editor through the canonical builder', async () => {
		rendered = await renderComponent(OrganizationPage);
		await click(element(rendered.container, '.header-actions .btn-secondary'));
		const editForm = element<HTMLFormElement>(rendered.container, '.edit-organization');
		const fields = editForm.querySelectorAll<HTMLInputElement | HTMLTextAreaElement>('input, textarea');
		await input(fields[0], ' Acme Inc ');
		await input(fields[1], '   ');
		const visibility = element<HTMLSelectElement>(editForm, 'select');
		visibility.value = 'private';
		visibility.dispatchEvent(new Event('change', { bubbles: true }));
		await submit(editForm);

		expect(orgs.update).toHaveBeenCalledWith('acme', {
			display_name: 'Acme Inc',
			description: '',
			visibility: 'private',
		});
	});

	it('renders working organization and team membership controls', async () => {
		rendered = await renderComponent(OrganizationPage);
		const sections = rendered.container.querySelectorAll<HTMLElement>('.grid > .section');
		const teamSection = sections[0];
		const memberSection = sections[1];

		const memberForm = element<HTMLFormElement>(memberSection, '.member-form');
		await input(element(memberForm, 'input'), ' carol ');
		await submit(memberForm);
		expect(orgs.addMember).toHaveBeenCalledWith('acme', 'carol', 'member');

		const bobRow = Array.from(memberSection.querySelectorAll('.item')).find((row) =>
			row.textContent?.includes('bob'),
		)!;
		await click(element(bobRow, '.btn-danger'));
		expect(orgs.removeMember).toHaveBeenCalledWith('acme', 2);

		await click(button(teamSection, 'View members'));
		expect(orgs.listTeamMembers).toHaveBeenCalledWith('acme', 5);
		const teamMemberForm = element<HTMLFormElement>(teamSection, '.team-members .member-form');
		await input(element(teamMemberForm, 'input'), ' dave ');
		await submit(teamMemberForm);
		expect(orgs.addTeamMember).toHaveBeenCalledWith('acme', 5, 'dave', 'member');

		await click(element(teamSection, '.team-members .item .btn-danger'));
		expect(orgs.removeTeamMember).toHaveBeenCalledWith('acme', 5, 2);

		await click(element(teamSection, '.managed-item > .item-row .btn-danger'));
		expect(orgs.deleteTeam).toHaveBeenCalledWith('acme', 5);

		await click(element(rendered.container, '.header-actions .btn-danger'));
		expect(orgs.delete).toHaveBeenCalledWith('acme');
	});

	// Security audit #5: ownership is a membership role. The page offers the
	// owner-only actions to an `owner`-role member, hands the organization over
	// through the API client, and reads the member list back for the new role.
	it('lets an owner-role member transfer ownership to another member', async () => {
		orgs.transferOwnership.mockResolvedValue({ ...organization, owner_id: 2 });
		rendered = await renderComponent(OrganizationPage);

		await click(button(rendered.container, 'Transfer ownership'));
		const form = element<HTMLFormElement>(rendered.container, '.transfer-ownership');
		const target = element<HTMLSelectElement>(form, 'select');
		expect(Array.from(target.options).map((option) => option.value)).toEqual(['bob']);
		await submit(form);

		expect(confirm).toHaveBeenCalled();
		expect(orgs.transferOwnership).toHaveBeenCalledWith('acme', 'bob');
		expect(orgs.listMembers).toHaveBeenCalledTimes(2);
		expect(rendered.container.querySelector('.transfer-ownership')).toBeNull();
	});

	// The removed creator keeps `org.owner_id` but no membership row, so the
	// page must not key any control on that column.
	it('offers no management controls to a non-member who is still owner_id', async () => {
		orgs.listMembers.mockResolvedValue([{ id: 2, user_id: 2, username: 'bob', role: 'owner' }]);
		rendered = await renderComponent(OrganizationPage);

		expect(rendered.container.querySelector('.header-actions')).toBeNull();
		expect(rendered.container.querySelector('.repositories-section .create-form')).toBeNull();
	});

	// An admin runs the organization; only an owner disposes of it.
	it('hides delete and transfer from an admin-role member', async () => {
		orgs.listMembers.mockResolvedValue([
			{ id: 1, user_id: 1, username: 'alice', role: 'admin' },
			{ id: 2, user_id: 2, username: 'bob', role: 'owner' },
		]);
		rendered = await renderComponent(OrganizationPage);

		expect(element(rendered.container, '.header-actions')).toBeTruthy();
		expect(rendered.container.querySelector('.header-actions .btn-danger')).toBeNull();
		expect(
			Array.from(rendered.container.querySelectorAll('.header-actions button')).map((b) => b.textContent?.trim()),
		).toEqual(['Edit']);
	});

	it('disables removing the last owner and surfaces the server refusal otherwise', async () => {
		rendered = await renderComponent(OrganizationPage);
		const memberSection = rendered.container.querySelectorAll<HTMLElement>('.grid > .section')[1];
		const aliceRow = Array.from(memberSection.querySelectorAll('.item')).find((row) =>
			row.textContent?.includes('alice'),
		)!;
		const removeAlice = element<HTMLButtonElement>(aliceRow, '.btn-danger');
		expect(removeAlice.disabled).toBe(true);
		expect(removeAlice.title).toContain('last owner');

		orgs.removeMember.mockRejectedValue(
			new ApiError("this member is the organization's last owner; make another member an owner (transfer ownership) before removing them", 409),
		);
		const bobRow = Array.from(memberSection.querySelectorAll('.item')).find((row) =>
			row.textContent?.includes('bob'),
		)!;
		await click(element(bobRow, '.btn-danger'));
		expect(element(rendered.container, '.page-error').textContent).toContain("last owner");
	});
});
