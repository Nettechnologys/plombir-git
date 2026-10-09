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
  /**
   * Where to read the source of the build answering this request:
   * `<[server].source_url>/tree/<commit>`, or the repository itself when the
   * build did not record its commit. The AGPL §13 offer — it follows the
   * operator's setting, so a fork's users are sent to the fork.
   */
  source_url: string;
  /** The commit this binary was built from, or `null` when the build was not told. */
  source_commit: string | null;
  /**
   * Whether the instance is still waiting for its first account. That one
   * registration needs the one-time setup token from the server's startup
   * log, so the register page shows a field for it while this is true.
   */
  setup_required: boolean;
  /**
   * Whether self-service sign-up would be accepted now — `false` on a closed
   * instance once its first account exists. Optional: an older server does not
   * send it, and then the sign-up links stay.
   */
  registration_open?: boolean;
}

export const instance = {
  get: () => request<InstanceInfo>('/instance'),
};
