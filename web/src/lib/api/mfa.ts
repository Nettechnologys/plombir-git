import { request } from './_base.svelte';

export interface MfaSetupResponse {
  secret: string;
  otpauth_url: string;
  qr_svg: string;
}

export interface MfaEnableResponse {
  enabled: boolean;
  backup_codes: string[];
}

export interface MfaRegenerateBackupResponse {
  backup_codes: string[];
}

export interface MfaBackupStatus {
  mfa_enabled: boolean;
  replacement_count: number;
  /** Includes used codes retained as history, even after MFA is disabled. */
  total: number;
  unused: number;
  codes: { used: boolean; used_at?: string | null; created_at: string }[];
}

export const mfa = {
  setup: () =>
    request<MfaSetupResponse>('/users/mfa/setup', { method: 'POST' }),
  // First enrolment proves a local password. Changes to an existing factor
  // require sudo, which `request` turns into the shared confirmation prompt.
  enable: (code: string, password?: string) =>
    request<MfaEnableResponse>('/users/mfa/enable', {
      method: 'POST',
      body: JSON.stringify({ code, password }),
    }),
  backup: () =>
    request<MfaBackupStatus>('/users/mfa/backup'),
  regenerateBackup: (password?: string) =>
    request<MfaRegenerateBackupResponse>('/users/mfa/backup/regenerate', {
      method: 'POST',
      body: JSON.stringify({ password }),
    }),
  disable: (password?: string) =>
    request<void>('/users/mfa/disable', {
      method: 'POST',
      body: JSON.stringify({ password }),
    }),
};
