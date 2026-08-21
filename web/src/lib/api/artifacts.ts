import { downloadApiFile, request } from './_base.svelte';

/// One CI artifact as `GET /repos/{owner}/{repo}/pipelines/{id}/artifacts`
/// returns it — the shape of `rg_http::api::artifacts::ArtifactResponse`.
export interface CiArtifact {
  id: number;
  job_id: number;
  name: string;
  file_path: string;
  size: number;
  created_at: string;
  /** `null` when the repository's retention policy leaves the artifact forever. */
  expires_at: string | null;
  /** `null` for artifacts uploaded before digest tracking existed. */
  sha256: string | null;
}

export const artifacts = {
  list: (owner: string, repo: string, pipelineId: number) =>
    request<CiArtifact[]>(
      `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pipelines/${pipelineId}/artifacts`,
    ),
  // The bytes are behind `RepoRead`, so a bare `<a href>` would fetch them
  // without the bearer token and get a 404 on any private repository. This is
  // the same authenticated fetch-then-save path release assets use, and it
  // honours the `Content-Disposition` filename the server sends.
  download: (artifactId: number, filename: string) =>
    downloadApiFile(`/artifacts/${artifactId}/download`, filename || 'artifact'),
};
