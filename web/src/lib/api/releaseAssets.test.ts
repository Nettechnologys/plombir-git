import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  downloadApiFile: vi.fn(),
  getToken: vi.fn(() => 'test-token'),
  request: vi.fn(),
  qs: vi.fn(() => ''),
  withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import releasePageSource from '../../routes/[owner]/[repo]/releases/+page.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { releases, type ReleaseAsset } from './releases';
import releasesSource from './releases.ts?raw';

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
    filename: 'forgekeep.zip',
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
    const file = new File(['payload'], 'forgekeep.zip', { type: 'application/zip' });
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
      'Content-Disposition': "attachment; filename*=UTF-8''forgekeep.zip",
    });
    expect(xhr.body).toBe(file);

    xhr.reportProgress(3, 7);
    expect(progress).toHaveBeenLastCalledWith({ loaded: 3, total: 7, percent: 43 });

    xhr.respond(201, asset());
    await expect(result).resolves.toEqual(asset());
    expect(progress).toHaveBeenLastCalledWith({ loaded: 7, total: 7, percent: 100 });
  });

  it('surfaces the backend error instead of reporting a successful upload', async () => {
    const result = releases.uploadAsset('alice', 'demo', 7, new File(['x'], 'bad.bin'));
    FakeXmlHttpRequest.instances[0].respond(413, { error: { message: 'asset is too large' } });

    await expect(result).rejects.toThrow('asset is too large');
  });
});

describe('release asset production wiring', () => {
  it('connects list, upload, authenticated download, and confirmed delete to the release page', () => {
    for (const caller of ['listAssets', 'uploadAsset', 'downloadAsset', 'deleteAsset']) {
      expect(releasePageSource).toContain(`releases.${caller}(`);
    }
    expect(releasePageSource).toContain('class="asset-upload-progress"');
    expect(releasePageSource).toContain('confirmDeleteAssetId === asset.id');
  });

  it('does not keep the unused metadata getter or raw download URL duplicate', () => {
    expect(releasesSource).not.toMatch(/\bgetAsset\s*:/);
    expect(releasesSource).not.toMatch(/\bassetDownloadUrl\s*:/);
    expect(releasePageSource).not.toContain('releases.assetDownloadUrl(');
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
