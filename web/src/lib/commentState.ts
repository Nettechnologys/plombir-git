import type { RepoPermission } from '$lib/api/client.svelte';
import { isRepoAdmin } from '$lib/repoPermission';

// Issue and review comments can be edited and deleted by their author or by a
// repository administrator (card_60961272e1ba); the server refuses everyone
// else with 403, so the pages offer the actions to exactly those two.

interface CommentStamps {
  created_at?: string | null;
  updated_at?: string | null;
}

/** An edit moves `updated_at` past `created_at`; nothing else touches it. */
export function isEdited(comment: CommentStamps): boolean {
  if (!comment.created_at || !comment.updated_at) return false;
  const created = Date.parse(comment.created_at);
  const updated = Date.parse(comment.updated_at);
  return Number.isFinite(created) && Number.isFinite(updated) && updated > created;
}

export function canModifyComment(
  comment: { author_id?: number | null },
  viewer: { id: number } | null | undefined,
  permission: RepoPermission | null | undefined,
): boolean {
  if (isRepoAdmin(permission)) return true;
  return Boolean(viewer) && comment.author_id != null && comment.author_id === viewer!.id;
}
