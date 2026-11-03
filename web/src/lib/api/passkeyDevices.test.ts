import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { locale } from '$lib/i18n';
import { setToken } from './_base.svelte';
import { passkeys, type PasskeyDevice } from './passkeys';

const fetchMock = vi.fn();
const create = vi.fn();
const get = vi.fn();
const bytes = () => new Uint8Array([1, 2, 3]).buffer;
const reply = (body: unknown) => new Response(JSON.stringify(body), {
  headers: { 'content-type': 'application/json' },
});

beforeEach(() => {
  locale.set('en');
  setToken(null);
  vi.stubGlobal('fetch', fetchMock);
  vi.stubGlobal('PublicKeyCredential', class {});
  vi.stubGlobal('navigator', { credentials: { create, get } });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.resetAllMocks();
  setToken(null);
});

describe('passkey device selection', () => {
  it.each<PasskeyDevice>(['auto', 'phone'])('preserves registration policy with the %s picker', async (device) => {
    fetchMock.mockResolvedValueOnce(reply({ publicKey: {
      challenge: 'AQID',
      rp: { id: 'git.example.test', name: 'Plombir Git' },
      user: { id: 'AQID', name: 'alice', displayName: 'Alice' },
      pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
      authenticatorSelection: { userVerification: 'required', residentKey: 'preferred' },
      excludeCredentials: [{ type: 'public-key', id: 'AQID', transports: ['internal', 'hybrid'] }],
      extensions: { credProps: true },
      timeout: 60000,
    } })).mockResolvedValueOnce(reply([]));
    create.mockResolvedValue({
      id: 'AQID', rawId: bytes(), type: 'public-key',
      response: {
        attestationObject: bytes(), clientDataJSON: bytes(),
        getTransports: () => ['hybrid'],
      },
    });

    await passkeys.register('Phone', device);

    const options = create.mock.calls[0][0].publicKey;
    expect(new Uint8Array(options.challenge)).toEqual(new Uint8Array([1, 2, 3]));
    expect(new Uint8Array(options.user.id)).toEqual(new Uint8Array([1, 2, 3]));
    expect(options).toMatchObject({
      rp: { id: 'git.example.test' },
      user: { name: 'alice' },
      pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
      authenticatorSelection: { userVerification: 'required', residentKey: 'preferred' },
      extensions: { credProps: true },
      timeout: 60000,
    });
    expect(options.excludeCredentials).toHaveLength(1);
    expect(new Uint8Array(options.excludeCredentials[0].id)).toEqual(new Uint8Array([1, 2, 3]));
    expect(options.excludeCredentials[0].transports).toEqual(['internal', 'hybrid']);
    expect(options.hints).toEqual(device === 'phone' ? ['hybrid'] : undefined);
    expect(options.authenticatorSelection.authenticatorAttachment)
      .toBe(device === 'phone' ? 'cross-platform' : undefined);
    const sent = JSON.parse(fetchMock.mock.calls[1][1].body);
    expect(sent).toEqual({ name: 'Phone', credential: {
      id: 'AQID', rawId: 'AQID', type: 'public-key',
      response: { attestationObject: 'AQID', clientDataJSON: 'AQID', transports: ['hybrid'] },
    } });
  });

  it.each<PasskeyDevice>(['auto', 'phone'])('preserves the credential allow-list and UV with the %s login picker', async (device) => {
    fetchMock.mockResolvedValueOnce(reply({ publicKey: {
      challenge: 'AQID', rpId: 'git.example.test', userVerification: 'required',
      allowCredentials: [{ type: 'public-key', id: 'AQID', transports: ['internal'] }],
    } })).mockResolvedValueOnce(reply({ token: 'session', user_id: 1, username: 'alice' }));
    get.mockResolvedValue({
      id: 'AQID', rawId: bytes(), type: 'public-key',
      response: { authenticatorData: bytes(), clientDataJSON: bytes(), signature: bytes(), userHandle: null },
    });

    await passkeys.login('alice', device);

    const options = get.mock.calls[0][0].publicKey;
    expect(options.rpId).toBe('git.example.test');
    expect(options.userVerification).toBe('required');
    expect(new Uint8Array(options.challenge)).toEqual(new Uint8Array([1, 2, 3]));
    expect(options.allowCredentials).toHaveLength(1);
    expect(new Uint8Array(options.allowCredentials[0].id)).toEqual(new Uint8Array([1, 2, 3]));
    expect(options.allowCredentials[0].transports).toEqual(device === 'phone' ? undefined : ['internal']);
    expect(options.hints).toEqual(device === 'phone' ? ['hybrid'] : undefined);
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ username: 'alice' });
    expect(JSON.parse(fetchMock.mock.calls[1][1].body).response).toEqual({
      authenticatorData: 'AQID', clientDataJSON: 'AQID', signature: 'AQID', userHandle: null,
    });
  });

  it.each(['register', 'login'] as const)('does not finish or silently retry a cancelled phone %s ceremony', async (operation) => {
    fetchMock.mockResolvedValueOnce(reply({ publicKey: { challenge: 'AQID', user: { id: 'AQID' } } }));
    create.mockRejectedValue(new DOMException('Cancelled', 'NotAllowedError'));
    get.mockRejectedValue(new DOMException('Cancelled', 'NotAllowedError'));

    await expect(passkeys[operation]('alice', 'phone')).rejects.toThrow('cancelled or timed out');

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(create.mock.calls.length + get.mock.calls.length).toBe(1);
  });

  it('accepts registration responses from browsers without getTransports', async () => {
    fetchMock.mockResolvedValueOnce(reply({ publicKey: { challenge: 'AQID', user: { id: 'AQID' } } }))
      .mockResolvedValueOnce(reply([]));
    create.mockResolvedValue({
      id: 'AQID', rawId: bytes(), type: 'public-key',
      response: { attestationObject: bytes(), clientDataJSON: bytes() },
    });

    await passkeys.register('Key');

    expect(JSON.parse(fetchMock.mock.calls[1][1].body).credential.response).not.toHaveProperty('transports');
  });
});
