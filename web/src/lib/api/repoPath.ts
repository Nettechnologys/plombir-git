/** Keep untrusted repository identifiers inside their own URL segments. */
export function pathSegment(value: string): string {
  if (!value || value === '.' || value === '..') {
    throw new Error('Invalid URL path segment');
  }
  return encodeURIComponent(value);
}

export function repoOwnerPath(owner: string): string {
  return ['/repos', pathSegment(owner)].join('/');
}

export function repoPath(owner: string, repo: string, ...rest: string[]): string {
  return [repoOwnerPath(owner), pathSegment(repo), ...rest.map(pathSegment)].join('/');
}
