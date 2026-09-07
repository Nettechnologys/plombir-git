import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { issues } from './issues';

const fetchMock = vi.fn();

function emptyPage(): Response {
	return new Response(
		JSON.stringify({
			data: [],
			pagination: { page: 1, per_page: 20, total: 0, total_pages: 0 },
		}),
		{ status: 200, headers: { 'content-type': 'application/json' } },
	);
}

function requestedUrl(): string {
	const [url] = fetchMock.mock.calls.at(-1) ?? [];
	return String(url);
}

beforeEach(() => {
	vi.stubGlobal('fetch', fetchMock);
	fetchMock.mockImplementation(() => Promise.resolve(emptyPage()));
});

afterEach(() => {
	vi.unstubAllGlobals();
	vi.clearAllMocks();
});

describe('issues.list label filter', () => {
	// The backend refuses only an empty label name, so `release, urgent` is a
	// name it will create. Sent through the comma-separated spelling it would
	// arrive as the two names `release` and `urgent`, and where both of those
	// also exist the server answers 200 with the wrong issues instead of failing.
	it('sends a name containing a comma as one repeated key', async () => {
		await issues.list('alice', 'demo', undefined, undefined, undefined, ['release, urgent']);

		const url = requestedUrl();
		expect(url).toContain('label=release%2C%20urgent');
		expect(url).not.toContain('labels=');
	});

	it('sends one repeated key per name, so the AND survives the encoding', async () => {
		await issues.list('alice', 'demo', undefined, undefined, undefined, ['bug', 'urgent']);

		expect(requestedUrl()).toContain('label=bug&label=urgent');
	});

	it('keeps the other filters alongside the label keys', async () => {
		await issues.list('alice', 'demo', 'open', 2, 50, ['release, urgent']);

		const url = requestedUrl();
		expect(url).toContain('state=open');
		expect(url).toContain('page=2');
		expect(url).toContain('per_page=50');
		expect(url).toContain('label=release%2C%20urgent');
	});

	it('drops an empty name rather than asking for a label that cannot exist', async () => {
		await issues.list('alice', 'demo', undefined, undefined, undefined, ['bug', '']);

		const url = requestedUrl();
		expect(url).toContain('label=bug');
		expect(url).not.toContain('label=&');
		expect(url.endsWith('label=')).toBe(false);
	});

	it('asks no label question at all for an empty array', async () => {
		await issues.list('alice', 'demo', 'open', undefined, undefined, []);

		const url = requestedUrl();
		expect(url).toContain('state=open');
		expect(url).not.toContain('label');
	});

	// The older spelling is still a supported way to ask, and it has to keep
	// meaning what it always meant for the callers that use it.
	it('leaves a plain string on the legacy comma-separated key', async () => {
		await issues.list('alice', 'demo', undefined, undefined, undefined, 'bug,urgent');

		const url = requestedUrl();
		expect(url).toContain('labels=bug%2Curgent');
		expect(url).not.toContain('label=bug');
	});
});
