import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({ request: vi.fn() }));
vi.mock('./_base.svelte', () => base);

import OrganizationPage from '../../routes/orgs/[name]/+page.svelte';
import { setTestPage } from '../test/app';
import { orgs as routeOrgs, repos as routeRepos, resetTestClient } from '../test/client';
import { renderComponent, type RenderedComponent } from '../test/render';
import { admin } from './admin';
import { orgs } from './orgs';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
  vi.clearAllMocks();
  resetTestClient();
  base.request.mockImplementation(async (url: string) => url === '/orgs/team%2Fmembers%3F'
    ? { id: 1, name: 'team/members?', display_name: 'Team', description: null, owner_id: 1, visibility: 'public' }
    : []);
  routeOrgs.get.mockImplementation(orgs.get);
  routeOrgs.listMembers.mockImplementation(orgs.listMembers);
  routeOrgs.listTeams.mockImplementation(orgs.listTeams);
  routeRepos.list.mockResolvedValue({ data: [] });
  setTestPage('/orgs/team%2Fmembers%3F', { name: 'team/members?' });
});

afterEach(async () => {
  await rendered?.destroy();
  rendered = undefined;
  document.body.innerHTML = '';
});

describe('organization API path segments', () => {
  it('keeps a decoded organization route parameter in one segment', async () => {
    rendered = await renderComponent(OrganizationPage);
    expect(base.request).toHaveBeenCalledWith('/orgs/team%2Fmembers%3F');
    expect(base.request).toHaveBeenCalledWith('/orgs/team%2Fmembers%3F/members');
    expect(base.request).toHaveBeenCalledWith('/orgs/team%2Fmembers%3F/teams');
    expect(base.request.mock.calls.some(([url]) => url === '/orgs/team/members?')).toBe(false);
  });

  it('encodes administrative organization paths and rejects dot segments', async () => {
    await admin.getOrg('team/members?');
    await admin.deleteOrg('team/members?');
    expect(base.request).toHaveBeenCalledWith('/admin/orgs/team%2Fmembers%3F');
    expect(base.request).toHaveBeenCalledWith('/admin/orgs/team%2Fmembers%3F', { method: 'DELETE' });
    expect(() => orgs.get('..')).toThrow('Invalid URL path segment');
  });
});
