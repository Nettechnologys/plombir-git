import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ApiError, downloadApiFile, request } from './_base.svelte';

const fetchMock = vi.fn();

function failedResponse(status: number): Response {
	return new Response(
		JSON.stringify({
			error: {
				code: 'storage_unavailable',
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
