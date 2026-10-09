import type { RepoSettingsPatch } from './repos';

/** What the repository settings form edits, as the inputs hold it. */
export interface RepoSettingsFormState {
  name: string;
  description: string;
  isPrivate: boolean;
  defaultBranch: string;
}

interface RepoSettingsSource {
  name: string;
  description: string | null;
  is_private: boolean;
  default_branch: string;
}

export function repoSettingsFormState(repository: RepoSettingsSource): RepoSettingsFormState {
  return {
    name: repository.name,
    description: repository.description ?? '',
    isPrivate: repository.is_private,
    defaultBranch: repository.default_branch,
  };
}

/**
 * The `PATCH /repos/{owner}/{name}` body for what changed, and nothing else
 * (card_3625a7b89abb): a key the form left alone is not sent, so saving the
 * description cannot also re-assert a visibility or a default branch someone
 * else changed meanwhile. An emptied description is sent as `null`, which the
 * server reads as "clear it".
 */
export function buildRepoSettingsPatch(
  original: RepoSettingsSource,
  form: RepoSettingsFormState,
): RepoSettingsPatch {
  const patch: RepoSettingsPatch = {};
  if (form.description !== (original.description ?? '')) {
    patch.description = form.description.trim() === '' ? null : form.description;
  }
  if (form.isPrivate !== original.is_private) patch.is_private = form.isPrivate;
  const defaultBranch = form.defaultBranch.trim();
  if (defaultBranch !== '' && defaultBranch !== original.default_branch) patch.default_branch = defaultBranch;
  const name = form.name.trim();
  if (name !== '' && name !== original.name) patch.name = name;
  return patch;
}
