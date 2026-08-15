// Yank is the registry's *reversible* withdrawal, and until now the product
// only offered the irreversible one.
//
// `DELETE .../{version}` drops the version row, and with it the `author_id`
// that attributes the publication forever. `PATCH .../{version}/yank` instead
// flips a flag: the version stops being offered to installers, the row and its
// attribution stay, and both directions are written to the audit log as
// `package.yank` / `package.unyank`. cargo drives its own yank through the
// registry protocol, so for npm / maven / composer / generic this endpoint is
// the only way to reach the soft operation at all.
//
// The three pieces below are the whole request the version list sends, kept
// out of `packages.ts` so a test can build the same call the page builds
// without pulling in the `$state`-bearing fetch client.

/** The version a yank control acts on, addressed the way the API addresses it. */
export interface PackageVersionRef {
  owner: string;
  repo: string;
  pkg_type: string;
  pkg_name: string;
  version: string;
}

/** The body `yank_version` reads — a target state, not a direction. */
export interface PackageYankPayload {
  yank: boolean;
}

/**
 * The state the toggle moves a version *to*.
 *
 * The whole reason this is a named function: a control that sends a constant
 * `true` looks identical on screen and can only ever yank, which leaves the
 * operation one-way again — the defect this module exists to close. A version
 * whose flag the server did not send is treated as live, so the first press
 * yanks it.
 */
export function nextYankState(isYanked: boolean | undefined): boolean {
  return !isYanked;
}

/** Body for setting a version's yank state to `yank`. */
export function buildPackageYankPayload(yank: boolean): PackageYankPayload {
  return { yank };
}

/**
 * Path of the yank endpoint for one version.
 *
 * Every segment is encoded the same way its siblings in `packages.ts` encode
 * theirs, so a scoped npm name or a maven coordinate addresses the same
 * version here as it does for download and delete.
 */
export function packageYankPath(ref: PackageVersionRef): string {
  const pkgType = encodeURIComponent(ref.pkg_type);
  const pkgName = encodeURIComponent(ref.pkg_name);
  const version = encodeURIComponent(ref.version);
  return `/repos/${ref.owner}/${ref.repo}/packages/${pkgType}/${pkgName}/${version}/yank`;
}
