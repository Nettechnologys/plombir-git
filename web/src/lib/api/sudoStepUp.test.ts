import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
	ApiError,
	isSudoRequired,
	request,
	requestSudo,
	setSudoPrompt,
	setToken,
} from './_base.svelte';
import { auth } from './auth';

// Security audit finding #7: the routes that mint SSH keys, tokens, passkeys
// and SSO links answer `403 { reason: "sudo_required" }` to a session that has
// not re-proved its password. `request` turns that one answer into a prompt
// and a single retry, so no settings page has to know the refusal exists.

const fetchMock = vi.fn();

function sudoRefusal(): Response {
	return new Response(
		JSON.stringify({
			error: {
				code: 'FORBIDDEN',
				message: 'confirm your password to create credentials',
				reason: 'sudo_required',
			},
		}),
		{ status: 403, headers: { 'content-type': 'application/json' } },
	);
}

function plainRefusal(): Response {
	return new Response(
		JSON.stringify({ error: { code: 'FORBIDDEN', message: 'nope' } }),
		{ status: 403, headers: { 'content-type': 'application/json' } },
	);
}

function created(body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status: 201,
		headers: { 'content-type': 'application/json' },
	});
}

function requestInit(call: number): RequestInit & { headers: Record<string, string> } {
	return fetchMock.mock.calls[call][1];
}

beforeEach(() => {
	vi.stubGlobal('fetch', fetchMock);
	setToken('plain');
});

afterEach(() => {
	setSudoPrompt(null);
	setToken(null);
	vi.unstubAllGlobals();
	vi.clearAllMocks();
});

describe('sudo_required interception', () => {
	it('prompts once, then retries the same request with the re-issued session', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal()).mockResolvedValueOnce(created({ id: 7 }));
		const prompt = vi.fn(async () => {
			setToken('elevated');
			return true;
		});
		setSudoPrompt(prompt);

		const body = JSON.stringify({ name: 'cli' });
		const result = await request<{ id: number }>('/users/tokens', { method: 'POST', body });

		expect(result).toEqual({ id: 7 });
		expect(prompt).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(requestInit(0).headers.Authorization).toBe('Bearer plain');
		const retry = requestInit(1);
		expect(fetchMock.mock.calls[1][0]).toBe('/api/v1/users/tokens');
		expect(retry.method).toBe('POST');
		expect(retry.body).toBe(body);
		expect(retry.headers.Authorization).toBe('Bearer elevated');
	});

	it('hands the original refusal to the caller when the prompt is cancelled', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal());
		const prompt = vi.fn(async () => false);
		setSudoPrompt(prompt);

		const error = await request('/users/ssh-keys', { method: 'POST', body: '{}' }).catch((e) => e);

		expect(isSudoRequired(error)).toBe(true);
		expect(error).toMatchObject({ status: 403, code: 'FORBIDDEN', reason: 'sudo_required' });
		expect(prompt).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('retries exactly once: a second refusal is final', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal()).mockResolvedValueOnce(sudoRefusal());
		const prompt = vi.fn(async () => true);
		setSudoPrompt(prompt);

		const error = await request('/users/tokens', { method: 'POST', body: '{}' }).catch((e) => e);

		expect(isSudoRequired(error)).toBe(true);
		expect(prompt).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);
	});

	it('leaves every other 403 alone', async () => {
		fetchMock.mockResolvedValueOnce(plainRefusal());
		const prompt = vi.fn(async () => true);
		setSudoPrompt(prompt);

		const error = await request('/users/tokens', { method: 'POST', body: '{}' }).catch((e) => e);

		expect(error).toBeInstanceOf(ApiError);
		expect(isSudoRequired(error)).toBe(false);
		expect(error.reason).toBeNull();
		expect(prompt).not.toHaveBeenCalled();
	});

	it('throws the refusal when no prompt is mounted', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal());

		const error = await request('/users/tokens', { method: 'POST', body: '{}' }).catch((e) => e);

		expect(isSudoRequired(error)).toBe(true);
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('shares one prompt between requests refused at the same time', async () => {
		let resolvePrompt: (confirmed: boolean) => void = () => {};
		const prompt = vi.fn(
			() =>
				new Promise<boolean>((resolve) => {
					resolvePrompt = resolve;
				}),
		);
		setSudoPrompt(prompt);

		const first = requestSudo();
		const second = requestSudo();
		expect(prompt).toHaveBeenCalledTimes(1);

		resolvePrompt(true);
		await expect(first).resolves.toBe(true);
		await expect(second).resolves.toBe(true);

		// Settled: the next refusal gets a fresh prompt.
		const third = requestSudo();
		expect(prompt).toHaveBeenCalledTimes(2);
		resolvePrompt(false);
		await expect(third).resolves.toBe(false);
	});
});

describe('auth.sudo', () => {
	it('posts the confirmation to /users/me/sudo and returns the login shape', async () => {
		fetchMock.mockResolvedValueOnce(
			new Response(JSON.stringify({ token: 'elevated', user_id: 1, username: 'alice', mfa_required: false }), {
				status: 200,
				headers: { 'content-type': 'application/json' },
			}),
		);

		const session = await auth.sudo({ password: 'pw', totp_code: '123456' });

		expect(session.token).toBe('elevated');
		expect(fetchMock.mock.calls[0][0]).toBe('/api/v1/users/me/sudo');
		const init = requestInit(0);
		expect(init.method).toBe('POST');
		expect(init.credentials).toBe('include');
		expect(JSON.parse(init.body as string)).toEqual({ password: 'pw', totp_code: '123456' });
	});
});
