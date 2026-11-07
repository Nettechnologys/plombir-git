import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { SvelteMap } from 'svelte/reactivity';
import Harness from '../test/RepositoryBoundaryHarness.svelte';
import { auth, repos, resetTestClient, ApiError } from '../test/client';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { click, renderComponent, settle, type RenderedComponent } from '../test/render';
import { locale } from '../i18n';

let page: RenderedComponent | undefined;
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((a, b) => { resolve = a; reject = b; });
  return { promise, resolve, reject };
}
beforeEach(async () => {
  resetTestClient(); locale.set('en');
  auth.me.mockRejectedValue(new ApiError('authentication required', 401));
  await fetchUser();
  setTestPage('/fixture/secret', { owner: 'fixture', repo: 'secret' });
});
afterEach(async () => { await page?.destroy(); page = undefined; await logout(); });

describe('repository page visibility', () => {
  it.each([401, 403, 404])('renders the same full-page 404 on denial %s, without repository UI', async status => {
    repos.get.mockRejectedValue(new ApiError('private fixture/secret diagnostic', status));
    page = await renderComponent(Harness);
    expect(page.container.textContent).toContain('404');
    expect(page.container.textContent).toContain('Page not found');
    expect(page.container.textContent).not.toContain('fixture');
    expect(page.container.querySelector('[data-repository-content]')).toBeNull();
    expect(page.container.querySelector('a[href="/"]')).not.toBeNull();
    expect(document.title).not.toContain('secret');
  });

  it('does not mount repository content while permission is unknown', async () => {
    const pending = deferred<object>(); repos.get.mockReturnValue(pending.promise);
    page = await renderComponent(Harness);
    expect(page.container.querySelector('[data-repository-content]')).toBeNull();
    pending.resolve({ id: 1 }); await settle();
    expect(page.container.querySelector('[data-repository-content]')).not.toBeNull();
  });

  it('does not turn a network/server error into not-found and supports retry', async () => {
    repos.get.mockRejectedValueOnce(new ApiError('database password must not be displayed', 503));
    page = await renderComponent(Harness);
    expect(page.container.textContent).toContain('503');
    expect(page.container.textContent).not.toContain('database password');
    repos.get.mockResolvedValue({ id: 1 });
    await click(page.container.querySelector('button')!);
    expect(page.container.querySelector('[data-repository-content]')).not.toBeNull();
  });

  it('discards a late allowed response after navigating to another repository', async () => {
    const old = deferred<object>();
    repos.get.mockReturnValueOnce(old.promise).mockRejectedValue(new ApiError('repository not found', 404));
    page = await renderComponent(Harness);
    setTestPage('/fixture/absent/network', { owner: 'fixture', repo: 'absent' }); await settle();
    old.resolve({ id: 1 }); await settle();
    expect(page.container.textContent).toContain('404');
    expect(page.container.querySelector('[data-repository-content]')).toBeNull();
  });

  it('checks again when the account changes on the same URL', async () => {
    auth.me.mockResolvedValue({ id: 1, username: 'owner' }); await fetchUser();
    repos.get.mockResolvedValueOnce({ id: 1 });
    page = await renderComponent(Harness);
    expect(page.container.querySelector('[data-repository-content]')).not.toBeNull();
    const next = deferred<object>(); repos.get.mockReturnValue(next.promise);
    auth.me.mockResolvedValue({ id: 2, username: 'outsider' }); await fetchUser(); await settle();
    expect(page.container.querySelector('[data-repository-content]')).toBeNull();
    next.reject(new ApiError('repository not found', 404)); await settle();
    expect(page.container.textContent).toContain('404');
  });

  it('does not publish a response after leaving the repository layout', async () => {
    const pending = deferred<object>(); repos.get.mockReturnValue(pending.promise);
    page = await renderComponent(Harness); await page.destroy(); page = undefined;
    pending.resolve({ id: 1 }); await settle();
    expect(document.querySelector('[data-repository-content]')).toBeNull();
  });
  it('rechecks permission when the branch changes without changing the pathname', async () => {
    repos.get.mockResolvedValueOnce({ id: 1 });
    page = await renderComponent(Harness);
    expect(page.container.querySelector('[data-repository-content]')).not.toBeNull();
    repos.get.mockRejectedValue(new ApiError('repository not found', 404));
    setTestPage('/fixture/secret?ref=dev', { owner: 'fixture', repo: 'secret' }); await settle();
    expect(page.container.textContent).toContain('404');
    expect(page.container.querySelector('[data-repository-content]')).toBeNull();
  });

  it('keeps mounted forms when sudo replaces the bearer for the same account', async () => {
    const credential = new SvelteMap([['token', 'original']]);
    repos.get.mockImplementation(async () => { credential.get('token'); return { id: 1 }; });
    page = await renderComponent(Harness);
    const content = page.container.querySelector('[data-repository-content]');
    credential.set('token', 'after-sudo'); await settle();
    expect(repos.get).toHaveBeenCalledTimes(1);
    expect(page.container.querySelector('[data-repository-content]')).toBe(content);
  });

});
