import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import OrganizationPage from '../../routes/orgs/[name]/+page.svelte';
import { buildOrganizationUpdatePayload } from './orgManagement';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, orgs, repos, resetTestClient } from '../test/client';
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
});
