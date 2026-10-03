import { packageFormatLabel, packageFormatUsesGenericFallback } from './packageFormats';

/**
 * The install snippet shown next to a package, in the two parts a client
 * actually needs: point the tool at THIS registry, then install from it.
 *
 * The setup half is the load-bearing one. A bare `gem install foo` or
 * `npm install foo` resolves against the public registry, so for a name that is
 * free upstream the copied command 404s, and for a name that is taken it
 * silently installs somebody else's code — worse than an error. Every snippet
 * here therefore names `{origin}/api/v1/repos/{owner}/{repo}/packages/{format}`
 * or refuses to pretend.
 */
export interface PackageInstallSnippet {
  /** One-time source configuration, when the tool needs one. */
  setup?: string;
  /** The install command, once the source is configured. */
  install?: string;
  /**
   * Why there is no command, when the format has no client protocol here.
   * A snippet the server cannot serve is not a convenience, it is a wrong
   * answer the user has no way to check.
   */
  unavailable?: string;
}

export interface PackageInstallTarget {
  format: string;
  owner: string;
  repo: string;
  name: string;
  /** Omitted on the list pages, where a package has no version in hand. */
  version?: string;
  /** `page.url.origin` — the address the browser reached this instance at. */
  origin: string;
}

/**
 * The registry root for one repository and one format — the prefix every
 * protocol route in `routes.rs` hangs off.
 */
export function packageRegistryRoot(target: PackageInstallTarget): string {
  const { origin, owner, repo, format } = target;
  return `${origin.replace(/\/+$/, '')}/api/v1/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/packages/${encodeURIComponent(format.toLowerCase())}`;
}

/** `name@version` / `name==version` etc., or just the name when no version is known. */
function withVersion(name: string, version: string | undefined, separator: string): string {
  return version ? `${name}${separator}${version}` : name;
}

function versionFlag(flag: string, version: string | undefined): string {
  return version ? ` ${flag} ${version}` : '';
}

export function packageInstallSnippet(target: PackageInstallTarget): PackageInstallSnippet {
  const format = target.format.toLowerCase();
  const { owner, repo, name, version } = target;
  const root = packageRegistryRoot(target);

  switch (format) {
    case 'cargo':
      // Cargo resolves an alternative registry through its sparse index, and
      // the index root is the whole URL with the `sparse+` scheme prefix.
      return {
        setup: `# .cargo/config.toml\n[registries.plombir-git]\nindex = "sparse+${root}/index/"`,
        install: `cargo add ${withVersion(name, version, '@')} --registry plombir-git`,
      };
    case 'npm':
      return {
        install: `npm install ${withVersion(name, version, '@')} --registry ${root}`,
      };
    case 'pypi':
      return {
        install: `pip install ${withVersion(name, version, '==')} --index-url ${root}/simple/`,
      };
    case 'maven':
      return {
        setup: `<!-- pom.xml -->\n<repositories>\n  <repository>\n    <id>plombir-git</id>\n    <url>${root}</url>\n  </repository>\n</repositories>`,
        install: `<dependency>\n  <groupId>...</groupId>\n  <artifactId>${name}</artifactId>\n  <version>${version || '...'}</version>\n</dependency>`,
      };
    case 'docker': {
      // The OCI surface is served at `/v2/` on the host itself, not under the
      // package API path — an image reference is `host/owner/repo:tag`.
      const host = hostOf(target.origin);
      return {
        setup: `docker login ${host}`,
        install: `docker pull ${host}/${owner}/${repo}:${version || 'latest'}`,
      };
    }
    case 'nuget':
      return {
        install: `dotnet add package ${name}${versionFlag('--version', version)} --source ${root}/index.json`,
      };
    case 'rubygems':
      return {
        install: `gem install ${name}${versionFlag('--version', version)} --source ${root}`,
      };
    case 'helm':
      return {
        setup: `helm repo add plombir-git ${root}\nhelm repo update`,
        install: `helm install my-release plombir-git/${name}${versionFlag('--version', version)}`,
      };
    case 'composer':
      return {
        setup: `composer config repositories.plombir-git composer ${root}`,
        install: `composer require ${withVersion(name, version, ':')}`,
      };
    default:
      // `go` lands here on purpose: the GOPROXY protocol (`@v/list`,
      // `@v/{version}.info|.mod|.zip`) has no routes on this server, so the
      // `go get` line this page used to print pointed at a surface that does
      // not exist. Anything else served by the generic fallback is in the same
      // position — the files are downloadable, there is just no client
      // protocol to hand them out through.
      return {
        unavailable: packageFormatUsesGenericFallback(format)
          ? `${packageFormatLabel(format)} has no client protocol on this instance — download the files below directly.`
          : `No install command is defined for ${packageFormatLabel(format)}.`,
      };
  }
}

function hostOf(origin: string): string {
  try {
    return new URL(origin).host;
  } catch {
    return origin.replace(/^https?:\/\//, '').replace(/\/+$/, '');
  }
}

/** The snippet as one copyable block: setup first, then the install command. */
export function packageInstallText(snippet: PackageInstallSnippet): string {
  return [snippet.setup, snippet.install].filter(Boolean).join('\n\n') || snippet.unavailable || '';
}
