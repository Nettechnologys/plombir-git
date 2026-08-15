import { describe, expect, it } from 'vitest';

// The page's and the client's own sources, pulled in by vite so the checks
// below need no node filesystem API (and no `@types/node` for `npm run check`).
import versionsPageSource from '../../routes/[owner]/[repo]/packages/[format]/[...name]/+page.svelte?raw';
import packagesClientSource from './packages.ts?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import {
  buildPackageYankPayload,
  nextYankState,
  packageYankPath,
  type PackageVersionRef,
} from './packageYank';

function versionRef(overrides: Partial<PackageVersionRef> = {}): PackageVersionRef {
  return { owner: 'acme', repo: 'tools', pkg_type: 'npm', pkg_name: 'widget', version: '1.2.3', ...overrides };
}

/**
 * What the network actually carries — `JSON.stringify` is what the client does
 * with the body, and it drops any key whose value is `undefined`.
 */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('nextYankState', () => {
  it('withdraws a live version', () => {
    expect(nextYankState(false)).toBe(true);
  });

  it('restores a yanked one, which is the direction delete has no equivalent for', () => {
    expect(nextYankState(true)).toBe(false);
  });

  it('treats a version the server sent no flag for as live', () => {
    expect(nextYankState(undefined)).toBe(true);
  });
});

describe('buildPackageYankPayload', () => {
  it('sends the target state, not a direction', () => {
    expect(wire(buildPackageYankPayload(true))).toEqual({ yank: true });
  });

  it('sends `false` rather than omitting it, so unyank reaches the server', () => {
    const body = wire(buildPackageYankPayload(false));

    // `yank_version` deserializes `YankRequest { yank: bool }`; a body that
    // leaves the key out is a 422, not a no-op — and the audit action it picks
    // (`package.yank` vs `package.unyank`) is this field and nothing else.
    expect(body).toHaveProperty('yank');
    expect(body.yank).toBe(false);
  });
});

describe('packageYankPath', () => {
  it('addresses the PATCH endpoint the router mounts', () => {
    expect(packageYankPath(versionRef())).toBe('/repos/acme/tools/packages/npm/widget/1.2.3/yank');
  });

  it('encodes a scoped package name the way its sibling calls encode it', () => {
    const path = packageYankPath(versionRef({ pkg_name: '@acme/widget' }));

    expect(path).toBe('/repos/acme/tools/packages/npm/%40acme%2Fwidget/1.2.3/yank');
  });

  it('encodes a version with build metadata', () => {
    const path = packageYankPath(versionRef({ version: '1.0.0+build/1' }));

    expect(path).toBe('/repos/acme/tools/packages/npm/widget/1.0.0%2Bbuild%2F1/yank');
  });
});

describe('the yank control the version list actually renders', () => {
  it('is built by the client out of this module', () => {
    expect(packagesClientSource).toContain('packageYankPath');
    expect(packagesClientSource).toContain('buildPackageYankPayload');
    expect(packagesClientSource).toMatch(/method:\s*'PATCH'/);
  });

  it('is reached from the versions page, which had only the irreversible option', () => {
    expect(versionsPageSource).toContain('packages.yank(');
    expect(versionsPageSource).toContain('nextYankState(version.is_yanked)');
  });

  it('shows the state, so a yanked version is not silently indistinguishable', () => {
    // `is_yanked` was declared on the response type and rendered nowhere: the
    // operator could not tell a withdrawn version from a live one, which is
    // half of why deleting was the only usable answer.
    expect(versionsPageSource).toContain('version.is_yanked');
    expect(versionsPageSource).toContain("t('packages.yanked')");
  });

  it('offers unyank rather than a one-way button', () => {
    expect(versionsPageSource).toContain("t('packages.unyank')");
  });

  // `resolveTranslation` returns the KEY when a catalog has no entry for it, so
  // a label nobody translated reaches the operator as the literal string
  // `packages.yanked`. A control that announces a version's state cannot be
  // allowed to announce it in dot-notation.
  it.each(['versions', 'yank', 'unyank', 'yanked', 'yank_hint', 'yanked_hint', 'delete_confirm'])(
    'has a real label in both catalogs: packages.%s',
    (key) => {
      expect(en.packages).toHaveProperty(key);
      expect(zhCN.packages).toHaveProperty(key);
    }
  );
});
