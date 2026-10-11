# Publishing a release

1. Update `workspace.package.version` in `Cargo.toml` and add
   `docs/releases/v<version>.md`. Run the full push gates and merge the tested
   commit into `main`.
2. Create an annotated `v<version>` tag on that commit and push the tag. The
   Release workflow rejects tags whose version differs from Cargo, whose commit
   is outside `main`, or whose release notes are missing.
3. Wait for both architecture builds. The workflow publishes a multi-architecture
   GHCR image, Linux binary archives, SPDX SBOMs, checksums, and provenance
   attestations. It verifies the binaries' `--version` output and public image
   access before creating the GitHub release.
4. **First GHCR publication only:** GitHub creates a new container package as
   private, even when the repository is public. An organization package admin
   must open the package settings, change its visibility to public, then rerun
   the failed Release workflow job. Do not distribute the image or call the
   release complete until the anonymous-access check and release job pass.
5. Confirm that the GitHub release contains both architecture archives, their
   SPDX SBOMs, the source SBOM, and `SHA256SUMS`. Pull the versioned image
   without credentials and inspect its amd64 and arm64 manifests. Use its digest
   for deployments. Production deployment is a separate operation.

Never move an existing version tag to a different commit. If a release needs
changes, publish a new version and new notes.
