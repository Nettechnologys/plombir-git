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
import { releases, type ReleaseAsset } from './releases';
import { setTestPage } from '../test/app';
import {
	instance,
	releases as routeReleases,
	resetTestClient,
	repos as viewerRepos,
} from '../test/client';
import { click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

class FakeXmlHttpRequest {
  static instances: FakeXmlHttpRequest[] = [];

  readonly upload: { onprogress: ((event: ProgressEvent) => void) | null } = { onprogress: null };
  readonly headers: Record<string, string> = {};
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  ontimeout: (() => void) | null = null;
  method = '';
  url = '';
  body: unknown;
  status = 0;
  responseText = '';
  timeout = 0;
  withCredentials = false;

  constructor() {
    FakeXmlHttpRequest.instances.push(this);
  }

  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }

  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }

  send(body: unknown) {
    this.body = body;
  }

  reportProgress(loaded: number, total: number) {
    this.upload.onprogress?.(new ProgressEvent('progress', { lengthComputable: true, loaded, total }));
  }

  respond(status: number, body: unknown) {
    this.status = status;
    this.responseText = typeof body === 'string' ? body : JSON.stringify(body);
    this.onload?.();
  }
}

function asset(): ReleaseAsset {
  return {
    id: 11,
    release_id: 7,
    filename: 'plombir-git.zip',
    size: 7,
    content_type: 'application/zip',
    download_count: 0,
    uploader_id: 3,
    created_at: '2026-08-15T12:00:00Z',
    sha256: null,
  };
}

describe('release asset upload transport', () => {
  beforeEach(() => {
    FakeXmlHttpRequest.instances = [];
    vi.stubGlobal('XMLHttpRequest', FakeXmlHttpRequest);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it('sends the authenticated file request and reports measured progress', async () => {
    const file = new File(['payload'], 'plombir-git.zip', { type: 'application/zip' });
    const progress = vi.fn();
    const result = releases.uploadAsset('alice', 'demo', 7, file, progress);
    const xhr = FakeXmlHttpRequest.instances[0];

    expect(xhr.method).toBe('POST');
    expect(xhr.url).toBe('/api/v1/repos/alice/demo/releases/7/assets');
    expect(xhr.withCredentials).toBe(true);
    expect(xhr.timeout).toBe(300_000);
    expect(xhr.headers).toMatchObject({
      Authorization: 'Bearer test-token',
      'Content-Type': 'application/zip',
      'Content-Disposition': "attachment; filename*=UTF-8''plombir-git.zip",
    });
    expect(xhr.body).toBe(file);

    xhr.reportProgress(3, 7);
    expect(progress).toHaveBeenLastCalledWith({ loaded: 3, total: 7, percent: 43 });

    xhr.respond(201, asset());
    await expect(result).resolves.toEqual(asset());
    expect(progress).toHaveBeenLastCalledWith({ loaded: 7, total: 7, percent: 100 });
  });

  // The wording is what `rg_http::error::transport_refusal_envelope` really
  // writes: the ceiling is refused above every handler, by a layer, and until
  // that layer existed this fixture was checking an envelope the server never
  // sent on this path — the upload dialog could only say `HTTP 413`
  // (card_f71fddfcb23e).
  it('surfaces the backend error instead of reporting a successful upload', async () => {
    const result = releases.uploadAsset('alice', 'demo', 7, new File(['x'], 'bad.bin'));
    FakeXmlHttpRequest.instances[0].respond(413, {
      error: {
        code: 'PAYLOAD_TOO_LARGE',
        message: "request body exceeds this endpoint's limit of 512 MiB",
      },
    });

    await expect(result).rejects.toThrow("request body exceeds this endpoint's limit of 512 MiB");
  });

  // A reverse proxy in front of the instance enforces its own ceiling and
  // answers in its own words — an HTML page, not the API envelope. That path
  // is the one case where the status is all there is, and it must degrade
  // rather than throw a parse error over the upload.
  it('falls back to the status when a proxy answers outside the API envelope', async () => {
    const result = releases.uploadAsset('alice', 'demo', 7, new File(['x'], 'bad.bin'));
    FakeXmlHttpRequest.instances[0].respond(
      413,
      '<html><body>413 Request Entity Too Large</body></html>',
    );

    await expect(result).rejects.toThrow('HTTP 413');
  });
});

beforeEach(() => {
	resetTestClient();
	// The write controls follow `viewer_permission` (card_3625a7b89abb).
	viewerRepos.get.mockResolvedValue({ viewer_permission: 'write' });
	setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
	instance.get.mockResolvedValue({ attestation_enabled: false });
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
	routeReleases.listAssets.mockResolvedValue([asset()]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('release asset production wiring', () => {
	it('renders authenticated download and confirmed deletion controls', async () => {
		rendered = await renderComponent(ReleasesPage);
		expect(routeReleases.listAssets).toHaveBeenCalledWith('alice', 'demo', 7);

		await click(element(rendered.container, '.asset-link'));
		expect(routeReleases.downloadAsset).toHaveBeenCalledWith(
			'alice',
			'demo',
			11,
			'plombir-git.zip',
		);

		await click(element(rendered.container, '.asset-delete'));
		expect(rendered.container.textContent).toContain('Delete this asset?');
		await click(element(rendered.container, '.asset-delete-confirm .btn-danger'));
		expect(routeReleases.deleteAsset).toHaveBeenCalledWith('alice', 'demo', 11);
		expect(rendered.container.querySelector('.asset-row')).toBeNull();
	});

	it('renders measured upload progress and the newly uploaded asset', async () => {
		let finishUpload!: (asset: ReleaseAsset) => void;
		routeReleases.uploadAsset.mockImplementation(
			(_owner: string, _repo: string, _releaseId: number, _file: File, onProgress: (progress: any) => void) => {
				onProgress({ loaded: 3, total: 7, percent: 43 });
				return new Promise<ReleaseAsset>((resolve) => {
					finishUpload = resolve;
				});
			},
		);
		rendered = await renderComponent(ReleasesPage);
		const file = new File(['new'], 'new.zip', { type: 'application/zip' });
		const fileInput = element<HTMLInputElement>(rendered.container, '.asset-upload input');
		Object.defineProperty(fileInput, 'files', { configurable: true, value: [file] });
		fileInput.dispatchEvent(new Event('change', { bubbles: true }));
		await settle();

		expect(routeReleases.uploadAsset).toHaveBeenCalledWith(
			'alice',
			'demo',
			7,
			file,
			expect.any(Function),
		);
		expect(rendered.container.textContent).toContain('Uploading asset... 43%');

		finishUpload({ ...asset(), id: 12, filename: 'new.zip' });
		await settle();
		expect(rendered.container.textContent).toContain('new.zip');
	});

	it('does not keep the unused metadata getter or raw download URL duplicate', () => {
		expect(releases).not.toHaveProperty('getAsset');
		expect(releases).not.toHaveProperty('assetDownloadUrl');
	});

  it.each([
    'assets',
    'asset_upload',
    'asset_uploading',
    'asset_upload_progress',
    'asset_delete',
    'asset_delete_confirm',
    'asset_downloads',
    'asset_load_failed',
  ])('has a real label in both catalogs: releases.%s', (key) => {
    expect(en.releases).toHaveProperty(key);
    expect(zhCN.releases).toHaveProperty(key);
  });
});
