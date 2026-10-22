import { request } from './_base.svelte';

export interface StartImportPayload {
  platform: 'github' | 'gitlab' | 'gitea' | 'git';
  source_url: string;
  target_owner: string;
  target_name?: string;
  auth_token?: string;
  import_repo?: boolean;
  import_issues?: boolean;
  import_pull_requests?: boolean;
  import_wiki?: boolean;
  import_releases?: boolean;
  import_labels?: boolean;
  import_milestones?: boolean;
}

export interface ImportTask {
  id: number;
  user_id: number;
  repo_id: number | null;
  platform: string;
  source_url: string;
  target_owner: string;
  target_name: string;
  status: string;
  progress: number;
  stage: string | null;
  error: string | null;
  stats: string | null;
  /** The receipt of a finished import; `null` until it has finished. */
  receipt?: ImportReceipt | null;
  created_at: string;
  updated_at: string;
}

// The shape `crates/rg-core/src/import/receipt.rs` serializes (`ImportReceipt`).
// Optional fields are the ones the server leaves out when they are empty.

export interface ObjectReceipt {
  kind: string;
  /** What the source listed; `null` where it has no listing to compare with. */
  source: number | null;
  imported: number;
  skipped?: number;
  skipped_reason?: string;
  not_imported: number;
  missing?: string[];
  missing_omitted?: number;
}

export interface RefKindReceipt {
  source: number;
  imported: number;
  missing?: string[];
  missing_omitted?: number;
  different?: string[];
  different_omitted?: number;
}

export interface DefaultBranchReceipt {
  name: string;
  source_sha: string | null;
  imported_sha: string | null;
  commits: number | null;
}

export interface RefsReceipt {
  unavailable?: string;
  branches?: RefKindReceipt;
  tags?: RefKindReceipt;
  default_branch?: DefaultBranchReceipt;
}

export interface CiNotRun {
  path: string;
  reason: string;
}

export interface CiReceipt {
  unavailable?: string;
  not_run: CiNotRun[];
}

export interface TemplateNotRead {
  path: string;
  reason: string;
}

export interface TemplatesReceipt {
  unavailable?: string;
  not_read: TemplateNotRead[];
  not_read_omitted?: number;
}

export interface ImportReceipt {
  objects: ObjectReceipt[];
  refs: RefsReceipt;
  not_transferable: string[];
  ci?: CiReceipt;
  templates?: TemplatesReceipt;
}

export const imports = {
  list: () =>
    request<ImportTask[]>('/imports'),
  start: (payload: StartImportPayload) =>
    request<ImportTask>('/imports', {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  get: (id: number) =>
    request<ImportTask>(`/imports/${id}`),
  remove: (id: number) =>
    request<void>(`/imports/${id}`, { method: 'DELETE' }),
};
