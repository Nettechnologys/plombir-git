import { request, withApiBase } from './_base.svelte';

export interface AuthLoginResponse {
  token: string;
  user_id: number;
  username: string;
  mfa_required?: boolean;
  /**
   * The password was chosen by an administrator: no session was opened, and
   * `setInitialPassword` replaces it before one is.
   */
  password_change_required?: boolean;
}

/**
 * `POST /users/register` either signs the account in at once, or — on an
 * instance whose registration is `verify-email` — answers that a link went
 * to the address and nothing exists yet.
 */
export type RegisterResponse =
  | AuthLoginResponse
  | { status: 'confirmation_sent'; message: string };

/** The signed-in account, as `/users/me` describes it. */
export interface Me {
  id: number;
  username: string;
  email: string;
  is_admin: boolean;
  display_name: string | null;
  avatar_url: string | null;
  bio: string | null;
  /** `local` accounts have a password and an address of their own here. */
  auth_provider: string;
}

/** Following a mailed confirmation link: a new account, or a moved address. */
export type ConfirmEmailResponse = AuthLoginResponse | { email: string };

export interface PublicSsoProvider {
  slug: string;
  name: string;
  provider_type: string;
  icon_url: string | null;
}

/** One external identity linked to the signed-in account. */
export interface SsoLink {
  slug: string;
  name: string;
  provider_username: string;
  email: string;
  linked_at: string;
  /**
   * False when the operator has switched the provider off or removed it. The
   * link still exists and can still be dropped — the backend unlink is written
   * to keep working past that point on purpose.
   */
  provider_enabled: boolean;
}

export const auth = {
  // `setupToken` is the one-time secret the first account on an empty
  // instance has to present (`GET /instance` says when: `setup_required`).
  // Left out of the body entirely when absent, so an ordinary registration
  // looks exactly as it always did.
  register: (username: string, email: string, password: string, setupToken?: string) =>
    request<RegisterResponse>('/users/register', {
      method: 'POST',
      body: JSON.stringify(
        setupToken ? { username, email, password, setup_token: setupToken } : { username, email, password },
      ),
    }),
  login: (username: string, password: string) =>
    request<AuthLoginResponse>('/users/login', {
      method: 'POST',
      body: JSON.stringify({ username, password }),
    }),
  verifyMfa: (username: string, code: string, backup = false) =>
    request<AuthLoginResponse>('/users/mfa/verify', {
      method: 'POST',
      body: JSON.stringify({ username, code, backup }),
    }),
  me: () => request<Me>('/users/me'),
  // Signs every other session out; this one continues with the token in the
  // answer, which the HttpOnly cookie already carries.
  changePassword: (currentPassword: string, newPassword: string) =>
    request<AuthLoginResponse>('/users/me/password', {
      method: 'PUT',
      body: JSON.stringify({ current_password: currentPassword, new_password: newPassword }),
    }),
  // The second half of a login that answered `password_change_required`.
  setInitialPassword: (login: string, password: string, newPassword: string) =>
    request<AuthLoginResponse>('/users/password/initial', {
      method: 'POST',
      body: JSON.stringify({ login, password, new_password: newPassword }),
    }),
  // `null` clears a field; leaving a key out keeps it.
  updateProfile: (data: { display_name?: string | null; bio?: string | null }) =>
    request<Me>('/users/me', {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  requestEmailChange: (email: string, password: string) =>
    request<{ status: string; message: string }>('/users/me/email', {
      method: 'POST',
      body: JSON.stringify({ email, password }),
    }),
  confirmEmail: (token: string) =>
    request<ConfirmEmailResponse>('/users/verify-email', {
      method: 'POST',
      body: JSON.stringify({ token }),
    }),
  // A local account confirms with its password; one that signs in through a
  // provider types its username instead.
  deleteAccount: (confirmation: { password?: string; confirm_username?: string }) =>
    request<{ deleted: boolean }>('/users/me', {
      method: 'DELETE',
      body: JSON.stringify(confirmation),
    }),
  uploadAvatar: (image: Blob) =>
    request<{ avatar_url: string }>('/users/me/avatar', {
      method: 'PUT',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: image,
    }),
  deleteAvatar: () =>
    request<void>('/users/me/avatar', { method: 'DELETE' }),
  forgotPassword: (email: string) =>
    request<{ message: string }>('/users/forgot-password', {
      method: 'POST',
      body: JSON.stringify({ email }),
    }),
  // Answers with `mfa_required` and an empty token for an account that has a
  // second factor: the reset changes the password, the session comes from
  // `verifyMfa`. Same shape as `login`, so the same handling applies.
  resetPassword: (token: string, newPassword: string) =>
    request<AuthLoginResponse>('/users/reset-password', {
      method: 'POST',
      body: JSON.stringify({ token, new_password: newPassword }),
    }),
  logout: () =>
    request<{ logged_out: boolean }>('/users/logout', {
      method: 'POST',
    }),
  listSsoProviders: () =>
    request<PublicSsoProvider[]>('/auth/sso/providers'),
  ssoAuthorizeUrl: (slug: string) =>
    withApiBase(`/auth/sso/${encodeURIComponent(slug)}`),
  listSsoLinks: () => request<SsoLink[]>('/users/me/sso'),
  // Starts linking a provider to the signed-in account. The browser then goes
  // to `authorize_url`; the provider's callback attaches the identity to this
  // account and comes back to `/settings/security?sso_linked=<slug>`. A first
  // sign-in through a provider never joins an existing account by itself.
  linkSso: (slug: string) =>
    request<{ authorize_url: string }>(`/auth/sso/${encodeURIComponent(slug)}/link`, {
      method: 'POST',
    }),
  unlinkSso: (slug: string) =>
    request<{ unlinked: boolean }>(`/auth/sso/${encodeURIComponent(slug)}/unlink`, {
      method: 'DELETE',
    }),
};
