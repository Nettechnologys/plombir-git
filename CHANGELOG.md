# Changelog

This project records user-facing changes here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Public security reporting and contributor guidance.
- CI secrets can be scoped to an environment. Only jobs that declare the
  environment read them, after its approval when it is protected, and an
  environment's value overrides a repository-wide secret of the same name.

### Changed

- **Breaking:** the `sub` claim of CI OIDC tokens is now
  `repo:<owner>/<name>:ref:<ref>`, or `repo:<owner>/<name>:environment:<env>`
  for a job with an environment. Trust policies that match the old
  `repo:<repository id>:pipeline:<id>:job:<id>` (usually as `repo:<id>:*`) stop
  matching, and the token exchange with the cloud provider fails until they are
  rewritten. Match the whole new subject and also require the
  `repository_id` claim; see [CI: OIDC tokens](docs/ci.md#oidc-tokens). Tokens
  also carry `repository`, `repository_owner`, `ref_type`, `environment` and
  `actor`.
- Rolling back the CI secret migration refuses while environment-scoped
  secrets exist, rather than widening them to every job.

## [0.1.1] - 2026-10-11

### Added

- First published release of the server, runner, and MCP binary, with Linux
  x86_64 and aarch64 archives and build attestations.
- Release image and source dependency SBOMs.

### Fixed

- Release workflow publication after the v0.1.0 tag failed to produce a
  GitHub release.

See the [v0.1.1 release notes](docs/releases/v0.1.1.md) for upgrade and
rollback guidance. Publication of a release does not establish that a registry
package is publicly accessible.

[Unreleased]: https://github.com/Nettechnologys/plombir-git/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Nettechnologys/plombir-git/releases/tag/v0.1.1
