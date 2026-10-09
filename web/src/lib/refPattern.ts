// Whether a branch or tag name falls under a protection rule's pattern.
//
// Mirrors `rg_git::protocol::receive_pack::ref_matches_rejection_pattern`, the
// one matcher both branch protection (`refs/heads/<branch_name>`) and tag
// protection (`refs/tags/<pattern>`) are enforced with: a pattern without `*`
// is an exact name, and each `*` matches any run of characters — `/` included,
// unlike a shell glob. The server stays the authority; this only lets a page
// say up front which refs it will refuse to delete.
export function refPatternMatches(name: string, pattern: string): boolean {
  if (!pattern.includes('*')) return name === pattern;
  const source = pattern
    .split('*')
    .map((part) => part.replace(/[.+?^${}()|[\]\\]/g, '\\$&'))
    .join('[\\s\\S]*');
  return new RegExp(`^${source}$`).test(name);
}

export function protectedBy(name: string, patterns: readonly string[]): boolean {
  return patterns.some((pattern) => refPatternMatches(name, pattern));
}
