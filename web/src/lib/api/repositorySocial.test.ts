import { beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  request: vi.fn(),
  qs: vi.fn(() => '?page=2&per_page=20'),
}));

vi.mock('./_base.svelte', () => base);

import networkPageSource from '../../routes/[owner]/[repo]/network/+page.svelte?raw';
import repoHeaderSource from '../components/RepoHeader.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { repos } from './repos';

describe('repository social-list transport', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    base.qs.mockReturnValue('?page=2&per_page=20');
  });

  it('loads a typed stargazer page through the canonical pagination helper', () => {
    repos.stargazers('alice', 'demo', 2, 20);

    expect(base.qs).toHaveBeenCalledWith({ page: 2, per_page: 20 });
    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice/demo/stargazers?page=2&per_page=20',
    );
  });

  it('loads a typed fork page through the same pagination contract', () => {
    repos.forks('alice', 'demo', 2, 20);

    expect(base.qs).toHaveBeenCalledWith({ page: 2, per_page: 20 });
    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice/demo/forks?page=2&per_page=20',
    );
  });
});

describe('repository social-list production wiring', () => {
  it('is reachable from the repository navigation and highlights its tab', () => {
    expect(repoHeaderSource).toContain("{ id: 'network', label: t('repo.tabs.network')");
    expect(networkPageSource).toContain('activeTab="network"');
  });

  it('calls both formerly orphaned client methods with independent page state', () => {
    expect(networkPageSource).toContain(
      'repos.stargazers(requestedOwner, requestedRepo, requestedPage, PER_PAGE)',
    );
    expect(networkPageSource).toContain(
      'repos.forks(requestedOwner, requestedRepo, requestedPage, PER_PAGE)',
    );
    expect(networkPageSource).toContain('stargazersPage -= 1');
    expect(networkPageSource).toContain('stargazersPage += 1');
    expect(networkPageSource).toContain('forksPage -= 1');
    expect(networkPageSource).toContain('forksPage += 1');
  });

  it('renders explicit loading, error, and empty states for each list', () => {
    for (const state of [
      'stargazersLoading',
      'stargazersError',
      'stargazers.length === 0',
      'forksLoading',
      'forksError',
      'forks.length === 0',
    ]) {
      expect(networkPageSource).toContain(state);
    }
    expect(networkPageSource.match(/role="alert"/g)).toHaveLength(2);
    expect(networkPageSource.match(/aria-busy=/g)).toHaveLength(2);
  });

  it('links public identities rather than exposing database ids as navigation', () => {
    expect(networkPageSource).toContain('href={`/${stargazer.username}`}');
    expect(networkPageSource).toContain('href={`/${fork.owner_name}/${fork.name}`}');
    expect(networkPageSource).not.toMatch(/href=\{`\/\$\{(?:stargazer\.user_id|fork\.owner_id)/);
  });

  it.each(['network'])(
    'has a real repository tab label in both catalogs: repo.tabs.%s',
    (key) => {
      expect(en.repo.tabs).toHaveProperty(key);
      expect(zhCN.repo.tabs).toHaveProperty(key);
    },
  );

  it.each([
    'title',
    'stargazers_empty',
    'stargazers_load_failed',
    'starred_on',
    'forks_empty',
    'forks_load_failed',
    'page',
  ])('has a real social-list label in both catalogs: repo.social.%s', (key) => {
    expect(en.repo.social).toHaveProperty(key);
    expect(zhCN.repo.social).toHaveProperty(key);
  });
});
