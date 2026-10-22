// The platform an import came from, as a reader names it, from the `service`
// the server stores beside an imported byline.

const PLATFORMS: Record<string, string> = { github: 'GitHub', gitlab: 'GitLab', gitea: 'Gitea' };

/**
 * `github` / `gitlab` is github.com / gitlab.com; any other server is
 * `github:ghe.example.com`, and its host is worth showing. Gitea and Forgejo
 * have no public instance, so theirs always carries one.
 */
export function importedPlatform(service: string): string {
  const [kind, ...host] = service.split(':');
  const name = PLATFORMS[kind] ?? kind;
  return host.length ? `${name} · ${host.join(':')}` : name;
}
