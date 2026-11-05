import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiError, downloadApiFile, request, requestBytes, getToken, setToken, setIdentityConflictHandler } from './_base.svelte';

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
	setIdentityConflictHandler(null);
});

describe('API response errors', () => {
	it.each([
		['JSON request', () => request('/instance')],
		['byte request', () => requestBytes('/instance')],
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

	it.each([
		['JSON request', () => request('/users/logout', { method: 'POST' })],
		['byte request', () => requestBytes('/instance')],
		['file download', () => downloadApiFile('/instance', 'instance.json')],
	])('reports an opaque refusal without disclosing its body or credentials for a %s', async (_name, operation) => {
		setToken('private-session');
		fetchMock.mockResolvedValue(new Response('<html>Forbidden private-body</html>', {
			status: 403,
			headers: {
				'content-type': 'text/html; charset=utf-8',
				server: 'nginx',
				'x-request-id': 'proxy-request-17',
				'set-cookie': 'private-cookie',
				authorization: 'Bearer private-response-token',
			},
		}));

		const error = await operation().catch((cause: unknown) => cause);

		expect(error).toMatchObject({
			status: 403,
			message: 'HTTP 403 [content-type: text/html; charset=utf-8; server: nginx; request-id: proxy-request-17]',
			requestId: 'proxy-request-17', code: null, reason: null,
		});
		expect(JSON.stringify(error)).not.toContain('private-');
		expect(getToken()).toBe('private-session');
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('keeps a header request id when an API refusal omits it', async () => {
		fetchMock.mockResolvedValue(new Response(JSON.stringify({ error: { message: 'Access denied' } }), {
			status: 403, headers: { 'content-type': 'application/json', 'x-request-id': 'header-id' },
		}));
		await expect(request('/instance')).rejects.toMatchObject({ message: 'Access denied', requestId: 'header-id' });
	});

	it('bounds diagnostic headers and handles an empty error without headers', async () => {
		fetchMock.mockResolvedValueOnce(new Response(null, { status: 403, headers: { server: 'x'.repeat(1000) } }))
			.mockResolvedValueOnce(new Response(null, { status: 403 }));
		await expect(request('/instance')).rejects.toMatchObject({ message: `HTTP 403 [server: ${'x'.repeat(128)}]` });
		await expect(request('/instance')).rejects.toMatchObject({ message: 'HTTP 403' });
	});
});


describe('rejected session recovery', () => {
  it.each([
    ['JSON', () => request('/users/me')],
    ['bytes', () => requestBytes('/repos/owner/repo/raw/main/file')],
    ['download', () => downloadApiFile('/instance', 'instance.json')],
  ])('reconciles a %s identity conflict without retrying or prematurely clearing its bearer', async (_name, operation) => {
    setToken('alice');
    const reconcile = vi.fn(async () => { expect(getToken()).toBe('alice'); });
    setIdentityConflictHandler(reconcile);
    fetchMock.mockResolvedValueOnce(failedResponse(401, 'identity_conflict'));
    await expect(operation()).rejects.toMatchObject({ reason: 'identity_conflict' });
    expect(reconcile).toHaveBeenCalledOnce();
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(getToken()).toBe('alice');
  });

  it('ignores a late identity conflict from a previous login', async () => {
    const reconcile = vi.fn(async () => {});
    setIdentityConflictHandler(reconcile);
    let resolve!: (response: Response) => void;
    fetchMock.mockImplementationOnce(() => new Promise<Response>((done) => { resolve = done; }));
    setToken('old');
    const pending = request('/users/me');
    setToken('new');
    resolve(failedResponse(401, 'identity_conflict'));
    await expect(pending).rejects.toMatchObject({ reason: 'identity_conflict' });
    expect(reconcile).not.toHaveBeenCalled();
    expect(getToken()).toBe('new');
  });

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
