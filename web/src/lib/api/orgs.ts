import { request } from './_base.svelte';

export type OrganizationVisibility = 'public' | 'private';
export type OrganizationMemberRole = 'owner' | 'admin' | 'member';
export type TeamPermission = 'read' | 'write' | 'admin';
export type TeamMemberRole = 'member' | 'maintainer';

export interface Organization {
  id: number;
  name: string;
  display_name: string | null;
  description: string | null;
  owner_id: number;
  visibility: OrganizationVisibility;
  created_at: string;
  updated_at: string;
}

export interface OrganizationMember {
  id: number;
  org_id: number;
  user_id: number;
  role: OrganizationMemberRole;
  created_at: string;
}

export interface OrganizationTeam {
  id: number;
  org_id: number;
  name: string;
  description: string | null;
  permission: TeamPermission;
  created_at: string;
  updated_at: string;
}

export interface TeamMember {
  id: number;
  team_id: number;
  user_id: number;
  role: TeamMemberRole;
  created_at: string;
}

export interface OrganizationUpdatePayload {
  display_name: string;
  description: string;
  visibility: OrganizationVisibility;
}

export const orgs = {
  list: (userId?: number) =>
    request<Organization[]>(`/orgs${userId ? `?user_id=${userId}` : ''}`),
  get: (name: string) =>
    request<Organization>(`/orgs/${name}`),
  create: (name: string, displayName?: string, description?: string, visibility?: OrganizationVisibility) =>
    request<Pick<Organization, 'id' | 'name' | 'display_name' | 'visibility'>>('/orgs', {
      method: 'POST',
      body: JSON.stringify({ name, display_name: displayName, description, visibility }),
    }),
  update: (name: string, data: OrganizationUpdatePayload) =>
    request<Organization>(`/orgs/${name}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (name: string) =>
    request<{ deleted: boolean }>(`/orgs/${name}`, { method: 'DELETE' }),
  listMembers: (name: string) =>
    request<OrganizationMember[]>(`/orgs/${name}/members`),
  addMember: (name: string, userId: number, role?: OrganizationMemberRole) =>
    request<OrganizationMember>(`/orgs/${name}/members`, {
      method: 'POST',
      body: JSON.stringify({ user_id: userId, role: role || 'member' }),
    }),
  removeMember: (name: string, userId: number) =>
    request<{ removed: boolean }>(`/orgs/${name}/members/${userId}`, { method: 'DELETE' }),
  listTeams: (name: string) =>
    request<OrganizationTeam[]>(`/orgs/${name}/teams`),
  createTeam: (name: string, teamName: string, description?: string, permission?: TeamPermission) =>
    request<OrganizationTeam>(`/orgs/${name}/teams`, {
      method: 'POST',
      body: JSON.stringify({ name: teamName, description, permission: permission || 'read' }),
    }),
  deleteTeam: (name: string, teamId: number) =>
    request<{ deleted: boolean }>(`/orgs/${name}/teams/${teamId}`, { method: 'DELETE' }),
  listTeamMembers: (name: string, teamId: number) =>
    request<TeamMember[]>(`/orgs/${name}/teams/${teamId}/members`),
  addTeamMember: (name: string, teamId: number, userId: number, role?: TeamMemberRole) =>
    request<TeamMember>(`/orgs/${name}/teams/${teamId}/members`, {
      method: 'POST',
      body: JSON.stringify({ user_id: userId, role: role || 'member' }),
    }),
  removeTeamMember: (name: string, teamId: number, userId: number) =>
    request<{ removed: boolean }>(`/orgs/${name}/teams/${teamId}/members/${userId}`, { method: 'DELETE' }),
};
