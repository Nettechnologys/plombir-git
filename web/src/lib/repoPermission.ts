import { repos, type RepoPermission } from '$lib/api/client.svelte';

// Which actions a repository page offers (card_3625a7b89abb). The pages used
// to offer write controls to anyone signed in — and some to anonymous readers —
// because no repository response said what the viewer may do; the click then
// ended in a 403 the reader could do nothing about. `GET /repos/{owner}/{name}`
// now carries `viewer_permission`, and these are the only two questions a page
// asks of it. The server still decides every write on its own route.

export function canWriteRepo(permission: RepoPermission | null | undefined): boolean {
  return permission === 'write' || permission === 'admin';
}

export function isRepoAdmin(permission: RepoPermission | null | undefined): boolean {
  return permission === 'admin';
}

/**
 * The viewer's level, or `null` for an anonymous reader and for a repository
 * that could not be read — in both cases the page offers no write action
 * rather than one the server may refuse.
 */
export async function loadViewerPermission(owner: string, repo: string): Promise<RepoPermission | null> {
  try {
    const repository = await repos.get(owner, repo);
    return repository?.viewer_permission ?? null;
  } catch {
    return null;
  }
}
