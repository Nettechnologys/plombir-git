import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiError, isSudoRequired, request, requestSudo, setSudoPrompt, setToken } from './_base.svelte';

// Decision #10: the routes that mint keys, tokens, passkeys and SSO links
// answer `403 { reason: "sudo_required" }` to a session outside its sudo
// window. `request` turns that one answer into a prompt and a single retry,
// so no settings page has to know the refusal exists.

const fetchMock = vi.fn();

function json(status: number, body: unknown): Response {
	return new Response(JSON.stringify(body), {
		status,
		headers: { 'content-type': 'application/json' },
	});
}

const sudoRefusal = () =>
	json(403, { error: { code: 'FORBIDDEN', message: 'confirm it is you', reason: 'sudo_required' } });
const plainRefusal = () => json(403, { error: { code: 'FORBIDDEN', message: 'nope' } });

function authorization(call: number): string | undefined {
	return (fetchMock.mock.calls[call][1] as { headers: Record<string, string> }).headers.Authorization;
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
	it('prompts once and retries the same request on the re-issued session', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal()).mockResolvedValueOnce(json(201, { id: 7 }));
		const prompt = vi.fn(async () => {
			setToken('elevated');
			return true;
		});
		setSudoPrompt(prompt);

		const result = await request<{ id: number }>('/users/tokens', {
			method: 'POST',
			body: JSON.stringify({ name: 'cli' }),
		});

		expect(result).toEqual({ id: 7 });
		expect(prompt).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);
		expect(authorization(0)).toBe('Bearer plain');
		expect(authorization(1)).toBe('Bearer elevated');
		expect(fetchMock.mock.calls[1][1].body).toBe(JSON.stringify({ name: 'cli' }));
	});

	it('hands the refusal to the caller when the prompt is cancelled', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal());
		setSudoPrompt(async () => false);

		const error = await request('/users/ssh-keys', { method: 'POST' }).catch((e) => e);

		expect(isSudoRequired(error)).toBe(true);
		expect(fetchMock).toHaveBeenCalledTimes(1);
	});

	it('retries only once, and never for a refusal without the reason', async () => {
		fetchMock.mockResolvedValueOnce(sudoRefusal()).mockResolvedValueOnce(sudoRefusal());
		const prompt = vi.fn(async () => true);
		setSudoPrompt(prompt);
		const twice = await request('/users/ssh-keys', { method: 'POST' }).catch((e) => e);
		expect(isSudoRequired(twice)).toBe(true);
		expect(prompt).toHaveBeenCalledTimes(1);
		expect(fetchMock).toHaveBeenCalledTimes(2);

		fetchMock.mockResolvedValueOnce(plainRefusal());
		const plain = await request('/users/tokens', { method: 'POST' }).catch((e) => e);
		expect(plain).toBeInstanceOf(ApiError);
		expect(isSudoRequired(plain)).toBe(false);
		expect(prompt).toHaveBeenCalledTimes(1);
	});

	it('shares one prompt between refusals in the same tick', async () => {
		let release!: (confirmed: boolean) => void;
		const prompt = vi.fn(() => new Promise<boolean>((resolve) => (release = resolve)));
		setSudoPrompt(prompt);
		const first = requestSudo();
		const second = requestSudo();
		release(true);
		await expect(Promise.all([first, second])).resolves.toEqual([true, true]);
		expect(prompt).toHaveBeenCalledTimes(1);
	});

	it('answers false with no prompt mounted', async () => {
		await expect(requestSudo()).resolves.toBe(false);
	});
});
