import type { RepoPermission } from '$lib/api/client.svelte';
import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
import { canWriteRepo, isRepoAdmin, loadViewerPermission } from '$lib/repoPermission';

/**
 * The viewer's level on the repository a page shows, kept current as the route
 * changes (card_3625a7b89abb). Call it while the component initialises.
 *
 * It is read on its own, not as part of the page's list load: a page whose list
 * is slow, or reloads after every mutation, must not hide its write controls
 * meanwhile or re-ask the server what the viewer may do. Until the answer
 * arrives — and for an anonymous reader or an unreadable repository — the level
 * is `null`, and the page offers no write action.
 */
export function viewerPermission(owner: () => string, repo: () => string) {
  let current = $state<RepoPermission | null>(null);
  // Whether the answer for the current repository has arrived. `current` is
  // `null` both while asking and for an anonymous reader; a page that has to
  // choose between "wait" and "you may not" reads this.
  let settled = $state(false);
  const requests = new LatestRepositoryRequestFence();

  $effect(() => {
    const expectedOwner = owner();
    const expectedRepo = repo();
    const claim = requests.begin(expectedOwner, expectedRepo);
    current = null;
    settled = false;
    void loadViewerPermission(expectedOwner, expectedRepo).then((permission) => {
      if (requests.owns(claim, owner(), repo())) {
        current = permission;
        settled = true;
      }
    });
  });

  return {
    get current() {
      return current;
    },
    get settled() {
      return settled;
    },
    get canWrite() {
      return canWriteRepo(current);
    },
    get isAdmin() {
      return isRepoAdmin(current);
    },
  };
}
