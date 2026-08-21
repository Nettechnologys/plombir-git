import { describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import pipelinePageSource from '../../routes/[owner]/[repo]/pipelines/+page.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { artifacts } from './artifacts';
import clientBarrelSource from './client.svelte.ts?raw';

describe('CI artifact client transport', () => {
  it('lists the artifacts of one pipeline, with owner and repo escaped', () => {
    artifacts.list('alice/bob', 'de mo', 7);

    expect(base.request).toHaveBeenCalledWith('/repos/alice%2Fbob/de%20mo/pipelines/7/artifacts');
  });

  it('downloads through the authenticated file path, not a bare link', () => {
    // The bytes sit behind `RepoRead`. An `<a href>` would fetch them without
    // the bearer token, which on a private repository is a 404 the user cannot
    // tell apart from a missing artifact.
    artifacts.download(11, 'build-report');

    expect(base.downloadApiFile).toHaveBeenCalledWith('/artifacts/11/download', 'build-report');
  });

  it('falls back to a filename rather than saving an unnamed blob', () => {
    artifacts.download(12, '');

    expect(base.downloadApiFile).toHaveBeenCalledWith('/artifacts/12/download', 'artifact');
  });
});

describe('CI artifact production wiring', () => {
  // The defect this file exists for: the retention settings page offered a
  // lifetime for artifacts the SPA had no way to list or fetch, so the only way
  // to reach a published artifact was `curl` with a token (card_d02e6e977015).
  it('reaches the pipeline page through both halves of the feature', () => {
    expect(pipelinePageSource).toContain('artifacts.list(');
    expect(pipelinePageSource).toContain('artifacts.download(');
    expect(pipelinePageSource).toContain('class="artifact-list"');
    expect(pipelinePageSource).toContain('artifact-download');
  });

  it('is exported from the client barrel the routes import', () => {
    expect(clientBarrelSource).toContain("from './artifacts'");
  });

  it('keeps an empty list a normal state and a failure a visible one', () => {
    // Two separate branches on purpose: "no job declared `artifacts:`" and
    // "the request failed" must not render as the same thing, or the retention
    // page ends up offering to expire files the user was told do not exist.
    expect(pipelinePageSource).toContain("t('pipeline.artifacts_empty')");
    expect(pipelinePageSource).toContain("t('pipeline.artifacts_load_failed'");
  });

  it.each([
    'artifacts',
    'artifacts_empty',
    'artifacts_hint',
    'artifacts_field',
    'artifacts_load_failed',
    'artifact_download',
    'artifact_downloading',
    'artifact_download_failed',
    'artifact_expires',
    'artifact_expires_never',
  ])('has a real label in both catalogs: pipeline.%s', (key) => {
    expect(en.pipeline).toHaveProperty(key);
    expect(zhCN.pipeline).toHaveProperty(key);
  });
});
