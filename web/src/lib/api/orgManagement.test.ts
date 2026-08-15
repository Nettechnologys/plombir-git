import { describe, expect, it } from 'vitest';

import organizationPageSource from '../../routes/orgs/[name]/+page.svelte?raw';
import { buildOrganizationUpdatePayload, parseUserId } from './orgManagement';

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

  it('accepts only positive safe integer user ids', () => {
    expect(parseUserId(' 42 ')).toBe(42);
    expect(parseUserId('0')).toBeNull();
    expect(parseUserId('-1')).toBeNull();
    expect(parseUserId('1.5')).toBeNull();
    expect(parseUserId('not-a-user')).toBeNull();
  });

  it('wires every organization and team management API into the production page', () => {
    for (const call of [
      'orgs.update(',
      'orgs.delete(',
      'orgs.addMember(',
      'orgs.removeMember(',
      'orgs.deleteTeam(',
      'orgs.listTeamMembers(',
      'orgs.addTeamMember(',
      'orgs.removeTeamMember(',
    ]) {
      expect(organizationPageSource.includes(call), call).toBe(true);
    }

    expect(organizationPageSource).toContain('confirm(');
    expect(organizationPageSource).toContain('await refreshMembers()');
    expect(organizationPageSource).toContain('await refreshTeams()');
    expect(organizationPageSource).toContain('await refreshTeamMembers(teamId)');
  });
});
