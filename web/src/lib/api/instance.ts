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
  /**
   * Whether this instance signs and verifies release-asset provenance.
   *
   * A capability rather than a setting. Both attestation endpoints answer `404`
   * when the feature is off and when an asset simply has no attestation, and
   * those are opposite facts for a reader — "this forge does not do provenance"
   * versus "this file was never signed" (card_5e52392a0274).
   */
  attestation_enabled: boolean;
}

export const instance = {
  get: () => request<InstanceInfo>('/instance'),
};
