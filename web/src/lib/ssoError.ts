/**
 * A refused SSO round trip, as the callback hands it back to the page it
 * started from (card_0d7c54cae647).
 *
 * The callback is a top-level navigation from the provider's site, so it never
 * answers with JSON: it redirects to `/login` (or `/settings/security` for a
 * link) with `sso_error=<code>&provider=<slug>`. The codes are
 * `SsoFailure::code` in `crates/rg-http/src/api/sso.rs`; this file turns them
 * into the sentence the person reads.
 */

type Translate = (key: string, params?: Record<string, string | number>) => string;

export const SSO_ERROR_CODES = [
  'state_invalid',
  'provider_unavailable',
  'code_rejected',
  'profile_no_id',
  'profile_no_email',
  'profile_bad_email',
  'profile_unverified_email',
  'profile_no_username',
  'link_required',
  'auto_provision_disabled',
  'email_domain_not_allowed',
  'email_not_verified',
  'account_disabled',
  'account_locked',
  'linked_elsewhere',
  'provider_already_linked',
  'session_ended',
  'retry',
  'server_error',
  'failed',
] as const;

export type SsoErrorCode = (typeof SSO_ERROR_CODES)[number];

export interface SsoError {
  code: SsoErrorCode;
  /** The provider's slug, as the callback's URL named it. */
  provider: string;
}

/**
 * The refusal a callback redirect carries, or `null` when the query has none.
 * A code this build does not know reads as `failed` rather than as nothing: a
 * newer server must still not leave the person with a silent login page.
 */
export function readSsoError(search: string): SsoError | null {
  const params = new URLSearchParams(search);
  const raw = params.get('sso_error');
  if (raw === null) return null;
  const code = (SSO_ERROR_CODES as readonly string[]).includes(raw) ? (raw as SsoErrorCode) : 'failed';
  return { code, provider: params.get('provider') ?? '' };
}

/** What to tell the person; `provider` is the provider's display name. */
export function ssoErrorMessage(t: Translate, code: SsoErrorCode, provider: string): string {
  const p = { provider };
  switch (code) {
    case 'state_invalid':
      return t('auth.sso_error.state_invalid', p);
    case 'provider_unavailable':
      return t('auth.sso_error.provider_unavailable', p);
    case 'code_rejected':
      return t('auth.sso_error.code_rejected', p);
    case 'profile_no_id':
      return t('auth.sso_error.profile_no_id', p);
    case 'profile_no_email':
      return t('auth.sso_error.profile_no_email', p);
    case 'profile_bad_email':
      return t('auth.sso_error.profile_bad_email', p);
    case 'profile_unverified_email':
      return t('auth.sso_error.profile_unverified_email', p);
    case 'profile_no_username':
      return t('auth.sso_error.profile_no_username', p);
    case 'link_required':
      return t('auth.sso_error.link_required', p);
    case 'auto_provision_disabled':
      return t('auth.sso_error.auto_provision_disabled', p);
    case 'email_domain_not_allowed':
      return t('auth.sso_error.email_domain_not_allowed', p);
    case 'email_not_verified':
      return t('auth.sso_error.email_not_verified', p);
    case 'account_disabled':
      return t('auth.sso_error.account_disabled', p);
    case 'account_locked':
      return t('auth.sso_error.account_locked', p);
    case 'linked_elsewhere':
      return t('auth.sso_error.linked_elsewhere', p);
    case 'provider_already_linked':
      return t('auth.sso_error.provider_already_linked', p);
    case 'session_ended':
      return t('auth.sso_error.session_ended', p);
    case 'retry':
      return t('auth.sso_error.retry', p);
    case 'server_error':
      return t('auth.sso_error.server_error', p);
    case 'failed':
      return t('auth.sso_error.failed', p);
  }
}
