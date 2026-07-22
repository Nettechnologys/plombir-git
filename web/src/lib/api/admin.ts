import { request, qs, type PaginatedResponse } from './_base.svelte';

export interface AdminUser {
  id: number;
  username: string;
  email: string;
  display_name: string | null;
  avatar_url: string | null;
  bio: string | null;
  is_admin: boolean;
  is_active: boolean;
  auth_provider: string;
  last_login_at: string | null;
  login_attempts: number;
  locked_until: string | null;
  created_at: string;
}

export interface AdminOrg {
  id: number;
  name: string;
  display_name: string | null;
  description: string | null;
  owner_id: number;
  visibility: string;
  created_at: string;
  updated_at: string;
}

export interface UpdateUserData {
  display_name?: string;
  bio?: string;
  is_admin?: boolean;
  is_active?: boolean;
}

export interface AuditLogEntry {
  id: number;
  user_id: number | null;
  username: string | null;
  action: string;
  resource_type: string | null;
  resource_id: number | null;
  resource_name: string | null;
  ip_address: string | null;
  details: string | null;
  created_at: string;
}

export interface AuditLogResponse {
  total: number;
  page: number;
  per_page: number;
  logs: AuditLogEntry[];
}

export interface AuditLogQuery {
  page?: number;
  per_page?: number;
  user_id?: number;
  action?: string;
  resource_type?: string;
  start_time?: string;
  end_time?: string;
}

export interface LoginAttemptEntry {
  id: number;
  user_id: number | null;
  username: string;
  auth_provider: string;
  ip_address: string | null;
  user_agent: string | null;
  success: boolean;
  failure_reason: string | null;
  created_at: string;
}

export interface LoginAttemptResponse {
  total: number;
  page: number;
  per_page: number;
  attempts: LoginAttemptEntry[];
}

export interface LoginAttemptQuery {
  page?: number;
  per_page?: number;
  username?: string;
  auth_provider?: string;
  success?: boolean;
  start_time?: string;
  end_time?: string;
}

export interface AdminSettings {
  maintenance_mode: boolean;
  banner_message: string | null;
  banner_type: 'info' | 'warning' | 'error';
}

export interface AdminSsoProvider {
  id: number;
  name: string;
  slug: string;
  provider_type: string;
  client_id: string | null;
  discovery_url: string | null;
  scopes: string | null;
  ldap_host: string | null;
  ldap_port: number | null;
  ldap_bind_dn: string | null;
  ldap_base_dn: string | null;
  ldap_user_filter: string | null;
  enabled: boolean;
  icon_url: string | null;
  created_at: string;
  updated_at: string;
}

export interface SsoProviderPayload {
  name: string;
  slug: string;
  provider_type?: string;
  client_id?: string;
  client_secret?: string;
  discovery_url?: string;
  scopes?: string;
  ldap_host?: string;
  ldap_port?: number;
  ldap_bind_dn?: string;
  ldap_bind_password?: string;
  ldap_base_dn?: string;
  ldap_user_filter?: string;
  enabled?: boolean;
  icon_url?: string;
}

export const admin = {
  listUsers: (page?: number, perPage?: number) =>
    request<PaginatedResponse<AdminUser>>(`/admin/users${qs({ page, per_page: perPage })}`),
  getUser: (id: number) =>
    request<AdminUser>(`/admin/users/${id}`),
  updateUser: (id: number, data: UpdateUserData) =>
    request<AdminUser>(`/admin/users/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  unlockUser: (id: number) =>
    request<AdminUser>(`/admin/users/${id}/unlock`, { method: 'POST' }),
  deleteUser: (id: number) =>
    request<{ deleted: boolean }>(`/admin/users/${id}`, { method: 'DELETE' }),
  listOrgs: (page?: number, perPage?: number) =>
    request<PaginatedResponse<AdminOrg>>(`/admin/orgs${qs({ page, per_page: perPage })}`),
  getOrg: (name: string) =>
    request<AdminOrg>(`/admin/orgs/${name}`),
  deleteOrg: (name: string) =>
    request<{ deleted: boolean }>(`/admin/orgs/${name}`, { method: 'DELETE' }),
  listAuditLogs: (query?: AuditLogQuery) =>
    request<AuditLogResponse>(`/admin/audit/logs${qs({
      page: query?.page,
      per_page: query?.per_page,
      user_id: query?.user_id,
      action: query?.action,
      resource_type: query?.resource_type,
      start_time: query?.start_time,
      end_time: query?.end_time,
    })}`),
  getAuditLog: (id: number) =>
    request<AuditLogEntry>(`/admin/audit/logs/${id}`),
  listLoginAttempts: (query?: LoginAttemptQuery) =>
    request<LoginAttemptResponse>(`/admin/login-attempts${qs({
      page: query?.page,
      per_page: query?.per_page,
      username: query?.username,
      auth_provider: query?.auth_provider,
      success: query?.success,
      start_time: query?.start_time,
      end_time: query?.end_time,
    })}`),
  getSettings: () =>
    request<AdminSettings>('/admin/settings'),
  updateSettings: (payload: Partial<AdminSettings>) =>
    request<AdminSettings>('/admin/settings', {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  listSsoProviders: () =>
    request<AdminSsoProvider[]>('/admin/sso/providers'),
  createSsoProvider: (payload: SsoProviderPayload) =>
    request<AdminSsoProvider>('/admin/sso/providers', {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  updateSsoProvider: (id: number, payload: SsoProviderPayload) =>
    request<AdminSsoProvider>(`/admin/sso/providers/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  deleteSsoProvider: (id: number) =>
    request<{ deleted: boolean }>(`/admin/sso/providers/${id}`, { method: 'DELETE' }),
  testSsoProvider: (id: number) =>
    request<{ ok: boolean; message: string }>(`/admin/sso/providers/${id}/test`, {
      method: 'POST',
    }),
};
