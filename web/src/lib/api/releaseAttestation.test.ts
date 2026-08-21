import { describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import releasesPageSource from '../../routes/[owner]/[repo]/releases/+page.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import clientBarrelSource from './client.svelte.ts?raw';
import { releases } from './releases';

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
  it('reaches the releases page through all three halves of the feature', () => {
    expect(releasesPageSource).toContain('releases.attestation.sign(');
    expect(releasesPageSource).toContain('releases.attestation.get(');
    expect(releasesPageSource).toContain('releases.attestation.verify(');
    expect(releasesPageSource).toContain('class="asset-attestation"');
  });

  it('is exported from the client barrel the routes import', () => {
    expect(clientBarrelSource).toContain("from './releases'");
    expect(clientBarrelSource).toContain("from './instance'");
  });

  // The load-bearing distinction. `verified: false` arrives with a 200 and
  // means the asset no longer matches what was signed — the loudest thing this
  // feature can say. Rendering it as "not signed" would turn the alarm off.
  it('keeps a failed check apart from an unsigned asset', () => {
    expect(releasesPageSource).toContain("t('releases.attestation.verify_failed')");
    expect(releasesPageSource).toContain("t('releases.attestation.unsigned')");

    const failedAt = releasesPageSource.indexOf('attestation.verify_failed');
    const unsignedAt = releasesPageSource.indexOf('attestation.unsigned');
    expect(failedAt).toBeGreaterThan(-1);
    expect(unsignedAt).toBeGreaterThan(-1);
    expect(failedAt).not.toEqual(unsignedAt);

    // And the branch that picks between them reads the report, not merely
    // whether one exists.
    expect(releasesPageSource).toMatch(/attestationReports\[asset\.id\]\.verified/);
  });

  // A third state, separate again: the check could not run. Folding an
  // unreachable blob store into "verify failed" accuses an asset of being
  // tampered with because of an infrastructure fault.
  it('keeps a check that could not run apart from a check that failed', () => {
    expect(releasesPageSource).toContain("t('releases.attestation.error'");
    expect(releasesPageSource).toContain('attestationErrors[asset.id]');
  });

  // Both endpoints answer 404 for "feature off" and for "asset never signed",
  // so the page asks the instance which one it is. An unknown capability must
  // not render as "off": on a provenance-enabled forge that would tell every
  // reader the forge has no provenance.
  it('asks the instance whether it does provenance at all', () => {
    expect(releasesPageSource).toContain('instance.get()');
    expect(releasesPageSource).toContain('attestation_enabled');
    expect(releasesPageSource).toContain('attestationEnabled === true');
    expect(releasesPageSource).not.toContain('attestationEnabled = false');
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
