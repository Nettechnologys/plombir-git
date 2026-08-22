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
import { reviews } from './pulls';

describe('review dismissal transport', () => {
  it('posts to the dismissal endpoint of one review, with owner and repo escaped', () => {
    reviews.dismiss('alice/bob', 'de mo', 7, 42, 'stale — the branch moved on');

    expect(base.request).toHaveBeenCalledWith('/repos/alice%2Fbob/de%20mo/pulls/7/reviews/42/dismiss', {
      method: 'POST',
      body: JSON.stringify({ message: 'stale — the branch moved on' }),
    });
  });
});

describe('review dismissal production wiring', () => {
  // The defect this file exists for: `POST .../reviews/{id}/dismiss` is
  // mounted, does take a stale approval back off the branch-protection counter
  // (card_dc0f5d58e5f4), and renders on the timeline — but nothing in the SPA
  // called it. The only way to withdraw an approval on a protected branch was
  // `curl` with a token (card_1714b4dacad5).
  it('reaches the pull request page through both halves of the feature', () => {
    expect(prPageSource).toContain('reviews.dismiss(');
    expect(prPageSource).toContain('class="dismiss-form"');
  });

  it('offers the action on the verdict entries, keyed by the review it withdraws', () => {
    const verdict = verdictDerivation();

    expect(verdict).toContain('review_approve');
    expect(verdict).toContain('review_request_changes');
    expect(verdict).toContain('reviewById.get(');
    expect(verdict).toContain('review_id');
  });

  // A `comment` review carries no verdict, so there is nothing to take back;
  // offering the action there would suggest the merge gate is involved when it
  // is not.
  it('does not offer it on review kinds the merge gate never counted', () => {
    expect(verdictDerivation()).not.toContain('review_comment');
  });

  // Both halves are load-bearing. `dismissed_at` is what the merge gate reads,
  // so an already-withdrawn verdict must not offer the action again — and it
  // must still say it was withdrawn, or the timeline shows a standing approval
  // that no longer counts.
  it('stops offering the action once the review is already withdrawn', () => {
    expect(prPageSource).toContain('{#if verdict && !verdict.dismissed_at');
    expect(prPageSource).toContain("{#if verdict?.dismissed_at}");
    expect(prPageSource).toContain("t('pulls.review.withdrawn')");
  });

  it('offers the dismissal only while the pull request is still open', () => {
    expect(prPageSource).toMatch(/\{#if verdict && !verdict\.dismissed_at && pr\.state === 'open'\}/);
  });

  // The endpoint takes a message and the timeline entry renders it: a
  // dismissal with no stated reason is a maintainer overriding an approval
  // without saying why.
  it('requires a reason before the dismissal can be sent', () => {
    expect(prPageSource).toContain('!dismissMessage.trim()');
    expect(prPageSource).toContain("t('pulls.review.dismiss_placeholder')");
  });

  it.each(['dismiss', 'dismiss_placeholder', 'dismiss_confirm', 'dismiss_cancel', 'dismissing', 'withdrawn'])(
    'has a real label in both catalogs: pulls.review.%s',
    (key) => {
      expect(en.pulls.review).toHaveProperty(key);
      expect(zhCN.pulls.review).toHaveProperty(key);
    },
  );
});

/// The `{@const verdict = ...}` binding the timeline entry hangs the action on,
/// read out of the page source.
///
/// Source-level like its sibling `prCiApproval.test.ts`: `web/src/lib/api/*` has
/// no DOM harness, and the subject really is which entries the action attaches
/// to, not its pixels.
function verdictDerivation(): string {
  const start = prPageSource.indexOf('{@const verdict =');
  expect(start, 'the timeline must resolve the review row behind a verdict entry').toBeGreaterThan(-1);
  const end = prPageSource.indexOf('}\n', start);
  expect(end, 'the `verdict` binding must be closed').toBeGreaterThan(start);
  return prPageSource.slice(start, end);
}
