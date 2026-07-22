import { request } from './_base.svelte';

export const orgs = {
  list: (userId?: number) =>
    request<any[]>(`/orgs${userId ? `?user_id=${userId}` : ''}`),
  get: (name: string) =>
    request<any>(`/orgs/${name}`),
  create: (name: string, displayName?: string, description?: string, visibility?: string) =>
    request<any>('/orgs', {
      method: 'POST',
      body: JSON.stringify({ name, display_name: displayName, description, visibility }),
    }),
  update: (name: string, data: { display_name?: string; description?: string; visibility?: string }) =>
    request<any>(`/orgs/${name}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (name: string) =>
    request<any>(`/orgs/${name}`, { method: 'DELETE' }),
  listMembers: (name: string) =>
    request<any[]>(`/orgs/${name}/members`),
  addMember: (name: string, userId: number, role?: string) =>
    request<any>(`/orgs/${name}/members`, {
      method: 'POST',
      body: JSON.stringify({ user_id: userId, role: role || 'member' }),
    }),
  removeMember: (name: string, userId: number) =>
    request<any>(`/orgs/${name}/members/${userId}`, { method: 'DELETE' }),
  listTeams: (name: string) =>
    request<any[]>(`/orgs/${name}/teams`),
  createTeam: (name: string, teamName: string, description?: string, permission?: string) =>
    request<any>(`/orgs/${name}/teams`, {
      method: 'POST',
      body: JSON.stringify({ name: teamName, description, permission: permission || 'read' }),
    }),
  deleteTeam: (name: string, teamId: number) =>
    request<any>(`/orgs/${name}/teams/${teamId}`, { method: 'DELETE' }),
  listTeamMembers: (name: string, teamId: number) =>
    request<any[]>(`/orgs/${name}/teams/${teamId}/members`),
  addTeamMember: (name: string, teamId: number, userId: number, role?: string) =>
    request<any>(`/orgs/${name}/teams/${teamId}/members`, {
      method: 'POST',
      body: JSON.stringify({ user_id: userId, role: role || 'member' }),
    }),
  removeTeamMember: (name: string, teamId: number, userId: number) =>
    request<any>(`/orgs/${name}/teams/${teamId}/members/${userId}`, { method: 'DELETE' }),
};
