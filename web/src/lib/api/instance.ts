import { request } from './_base.svelte';

/// The public announcement of the instance — `GET /api/v1/instance`.
///
/// The same three values the admin settings page writes, minus the gate. They
/// used to be readable only through `/admin/settings`, so a banner announcing
/// maintenance was visible to the one admin who typed it and to nobody else.
export interface InstanceInfo {
  maintenance_mode: boolean;
  banner_message: string | null;
  banner_type: 'info' | 'warning' | 'error';
}

export const instance = {
  get: () => request<InstanceInfo>('/instance'),
};
