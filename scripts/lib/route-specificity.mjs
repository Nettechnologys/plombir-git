/** How the router ranks a segment: static beats a placeholder beats a catch-all tail. */
const segmentRank = (segment) => {
  if (segment.startsWith('{*')) return 0;
  if (segment.startsWith('{')) return 1;
  return 2;
};

/**
 * Whether `rivalUrl` would win some concrete path that `routeUrl` also matches.
 *
 * Axum selects the most specific path registration before it consults that
 * registration's method table. Method is therefore deliberately absent here:
 * a request that reaches a static sibling and gets 405 does not fall through to
 * a placeholder or catch-all route that happens to accept the verb.
 */
export function canOutrank(rivalUrl, routeUrl) {
  const rival = rivalUrl.split('/');
  const route = routeUrl.split('/');
  const rivalTail = rival.findIndex((segment) => segment.startsWith('{*'));
  const routeTail = route.findIndex((segment) => segment.startsWith('{*'));
  if (rivalTail === -1 && routeTail === -1 && rival.length !== route.length) return false;
  if (rivalTail === -1 && routeTail !== -1 && rival.length < routeTail) return false;
  if (routeTail === -1 && rivalTail !== -1 && route.length < rivalTail) return false;

  const limit = Math.min(
    rivalTail === -1 ? rival.length : rivalTail,
    routeTail === -1 ? route.length : routeTail,
  );
  for (let i = 0; i < limit; i += 1) {
    const here = segmentRank(rival[i]);
    const there = segmentRank(route[i]);
    if (here === 2 && there === 2 && rival[i] !== route[i]) return false;
    if (here !== there) return here > there;
  }

  if (routeTail === -1) return false;
  if (rivalTail === -1) return rival.length > routeTail;
  return rivalTail > routeTail;
}

/** The registrations that would take a concrete path away from `routeUrl`. */
export function outrankingRoutes(routeUrl, routeUrls) {
  if (!routeUrl) return [];
  return [...new Set(routeUrls)]
    .filter((url) => url && url !== routeUrl && canOutrank(url, routeUrl));
}
