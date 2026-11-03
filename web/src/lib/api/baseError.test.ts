import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiError, downloadApiFile, request, requestBytes, getToken, setToken } from './_base.svelte';

const fetchMock = vi.fn();

function failedResponse(status: number, reason?: string): Response {
	return new Response(
		JSON.stringify({
			error: {
				code: 'storage_unavailable',
                reason,
				message: 'attestation store unavailable',
				request_id: 'request-17',
			},
		}),
		{ status, headers: { 'content-type': 'application/json' } },
	);
}

beforeEach(() => {
	vi.stubGlobal('fetch', fetchMock);
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.clearAllMocks();
	setToken(null);
});

describe('API response errors', () => {
	it.each([
		['JSON request', () => request('/instance')],
		['file download', () => downloadApiFile('/instance', 'instance.json')],
	])('keeps the HTTP status and backend envelope for a failed %s', async (_name, operation) => {
		fetchMock.mockResolvedValue(failedResponse(503));

		const error = await operation().catch((cause: unknown) => cause);

		expect(error).toBeInstanceOf(ApiError);
		expect(error).toMatchObject({
			name: 'ApiError',
			message: 'attestation store unavailable',
			status: 503,
			code: 'storage_unavailable',
			requestId: 'request-17',
		});
	});
});


describe('rejected session recovery', () => {
  it.each([
    ['JSON', () => request('/users/me')],
    ['bytes', () => requestBytes('/repos/owner/repo/raw/main/file')],
    ['download', () => downloadApiFile('/instance', 'instance.json')],
  ])('stops replaying a rejected bearer after a %s request', async (_name, operation) => {
    setToken('revoked');
    fetchMock.mockResolvedValueOnce(failedResponse(401, 'session_revoked'));
    await expect(operation()).rejects.toMatchObject({ status: 401 });
    expect(getToken()).toBeNull();
    fetchMock.mockResolvedValueOnce(new Response('{}'));
    await request('/users/passkeys/login/start', { method: 'POST' });
    expect(fetchMock.mock.calls[1][1].headers.Authorization).toBeUndefined();
  });

  it.each(['/users/me/sudo', '/users/mfa/verify'])('preserves a valid bearer on a wrong proof at %s', async (path) => {
    setToken('valid');
    fetchMock.mockResolvedValueOnce(failedResponse(401));
    await expect(request(path, { method: 'POST' })).rejects.toMatchObject({ status: 401 });
    expect(getToken()).toBe('valid');
  });

  it('does not clear a fresh login when an older request is refused late', async () => {
    let resolve!: (response: Response) => void;
    fetchMock.mockImplementationOnce(() => new Promise<Response>((done) => { resolve = done; }));
    setToken('old');
    const pending = request('/users/me');
    setToken('new');
    resolve(failedResponse(401, 'session_revoked'));
    await expect(pending).rejects.toMatchObject({ status: 401 });
    expect(getToken()).toBe('new');
  });
});
