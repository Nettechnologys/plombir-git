import { request } from './_base.svelte';

/** Bytes and rows one repository holds, per store. */
export interface RepoStorageUsage {
  lfs_bytes: number;
  lfs_objects: number;
  release_bytes: number;
  release_assets: number;
  attachment_bytes: number;
  ci_cache_bytes: number;
  ci_cache_entries: number;
  package_bytes: number;
  package_files: number;
  oci_bytes: number;
  oci_blobs: number;
  /** Every store summed; compare against `limits.repo_quota_bytes`. */
  total_bytes: number;
}

/** The `[limits]` ceilings the server enforces, in bytes and entries. */
export interface RepoStorageLimits {
  repo_quota_bytes: number;
  oci_blob_max_bytes: number;
  ci_cache_max_entries_per_repo: number;
  release_assets_max_per_release: number;
}

export interface RepoStorageReport {
  usage: RepoStorageUsage;
  limits: RepoStorageLimits;
}

export const repoStorage = {
  get: (owner: string, repo: string) =>
    request<RepoStorageReport>(`/repos/${owner}/${repo}/storage`),
};
