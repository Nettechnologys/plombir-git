import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	qs: vi.fn(() => ''),
	request: vi.fn(),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import VersionsPage from '../../routes/[owner]/[repo]/packages/[format]/[...name]/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import {
  buildPackageYankPayload,
  nextYankState,
  packageYankPath,
  type PackageVersionRef,
} from './packageYank';
import { packages as packageClient } from './packages';
import { setTestPage } from '../test/app';
import { packages as routePackages, resetTestClient } from '../test/client';
import { button, click, element, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const liveVersion = { version: '1.2.3', is_yanked: false, files: [] };
const yankedVersion = { ...liveVersion, is_yanked: true };

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	setTestPage('/acme/tools/packages/npm/widget', {
		owner: 'acme',
		repo: 'tools',
		format: 'npm',
		name: 'widget',
	});
	routePackages.get.mockResolvedValue({
		name: 'widget',
		description: 'A package',
		latest_version: '1.2.3',
		created_at: '2026-08-15T12:00:00Z',
	});
	routePackages.getVersions
		.mockResolvedValueOnce({ versions: [liveVersion] })
		.mockResolvedValue({ versions: [yankedVersion] });
	routePackages.downloadUrl.mockReturnValue('/download');
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

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
		packageClient.yank('acme', 'tools', 'npm', 'widget', '1.2.3', true);

		expect(base.request).toHaveBeenCalledWith(
			'/repos/acme/tools/packages/npm/widget/1.2.3/yank',
			{ method: 'PATCH', body: JSON.stringify({ yank: true }) },
		);
	});

	it('toggles the rendered version and keeps its yanked state visible', async () => {
		// `is_yanked` was declared on the response type and rendered nowhere: the
		// operator could not tell a withdrawn version from a live one, which is
		// half of why deleting was the only usable answer.
		rendered = await renderComponent(VersionsPage);
		await click(button(rendered.container, 'Yank'));

		expect(routePackages.yank).toHaveBeenCalledWith(
			'acme',
			'tools',
			'npm',
			'widget',
			'1.2.3',
			true,
		);
		expect(element(rendered.container, '.version-card').classList.contains('yanked')).toBe(true);
		expect(rendered.container.textContent).toContain('Yanked');
		expect(button(rendered.container, 'Unyank')).toBeTruthy();

		await click(button(rendered.container, 'Unyank'));
		expect(routePackages.yank).toHaveBeenLastCalledWith(
			'acme',
			'tools',
			'npm',
			'widget',
			'1.2.3',
			false,
		);
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
