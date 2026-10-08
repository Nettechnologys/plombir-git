//! The one way a CI job's workspace becomes an archive, and an archive becomes
//! a workspace.
//!
//! A CI workspace is written by whoever can push: the checked-out tree carries
//! committed symlinks, and a containerised job can create more at run time.
//! The process that packs the declared artifact and cache paths is not inside
//! that container, though — it is the server (embedded runner) or the runner
//! host, and it can read far more than the job could. `tar::Builder` follows
//! symlinks by default, so a committed `leak -> /srv/plombir-git/<repo>.git`
//! or `leak -> ../../../plombir-git.db` declared as an artifact used to be
//! packed *as the target's bytes* into a downloadable archive.
//!
//! [`WorkspaceArchive`] closes that by construction rather than by a flag each
//! packer has to remember:
//!
//! - the builder never follows a link; a symlink met while walking a declared
//!   directory is stored as a symlink entry, never as what it points at;
//! - a declared path is resolved with every symlink in it, and refused when the
//!   resolution leaves the workspace — `out -> /etc` declared as `out` or as
//!   `out/passwd` is an error naming the path, not an archive of `/etc`.
//!
//! [`unpack_into`] is the restore half. `tar::Archive::unpack` already refuses
//! entries that escape the destination (`..` components, writes through a
//! symlink unpacked earlier, hard links to outside files); the function exists
//! so every restore in both runners goes through the one call the tests in this
//! module pin, and a different extraction loop cannot creep in at one site.

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// A `tar` writer whose entries can only come from inside one workspace.
pub struct WorkspaceArchive<W: Write> {
    builder: tar::Builder<W>,
    /// The workspace with every symlink resolved — the prefix a declared
    /// path's own resolution has to stay under.
    workspace: PathBuf,
}

impl<W: Write> WorkspaceArchive<W> {
    /// Start an archive of paths inside `workspace`, written to `writer`.
    pub fn new(workspace: &Path, writer: W) -> io::Result<Self> {
        let workspace = workspace.canonicalize().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "failed to resolve the CI workspace `{}`: {error}",
                    workspace.display()
                ),
            )
        })?;
        let mut builder = tar::Builder::new(writer);
        builder.follow_symlinks(false);
        Ok(Self { builder, workspace })
    }

    /// Add one declared path, archived under the name it was declared with.
    ///
    /// `Ok(false)` means the path does not exist in the workspace (the job never
    /// produced it, or it is a dangling link), which callers have always
    /// treated as "skip". A path whose resolution leaves the workspace is an
    /// error: it is the one case where packing would hand out bytes the job was
    /// never given.
    pub fn append_declared(&mut self, declared: &str) -> io::Result<bool> {
        let relative = Path::new(declared);
        if relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(escapes_workspace(
                declared,
                "it is not a relative path inside it",
            ));
        }

        let source = match self.workspace.join(relative).canonicalize() {
            Ok(source) => source,
            Err(error) if is_absent(&error) => return Ok(false),
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("failed to resolve the declared path `{declared}`: {error}"),
                ))
            }
        };
        if !source.starts_with(&self.workspace) {
            return Err(escapes_workspace(
                declared,
                &format!("a symlink resolves it to `{}`", source.display()),
            ));
        }

        // `source` holds no symlink any more, so these reads cannot be steered
        // somewhere else; anything below a directory is walked without
        // following links by the builder itself.
        let metadata = std::fs::metadata(&source)?;
        if metadata.is_dir() {
            self.builder.append_dir_all(declared, &source)?;
        } else if metadata.is_file() {
            self.builder.append_path_with_name(&source, declared)?;
        } else {
            return Ok(false);
        }
        Ok(true)
    }

    /// Write the archive trailer and hand the writer back.
    pub fn finish(self) -> io::Result<W> {
        self.builder.into_inner()
    }
}

