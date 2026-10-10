import {
  downloadApiFile,
  getToken,
  request,
  qs,
  withApiBase,
  type PaginatedResponse,
} from './_base.svelte';
import { repoPath } from './repoPath';

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

/// The DSSE envelope `GET .../assets/{id}/attestation` returns.
///
/// Deliberately shallow: the page shows *that* an asset is signed and what the
/// signature says about itself, and hands the payload to whoever wants to run
/// their own verifier. Re-typing the in-toto statement here would be a second
/// copy of a schema the server already owns.
export interface AttestationEnvelope {
  payloadType: string;
  payload: string;
  signatures: Array<{ keyid?: string; sig: string }>;
}

/// The three answers to "does this signature still hold for the bytes on disk?"
///
/// - `verified` — signature, digest binding and predicate all hold.
/// - `mismatch` — the bytes are not the bytes that were signed. The single most
///   important thing this feature can say, and it arrives with a 200.
/// - `undeterminable` — the server could not reach a verdict at all (a
///   predicate type it has no verifier for, an unreadable envelope, a rotated
///   key). It observes nothing about the asset, so it must never be drawn as
///   the `mismatch` alarm (card_4579598691ce).
export type AttestationStatus = 'verified' | 'mismatch' | 'undeterminable';

/// The answer to "does this signature still hold for the bytes on disk?"
///
/// A non-verified `status` is a **report**, not an error: the request
/// succeeded, and which of the two non-verified answers came back is exactly
/// the thing a boolean could not carry.
export interface AttestationReport {
  status: AttestationStatus;
  /** Why it did not verify. `null` when it verified. */
  reason: string | null;
  predicate_type: string | null;
  keyid: string | null;
  /** SHA-256 recomputed from the stored bytes at verification time. */
  asset_sha256: string;
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
    request<PaginatedResponse<any>>(`${repoPath(owner, repo)}/releases${qs({ page, per_page: perPage })}`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`${repoPath(owner, repo)}/releases/${id}`),
  create: (owner: string, repo: string, data: { tag_name: string; title: string; body?: string; target_commitish?: string; is_draft?: boolean; is_prerelease?: boolean }) =>
    request<any>(`${repoPath(owner, repo)}/releases`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  update: (owner: string, repo: string, id: number, data: { title?: string; body?: string; is_draft?: boolean; is_prerelease?: boolean }) =>
    request<any>(`${repoPath(owner, repo)}/releases/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`${repoPath(owner, repo)}/releases/${id}`, { method: 'DELETE' }),
  listAssets: (owner: string, repo: string, releaseId: number) =>
    request<ReleaseAsset[]>(`${repoPath(owner, repo)}/releases/${releaseId}/assets`),
  uploadAsset: (
    owner: string,
    repo: string,
    releaseId: number,
    file: File,
    onProgress?: (progress: ReleaseAssetUploadProgress) => void,
  ) => uploadReleaseAsset(`${repoPath(owner, repo)}/releases/${releaseId}/assets`, file, onProgress),
  downloadAsset: (owner: string, repo: string, assetId: number, filename: string) =>
    downloadApiFile(
      `${repoPath(owner, repo)}/releases/assets/${assetId}/download`,
      filename || 'asset'
    ),
  deleteAsset: (owner: string, repo: string, assetId: number) =>
    request<void>(`${repoPath(owner, repo)}/releases/assets/${assetId}`, { method: 'DELETE' }),

  /// Release-asset provenance (card_5e52392a0274).
  ///
  /// The three endpoints existed, the README explained what key rotation does
  /// to them, and `FEATURE_INVENTORY.md` marked the feature shipped — while the
  /// word `attestation` appeared nowhere in `web/src`, so the only way to sign
  /// or check anything was `curl` with a token.
  ///
  /// `get` and `verify` both answer `404` when the instance has the feature off
  /// AND when the asset was never signed. The page must not conflate those, so
  /// it reads `attestation_enabled` from `GET /instance` first and only then
  /// treats a 404 as "unsigned".
  attestation: {
    /** Sign the asset with the instance key. `RepoWrite`. */
    sign: (owner: string, repo: string, assetId: number) =>
      request<AttestationEnvelope>(
        `${repoPath(owner, repo)}/releases/assets/${assetId}/attestation`,
        { method: 'POST' },
      ),
    /** Read the stored DSSE envelope. `RepoRead`. */
    get: (owner: string, repo: string, assetId: number) =>
      request<AttestationEnvelope>(
        `${repoPath(owner, repo)}/releases/assets/${assetId}/attestation`,
      ),
    /** Re-check the signature against the asset's current bytes. `RepoRead`. */
    verify: (owner: string, repo: string, assetId: number) =>
      request<AttestationReport>(
        `${repoPath(owner, repo)}/releases/assets/${assetId}/attestation/verify`,
        { method: 'POST' },
      ),
  },
};
