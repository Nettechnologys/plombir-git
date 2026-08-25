import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import ReleasesPage from '../../routes/[owner]/[repo]/releases/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { releases } from './releases';
import { setTestPage } from '../test/app';
import {
	instance,
	releases as routeReleases,
	resetTestClient,
} from '../test/client';
import { button, click, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const asset = {
	id: 11,
	release_id: 7,
	filename: 'forgekeep.zip',
	size: 7,
	content_type: 'application/zip',
	download_count: 0,
	uploader_id: 3,
	created_at: '2026-08-15T12:00:00Z',
	sha256: null,
};

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
	instance.get.mockResolvedValue({ attestation_enabled: true });
	routeReleases.list.mockResolvedValue({
		data: [
			{
				id: 7,
				tag_name: 'v1.0.0',
				title: 'Version 1',
				body: '',
				is_prerelease: false,
				is_draft: false,
				created_at: '2026-08-15T12:00:00Z',
			},
		],
		pagination: { total_pages: 1 },
	});
	routeReleases.listAssets.mockResolvedValue([asset]);
	routeReleases.attestation.get.mockResolvedValue({ signature: 'test' });
	routeReleases.attestation.verify.mockResolvedValue({
		verified: false,
		reason: 'digest mismatch',
	});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('release attestation transport', () => {
  it('signs through the maintainer endpoint, with owner and repo escaped', () => {
    releases.attestation.sign('alice/bob', 'de mo', 7);

    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice%2Fbob/de%20mo/releases/assets/7/attestation',
      { method: 'POST' },
    );
  });

  it('reads the stored envelope without a body', () => {
    releases.attestation.get('alice/bob', 'de mo', 7);

    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice%2Fbob/de%20mo/releases/assets/7/attestation',
    );
  });

  it('verifies against the asset bytes through its own endpoint', () => {
    // A separate path from `get` on purpose: reading the envelope proves the
    // asset was signed once, verifying proves the bytes still match it.
    releases.attestation.verify('alice/bob', 'de mo', 7);

    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice%2Fbob/de%20mo/releases/assets/7/attestation/verify',
      { method: 'POST' },
    );
  });
});

describe('release attestation production wiring', () => {
  // The defect this file exists for: three endpoints, a README section
  // explaining what key rotation does to past attestations, and a ✅ in
  // `FEATURE_INVENTORY.md` — while the word `attestation` appeared nowhere in
  // `web/src`, so signing or checking anything meant `curl` with a token
  // (card_5e52392a0274).
	// The load-bearing distinction. `verified: false` arrives with a 200 and
	// means the asset no longer matches what was signed — the loudest thing this
	// feature can say. Rendering it as "not signed" would turn the alarm off.
	it('renders a failed verification as tampering, not as unsigned', async () => {
		rendered = await renderComponent(ReleasesPage);
		expect(rendered.container.textContent).toContain('Signed');
		await click(button(rendered.container, 'Verify'));

		expect(routeReleases.attestation.verify).toHaveBeenCalledWith('alice', 'demo', 11);
		expect(rendered.container.textContent).toContain('Provenance check failed');
		expect(rendered.container.textContent).toContain('digest mismatch');
		expect(rendered.container.textContent).not.toContain('Not signed');
	});

  // A third state, separate again: the check could not run. Folding an
  // unreachable blob store into "verify failed" accuses an asset of being
  // tampered with because of an infrastructure fault.
	it('renders an unavailable check separately from a negative verdict', async () => {
		routeReleases.attestation.verify.mockRejectedValue(new Error('blob store unavailable'));
		rendered = await renderComponent(ReleasesPage);
		await click(button(rendered.container, 'Verify'));

		expect(rendered.container.textContent).toContain('blob store unavailable');
		expect(rendered.container.textContent).not.toContain('Provenance check failed');
	});

  // Both endpoints answer 404 for "feature off" and for "asset never signed",
  // so the page asks the instance which one it is. An unknown capability must
  // not render as "off": on a provenance-enabled forge that would tell every
  // reader the forge has no provenance.
	it('renders an unsigned asset and signs it only when provenance is enabled', async () => {
		routeReleases.attestation.get.mockRejectedValue(new Error('not signed'));
		rendered = await renderComponent(ReleasesPage);
		expect(instance.get).toHaveBeenCalled();
		expect(rendered.container.textContent).toContain('Not signed');

		await click(button(rendered.container, 'Sign'));
		expect(routeReleases.attestation.sign).toHaveBeenCalledWith('alice', 'demo', 11);
		expect(rendered.container.textContent).toContain('Signed');
	});

  it.each([
    'signed',
    'unsigned',
    'verified',
    'verify_failed',
    'sign',
    'signing',
    'verify',
    'verifying',
    'error',
  ])('has a real label in both catalogs: releases.attestation.%s', (key) => {
    expect(en.releases.attestation).toHaveProperty(key);
    expect(zhCN.releases.attestation).toHaveProperty(key);
  });
});
