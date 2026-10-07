//! The single point at which Plombir Git opens a Git repository in-process.
//!
//! `gix::open` applies [`gix::open::Permissions::secure()`], and "secure" there
//! does not mean "sandboxed" — it is `config: all`, `env: all`,
//! `attributes: all`. Every repository opened that way reads `/etc/gitconfig`,
//! the `~/.gitconfig` of whichever account the server process happens to run
//! under, that process's own `GIT_*` variables, and the system/global
//! `gitattributes` files.
//!
//! That is the right default for a person's own checkout and the wrong one for
//! a server. An operator who set `merge.renames = false` or
//! `merge.<name>.driver` for their own convenience would be steering what
//! Plombir Git does inside *other people's* repositories, and two instances whose
//! hosts are configured differently would answer the same request differently
//! without either of them saying so.
//!
//! [`open`] is the one way in. It keeps the repository-local `config` — which
//! Plombir Git writes itself, and which nothing a client pushes can reach — and
//! drops every source the host contributes.

use std::path::Path;

/// Open a Plombir Git-managed repository under permissions Plombir Git owns.
///
/// Prefer this over `gix::open` everywhere a server-side operation reads or
/// writes a repository: what the operation does must be a property of
/// Plombir Git, not of the machine it was deployed on.
// `gix::open::Error` is a large enum; gix's own constructors carry the same
// allow rather than boxing it, and adding a box here would only make every
// call site pay for a lint about gix's error type.
#[allow(clippy::result_large_err)]
pub fn open(path: impl AsRef<Path>) -> Result<gix::Repository, gix::open::Error> {
    let mut repo = gix::open_opts(path.as_ref(), gix::open::Options::isolated())?;
    // Objects are read as stored, never through `refs/replace/`: what the
    // server checks and merges has to be the object the branch will hold
    // (card_03ed757463d4). Said here rather than left to gix's default, which
    // hangs on `core.useReplaceRefs` — a key gix 0.84 reads with the opposite
    // sense git gives it — and the CLI half is `GIT_NO_REPLACE_OBJECTS` in the
    // gateway's disarmed environment.
    repo.objects.ignore_replacements = true;
    Ok(repo)
}
