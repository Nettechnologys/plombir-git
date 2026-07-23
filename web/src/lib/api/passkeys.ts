import { request } from './_base.svelte';

export interface PasskeyInfo {
  id: number;
  name: string;
  created_at: string;
  last_used_at: string | null;
}

export interface PasskeyLoginResponse {
  token: string;
  user_id: number;
  username: string;
}

/** True when the browser exposes the WebAuthn API. */
export function isPasskeySupported(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.PublicKeyCredential !== 'undefined' &&
    typeof navigator !== 'undefined' &&
    !!navigator.credentials
  );
}

// ── base64url ⇄ ArrayBuffer (WebAuthn wire format) ────────────────────────

function b64urlToBuf(value: string): ArrayBuffer {
  const padded = value.length % 4 === 0 ? value : value + '='.repeat(4 - (value.length % 4));
  const base64 = padded.replace(/-/g, '+').replace(/_/g, '/');
  const binary = atob(base64);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

function bufToB64url(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let binary = '';
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

/**
 * Normalize a DOMException from the WebAuthn ceremony into a friendly message.
 * A user cancelling the browser dialog throws `NotAllowedError`.
 */
function ceremonyError(err: unknown, fallback: string): Error {
  if (err instanceof DOMException) {
    if (err.name === 'NotAllowedError') {
      return new Error('The request was cancelled or timed out.');
    }
    if (err.name === 'InvalidStateError') {
      return new Error('This authenticator is already registered.');
    }
    return new Error(err.message || fallback);
  }
  return err instanceof Error ? err : new Error(fallback);
}

export const passkeys = {
  list: () => request<PasskeyInfo[]>('/users/passkeys'),

  remove: (id: number) =>
    request<void>(`/users/passkeys/${id}`, { method: 'DELETE' }),

  /** Full registration ceremony: begin → browser prompt → finish. */
  async register(name: string): Promise<PasskeyInfo[]> {
    if (!isPasskeySupported()) throw new Error('This browser does not support passkeys.');

    const options = await request<{ publicKey: any }>('/users/passkeys/register/start', {
      method: 'POST',
    });
    const publicKey = options.publicKey;
    publicKey.challenge = b64urlToBuf(publicKey.challenge);
    publicKey.user.id = b64urlToBuf(publicKey.user.id);
    if (Array.isArray(publicKey.excludeCredentials)) {
      publicKey.excludeCredentials = publicKey.excludeCredentials.map((cred: any) => ({
        ...cred,
        id: b64urlToBuf(cred.id),
      }));
    }

    let credential: PublicKeyCredential | null;
    try {
      credential = (await navigator.credentials.create({ publicKey })) as PublicKeyCredential | null;
    } catch (err) {
      throw ceremonyError(err, 'Passkey registration failed.');
    }
    if (!credential) throw new Error('Passkey registration was cancelled.');

    const response = credential.response as AuthenticatorAttestationResponse;
    return request<PasskeyInfo[]>('/users/passkeys/register/finish', {
      method: 'POST',
      body: JSON.stringify({
        name,
        credential: {
          id: credential.id,
          rawId: bufToB64url(credential.rawId),
          type: credential.type,
          response: {
            attestationObject: bufToB64url(response.attestationObject),
            clientDataJSON: bufToB64url(response.clientDataJSON),
          },
        },
      }),
    });
  },

  /** Full authentication ceremony: begin → browser prompt → finish. */
  async login(username: string): Promise<PasskeyLoginResponse> {
    if (!isPasskeySupported()) throw new Error('This browser does not support passkeys.');

    const options = await request<{ publicKey: any }>('/users/passkeys/login/start', {
      method: 'POST',
      body: JSON.stringify({ username }),
    });
    const publicKey = options.publicKey;
    publicKey.challenge = b64urlToBuf(publicKey.challenge);
    if (Array.isArray(publicKey.allowCredentials)) {
      publicKey.allowCredentials = publicKey.allowCredentials.map((cred: any) => ({
        ...cred,
        id: b64urlToBuf(cred.id),
      }));
    }

    let credential: PublicKeyCredential | null;
    try {
      credential = (await navigator.credentials.get({ publicKey })) as PublicKeyCredential | null;
    } catch (err) {
      throw ceremonyError(err, 'Passkey login failed.');
    }
    if (!credential) throw new Error('Passkey login was cancelled.');

    const response = credential.response as AuthenticatorAssertionResponse;
    return request<PasskeyLoginResponse>('/users/passkeys/login/finish', {
      method: 'POST',
      body: JSON.stringify({
        id: credential.id,
        rawId: bufToB64url(credential.rawId),
        type: credential.type,
        response: {
          authenticatorData: bufToB64url(response.authenticatorData),
          clientDataJSON: bufToB64url(response.clientDataJSON),
          signature: bufToB64url(response.signature),
          userHandle: response.userHandle ? bufToB64url(response.userHandle) : null,
        },
      }),
    });
  },
};
