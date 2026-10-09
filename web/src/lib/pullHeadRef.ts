// How the UI names the head of a pull request, and the compare page that shows
// it before the pull request exists.
//
// `POST /repos/{owner}/{name}/pulls` takes `head` as either a bare branch of
// the target repository or `"<owner>:<branch>"` for a branch of one of its
// forks (`CreatePrRequest` in `crates/rg-http/src/api/pulls.rs`, resolved by
// `rg_core::pull_request::resolve_head_ref`). The server looks the fork up as
// `<owner>/<target repo name>` and requires it to be a direct fork of the
// target, so the owner is the only thing the prefix carries.
//
// A git ref name cannot contain `:` (git-check-ref-format), so splitting on the
// first `:` is unambiguous; nor can it contain `..`, so the first `...` in a
// compare spec always separates base from head.

export type HeadRef = {
  /** Namespace of the fork holding the branch; `null` for the target itself. */
  owner: string | null;
  branch: string;
};

/**
 * The wire form of a pull-request head.
 *
 * A head in the target repository's own namespace is sent bare: the server
 * accepts `owner:branch` naming the target too, but a bare branch is the form
 * every same-repo client already sends.
 */
export function formatHeadRef(head: HeadRef, targetOwner?: string): string {
  if (!head.owner || head.owner === targetOwner) return head.branch;
  return `${head.owner}:${head.branch}`;
}

export function parseHeadRef(raw: string): HeadRef {
  const colon = raw.indexOf(':');
  if (colon <= 0) return { owner: null, branch: colon === 0 ? raw.slice(1) : raw };
  return { owner: raw.slice(0, colon), branch: raw.slice(colon + 1) };
}

export type CompareSpec = {
  /** `null` when the spec names only a head: compare against the default branch. */
  base: string | null;
  head: HeadRef;
};

/** Read `base...head` (or a bare `head`) from the compare route's rest param. */
export function parseCompareSpec(spec: string): CompareSpec | null {
  const trimmed = spec.replace(/^\/+|\/+$/g, '');
  if (!trimmed) return null;
  const separator = trimmed.indexOf('...');
  if (separator === -1) {
    const head = parseHeadRef(trimmed);
    return head.branch ? { base: null, head } : null;
  }
  const base = trimmed.slice(0, separator);
  const head = parseHeadRef(trimmed.slice(separator + 3));
  if (!base || !head.branch) return null;
  return { base, head };
}

// Branch names keep their `/` as path separators (the route is a rest param)
// and the head keeps a readable `:`; everything else is percent-encoded.
function encodeRefForPath(ref: string): string {
  return encodeURIComponent(ref).replaceAll('%2F', '/').replaceAll('%3A', ':');
}

export function compareHref(owner: string, repo: string, base: string, head: HeadRef): string {
  const headRef = head.owner ? `${head.owner}:${head.branch}` : head.branch;
  return `/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/compare/${encodeRefForPath(base)}...${encodeRefForPath(headRef)}`;
}

/** The pull-request list with its create form opened and prefilled. */
export function newPullHref(owner: string, repo: string, base: string, head: HeadRef): string {
  const params = new URLSearchParams({ new: '1', base, head: formatHeadRef(head, owner) });
  return `/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls?${params.toString()}`;
}
