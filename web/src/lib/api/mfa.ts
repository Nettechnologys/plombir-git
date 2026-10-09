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
  total: number;
  unused: number;
  codes: { used: boolean; used_at?: string | null; created_at: string }[];
}

export const mfa = {
  setup: () =>
    request<MfaSetupResponse>('/users/mfa/setup', { method: 'POST' }),
  // The password is asked for at the moment the factor is armed, first
  // enrolment included: a stolen session must not be enough to put a second
  // factor only the thief can pass on the account.
  enable: (code: string, password: string) =>
    request<MfaEnableResponse>('/users/mfa/enable', {
      method: 'POST',
      body: JSON.stringify({ code, password }),
    }),
  backup: () =>
    request<MfaBackupStatus>('/users/mfa/backup'),
  regenerateBackup: (password: string) =>
    request<MfaRegenerateBackupResponse>('/users/mfa/backup/regenerate', {
      method: 'POST',
      body: JSON.stringify({ password }),
    }),
  disable: (password: string) =>
    request<void>('/users/mfa/disable', {
      method: 'POST',
      body: JSON.stringify({ password }),
    }),
};
