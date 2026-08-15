import {
  downloadApiFile,
  getToken,
  request,
  qs,
  withApiBase,
  type PaginatedResponse,
} from './_base.svelte';

export interface ReleaseAsset {
  id: number;
  release_id: number;
  filename: string;
  size: number;
  content_type: string;
  download_count: number;
  /** `null` once the account that uploaded the asset has been deleted. */
  uploader_id: number | null;
  created_at: string;
  sha256: string | null;
}

export interface ReleaseAssetUploadProgress {
  loaded: number;
  total: number | null;
  percent: number | null;
}

function contentDispositionAttachment(filename: string): string {
  return `attachment; filename*=UTF-8''${encodeURIComponent(filename || 'package')}`;
}

function uploadErrorMessage(status: number, responseText: string): string {
  let body: any = {};
  try {
    body = JSON.parse(responseText);
  } catch {
    // A proxy can return a non-JSON error page. The status remains useful.
  }

  return (
    (body?.error && typeof body.error === 'object' ? body.error.message : body?.error) ||
    body?.message ||
    `HTTP ${status}`
  );
}

function uploadReleaseAsset(
  path: string,
  file: File,
  onProgress?: (progress: ReleaseAssetUploadProgress) => void,
): Promise<ReleaseAsset> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', withApiBase(path));
    xhr.withCredentials = true;
    xhr.timeout = 300_000;
    xhr.setRequestHeader('Content-Type', file.type || 'application/octet-stream');
    xhr.setRequestHeader('Content-Disposition', contentDispositionAttachment(file.name || 'asset'));

    const token = getToken();
    if (token) xhr.setRequestHeader('Authorization', `Bearer ${token}`);

    xhr.upload.onprogress = (event) => {
      const total = event.lengthComputable && event.total > 0 ? event.total : null;
      onProgress?.({
        loaded: event.loaded,
        total,
        percent: total === null ? null : Math.min(100, Math.round((event.loaded / total) * 100)),
      });
    };

    xhr.onload = () => {
      if (xhr.status < 200 || xhr.status >= 300) {
        reject(new Error(uploadErrorMessage(xhr.status, xhr.responseText)));
        return;
      }

      try {
        const asset = JSON.parse(xhr.responseText) as ReleaseAsset;
        onProgress?.({ loaded: file.size, total: file.size, percent: 100 });
        resolve(asset);
      } catch {
        reject(new Error('Release asset upload returned an invalid response'));
      }
    };
    xhr.onerror = () => reject(new Error('Release asset upload failed because of a network error'));
    xhr.ontimeout = () => reject(new Error('Release asset upload timed out'));
    xhr.send(file);
  });
}

export const releases = {
  list: (owner: string, repo: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/releases${qs({ page, per_page: perPage })}`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/releases/${id}`),
  create: (owner: string, repo: string, data: { tag_name: string; title: string; body?: string; target_commitish?: string; is_draft?: boolean; is_prerelease?: boolean }) =>
    request<any>(`/repos/${owner}/${repo}/releases`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  update: (owner: string, repo: string, id: number, data: { title?: string; body?: string; is_draft?: boolean; is_prerelease?: boolean }) =>
    request<any>(`/repos/${owner}/${repo}/releases/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/releases/${id}`, { method: 'DELETE' }),
  listAssets: (owner: string, repo: string, releaseId: number) =>
    request<ReleaseAsset[]>(`/repos/${owner}/${repo}/releases/${releaseId}/assets`),
  uploadAsset: (
    owner: string,
    repo: string,
    releaseId: number,
    file: File,
    onProgress?: (progress: ReleaseAssetUploadProgress) => void,
  ) => uploadReleaseAsset(`/repos/${owner}/${repo}/releases/${releaseId}/assets`, file, onProgress),
  downloadAsset: (owner: string, repo: string, assetId: number, filename: string) =>
    downloadApiFile(
      `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/releases/assets/${assetId}/download`,
      filename || 'asset'
    ),
  deleteAsset: (owner: string, repo: string, assetId: number) =>
    request<void>(`/repos/${owner}/${repo}/releases/assets/${assetId}`, { method: 'DELETE' }),
};