/// Unpack a workspace or cache archive into `destination`.
///
/// Entries that would land outside `destination` — through `..`, through a
/// symlink the same archive (or the workspace) already holds, or as a hard
/// link to a file outside it — are refused by `tar::Archive::unpack`, and
/// `tests::hostile_archives_never_write_outside_the_destination` keeps it that
/// way.
pub fn unpack_into<R: Read>(reader: R, destination: &Path) -> io::Result<()> {
    tar::Archive::new(reader).unpack(destination)
}

fn is_absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn escapes_workspace(declared: &str, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("declared path `{declared}` leaves the CI workspace: {why}"),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const SECRET: &[u8] = b"server-only-secret-bytes";

    /// A workspace plus a sibling directory holding a file the job must never
    /// be able to read — the shape `_ci_workspaces/<repo>/<pipeline>` next to
    /// the repositories it is not allowed to see.
    struct Fixture {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        outside: PathBuf,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("ws");
        let outside = root.path().join("victim");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("private"), SECRET).unwrap();
        Fixture {
            _root: root,
            workspace,
            outside,
        }
    }

    fn pack(workspace: &Path, declared: &[&str]) -> io::Result<Vec<u8>> {
        let mut archive = WorkspaceArchive::new(workspace, Vec::new())?;
        for path in declared {
            archive.append_declared(path)?;
        }
        archive.finish()
    }

    /// Every entry as (path, type, link target, contents).
    fn entries(bytes: &[u8]) -> Vec<(String, tar::EntryType, Option<String>, Vec<u8>)> {
        let mut archive = tar::Archive::new(bytes);
        archive
            .entries()
            .unwrap()
            .map(|entry| {
                let mut entry = entry.unwrap();
                let path = entry.path().unwrap().display().to_string();
                let kind = entry.header().entry_type();
                let link = entry
                    .link_name()
                    .unwrap()
                    .map(|link| link.display().to_string());
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents).unwrap();
                (path, kind, link, contents)
            })
            .collect()
    }

    fn assert_no_secret(bytes: &[u8]) {
        assert!(
            !bytes.windows(SECRET.len()).any(|window| window == SECRET),
            "the archive carries the bytes of a file outside the workspace"
        );
    }

    #[test]
    fn links_inside_a_declared_directory_are_stored_as_links() {
        let fixture = fixture();
        let dist = fixture.workspace.join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("app.bin"), b"built").unwrap();
        let absolute = fixture.outside.join("private");
        symlink(&absolute, dist.join("absolute")).unwrap();
        symlink("../../victim/private", dist.join("relative")).unwrap();

        let bytes = pack(&fixture.workspace, &["dist"]).expect("dist packs");

        assert_no_secret(&bytes);
        let entries = entries(&bytes);
        let link = |name: &str| {
            entries
                .iter()
                .find(|(path, ..)| path == name)
                .unwrap_or_else(|| panic!("{name} missing from {entries:?}"))
        };
        let absolute_entry = link("dist/absolute");
        assert_eq!(absolute_entry.1, tar::EntryType::Symlink);
        assert_eq!(
            absolute_entry.2.as_deref(),
            Some(absolute.to_str().unwrap())
        );
        let relative_entry = link("dist/relative");
        assert_eq!(relative_entry.1, tar::EntryType::Symlink);
        assert_eq!(relative_entry.2.as_deref(), Some("../../victim/private"));
        assert_eq!(link("dist/app.bin").3, b"built");
    }

    #[test]
    fn a_declared_path_that_resolves_outside_is_refused() {
        let fixture = fixture();
        symlink(
            fixture.outside.join("private"),
            fixture.workspace.join("abs-file"),
        )
        .unwrap();
        symlink("../victim/private", fixture.workspace.join("rel-file")).unwrap();
        symlink(&fixture.outside, fixture.workspace.join("abs-dir")).unwrap();
        symlink("../victim", fixture.workspace.join("rel-dir")).unwrap();

        for declared in [
            "abs-file",
            "rel-file",
            "abs-dir",
            "rel-dir",
            "abs-dir/private",
            "rel-dir/private",
            "./rel-dir/",
        ] {
            let error = pack(&fixture.workspace, &[declared])
                .expect_err("a path resolving outside the workspace must not pack");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{declared}");
            assert!(
                error.to_string().contains("leaves the CI workspace"),
                "{declared}: {error}"
            );
        }
        for declared in ["../victim/private", "/etc/passwd"] {
            let error = pack(&fixture.workspace, &[declared])
                .expect_err("a lexically escaping path must not pack");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{declared}");
        }
    }

    #[test]
    fn links_that_stay_inside_and_missing_paths_keep_working() {
        let fixture = fixture();
        let build = fixture.workspace.join("build/v1");
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("report.txt"), b"report").unwrap();
        symlink("build/v1", fixture.workspace.join("latest")).unwrap();
        std::fs::write(fixture.workspace.join("plain.txt"), b"plain").unwrap();

        let mut archive = WorkspaceArchive::new(&fixture.workspace, Vec::new()).unwrap();
        assert!(archive.append_declared("latest").unwrap());
        assert!(archive.append_declared("plain.txt").unwrap());
        assert!(!archive.append_declared("never-built").unwrap());
        assert!(!archive.append_declared("plain.txt/child").unwrap());
        let bytes = archive.finish().unwrap();

        let entries = entries(&bytes);
        let contents = |name: &str| {
            entries
                .iter()
                .find(|(path, ..)| path == name)
                .map(|entry| entry.3.clone())
                .unwrap_or_else(|| panic!("{name} missing from {entries:?}"))
        };
        assert_eq!(contents("latest/report.txt"), b"report");
        assert_eq!(contents("plain.txt"), b"plain");
    }

    fn hostile(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        build(&mut builder);
        builder.into_inner().unwrap()
    }

    fn link_entry(
        builder: &mut tar::Builder<Vec<u8>>,
        kind: tar::EntryType,
        path: &str,
        target: &Path,
    ) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(0);
        header.set_mode(0o644);
        builder
            .append_link(&mut header, path, target)
            .expect("link entry");
    }

    fn file_entry(builder: &mut tar::Builder<Vec<u8>>, path: &str, contents: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        // `append_data` refuses `..`; a hostile archive is not written by us.
        let name = header.as_old_mut();
        name.name[..path.len()].copy_from_slice(path.as_bytes());
        header.set_cksum();
        builder.append(&header, contents).expect("file entry");
    }

    #[test]
    fn hostile_archives_never_write_outside_the_destination() {
        let fixture = fixture();
        let target = fixture.outside.join("private");

        // A link out, then a file written through it.
        let through_link = hostile(|builder| {
            link_entry(builder, tar::EntryType::Symlink, "out", &fixture.outside);
            file_entry(builder, "out/private", b"overwritten");
        });
        // A hard link to an outside file, then a write into that name.
        let hard_link = hostile(|builder| {
            link_entry(builder, tar::EntryType::Link, "hard", &target);
            file_entry(builder, "hard", b"overwritten");
        });
        // A plain `..` entry.
        let parent = hostile(|builder| file_entry(builder, "../victim/private", b"overwritten"));

        for (what, bytes) in [
            ("through a symlink", through_link),
            ("through a hard link", hard_link),
            ("through `..`", parent),
        ] {
            let destination = fixture.workspace.join("restore");
            std::fs::create_dir_all(&destination).unwrap();
            let outcome = unpack_into(bytes.as_slice(), &destination);
            assert_eq!(
                std::fs::read(&target).unwrap(),
                SECRET,
                "an archive wrote outside its destination {what} (unpack: {outcome:?})"
            );
            std::fs::remove_dir_all(&destination).unwrap();
        }

        // A link the workspace already holds is not a way out either.
        let destination = fixture.workspace.join("restore");
        std::fs::create_dir_all(&destination).unwrap();
        symlink(&fixture.outside, destination.join("cache")).unwrap();
        let through_existing =
            hostile(|builder| file_entry(builder, "cache/private", b"overwritten"));
        let outcome = unpack_into(through_existing.as_slice(), &destination);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            SECRET,
            "an archive wrote through a link the destination already held (unpack: {outcome:?})"
        );
    }
}
