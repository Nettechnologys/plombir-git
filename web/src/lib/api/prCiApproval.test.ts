import { describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import prPageSource from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { pulls } from './pulls';

describe('fork CI approval transport', () => {
  it('posts to the maintainer endpoint, with owner and repo escaped', () => {
    pulls.approveCi('alice/bob', 'de mo', 7);

    expect(base.request).toHaveBeenCalledWith('/repos/alice%2Fbob/de%20mo/pulls/7/ci-approval', {
      method: 'POST',
    });
  });
});

describe('fork CI approval production wiring', () => {
  // The defect this file exists for: `trigger_pull_request_ci` holds a fork
  // PR's pipeline until `ci_approved_sha` matches the head, and the only thing
  // that can lift that hold — `POST .../ci-approval`, behind `RepoWrite` — had
  // no client and no button. A maintainer saw a fork PR with no pipeline and no
  // explanation, and the only way through was `curl` with a token
  // (card_3c0751fbf09d).
  it('reaches the pull request page through both halves of the feature', () => {
    expect(prPageSource).toContain('pulls.approveCi(');
    expect(prPageSource).toContain('class="ci-held"');
  });

  it('tells the maintainer why the pipeline is missing, not just that it is', () => {
    // A bare button would be worse than nothing here: the person clicking it is
    // handing this repository's CI secrets to code they did not write, and the
    // banner is where that is said.
    expect(prPageSource).toContain("t('pulls.fork_ci.held')");
    expect(prPageSource).toContain("t('pulls.fork_ci.explanation')");
    expect(en.pulls.fork_ci.explanation).toMatch(/secret/i);
  });

  // The load-bearing half of the condition. An approval is recorded against one
  // commit precisely so that pushing something else to the fork puts the hold
  // back; a banner keyed on "is a fork" alone would vanish after the first
  // approval and never return, which is the same hole the approval closes.
  it('holds CI again when the fork head moves past the approved commit', () => {
    const condition = forkCiCondition();

    expect(condition).toContain('head_repo_id');
    expect(condition).toContain('ci_approved_sha');
    expect(condition).toContain('head_sha');
    expect(condition).toMatch(/ci_approved_sha\s*!==\s*pr\?\.head_sha/);
  });

  it('offers the approval only while the pull request is still open', () => {
    expect(forkCiCondition()).toContain("state === 'open'");
  });

  it.each(['held', 'explanation', 'approve', 'approving'])(
    'has a real label in both catalogs: pulls.fork_ci.%s',
    (key) => {
      expect(en.pulls.fork_ci).toHaveProperty(key);
      expect(zhCN.pulls.fork_ci).toHaveProperty(key);
    },
  );
});

/// The body of the `ciHeldForFork` derivation, read out of the page source.
///
/// Asserting on the source rather than rendering the component keeps this in
/// the same lane as the rest of `web/src/lib/api/*.test.ts`, which have no DOM
/// harness — and the subject really is the condition, not its pixels.
function forkCiCondition(): string {
  const start = prPageSource.indexOf('let ciHeldForFork = $derived(');
  expect(start, 'the page must derive whether CI is held for this fork').toBeGreaterThan(-1);
  const end = prPageSource.indexOf(');', start);
  expect(end, 'the `ciHeldForFork` derivation must be closed').toBeGreaterThan(start);
  return prPageSource.slice(start, end);
}
