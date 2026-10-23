# Using Git

[← README](../README.md)

## Using Git

### SSH

```bash
GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git clone ssh://git@localhost:2222/testuser/testrepo /tmp/myrepo
cd /tmp/myrepo
echo hello > test.txt && git add -A && git commit -m "test"
GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git push origin main
```

### HTTP

```bash
git clone http://localhost:8080/git/testuser/testrepo /tmp/myrepo-http
cd /tmp/myrepo-http
# ... edit files ...
git push origin main
```

### Pull mirrors

A repository with a pull mirror (**Settings → Mirror**, or
`POST /api/v1/repos/<owner>/<repo>/mirror`) fetches its upstream on the
interval it is given, or on **Sync now**, and every pass that succeeds
publishes the upstream's branches and tags to the repository clients clone.
The first pass also points the repository's default branch at the upstream's
when the repository has no branch of that name yet.

The upstream owns `refs/heads/*` and `refs/tags/*`: each pass force-updates
every branch and tag to the upstream's and removes the ones the upstream does
not have. So while the mirror is enabled the repository is read-only: a push
over HTTPS or SSH is refused, and so are web edits, applied suggestions and
pull request merges (`409`). Switching the mirror off (`status: inactive`)
makes it an ordinary repository again. The server's own refs are left alone.

A pass that moves a branch updates the open pull requests on it — their head
commit, auto-merge and the merge queue — but runs no CI pipeline and sends no
`push` webhook: nobody pushed, and an upstream's workflows do not run on this
instance's runners unasked.

### Push mirrors

The other direction: work here and keep a copy on another forge. A
repository administrator adds one or more push mirrors under **Settings → Push
mirrors** (or `POST /api/v1/repos/<owner>/<repo>/push_mirrors` with `url`,
`username`, `password`), and after every change to the repository — a push over
HTTPS or SSH, a web edit, a merge — its branches and tags are pushed to them.
Mirrors that do not push on every change go out on their interval
(`sync_interval_seconds`, `0` for none), and **Push now** sends one at once.

The push runs in the background, never on the push that triggered it: a remote
that is down or refuses the credential does not slow down or fail anything
here. The settings page shows, per mirror, since when it is behind and why its
last push failed; `plombir_git_push_mirror_max_lag_seconds` and the
`PushMirrorBehind` alert say the same to an operator.

By default the remote only moves forward. A branch rewritten here, or moved on
the remote, is reported as refused and left as it is, and a branch deleted
here stays there. **Overwrite the remote** (`force`) makes the mirror an exact
copy — rewritten branches are force-pushed, deleted ones removed — and with it
anything only the remote had is lost, which is why it is never the default.
Only `refs/heads/*` and `refs/tags/*` are pushed, never the server's own refs.

LFS objects go along, before the refs: the remote's LFS batch API is asked
which objects of the pushed history it lacks, and those are uploaded with the
mirror's credential, so `git lfs pull` from the mirror gets files rather than
pointers. An object this server does not have, or one the remote refuses,
does not hold the branches back — they are pushed, and the mirror's last error
names the object by oid and path until a later push gets it there.

The remote is held to the pull mirror's rules: HTTPS (plain HTTP only with
`[mirror].allow_insecure_http`), the private-address guard before every push,
the credential encrypted at rest and handed to `git` through the environment
only. A credential typed into the URL is moved out of it. Neither the API, the
audit log nor a recorded error ever shows it. SSH remotes are not supported:
they need an identity of their own that this server does not manage yet.

### Git LFS

`git lfs` needs no `lfs.url` with either clone URL. From the HTTPS URL the
client finds the endpoint itself. From the SSH URL it speaks Git LFS over the
SSH connection (`git-lfs-transfer`, git-lfs 3.0 and later): the same
repository permission as a clone (download) or a push (upload), the same
checks on every uploaded object as over HTTPS — its digest against its oid, its
size against the one the client declared, the repository's storage budget —
and the locking commands (`git lfs lock`, `locks`, `unlock`) on the same
channel. Deploy keys work too, read-only ones for downloads only, and hold no
locks.

Older clients ask instead with `git-lfs-authenticate`, which sends them to the
instance's public URL with a short-lived credential for that one repository.
That answer can only name the public URL if the server knows it: set
`[server].external_url`. Without it, `git-lfs-authenticate` refuses with a
message saying so, and such a clone needs
`git config lfs.url <external URL>/api/v1/repos/<owner>/<repo>/lfs`.

#### LFS over SSH: how many connections

A stock `git lfs push` to an SSH remote opens four SSH connections, each
authenticated on its own: `git-receive-pack`, `git-upload-pack` (git-lfs
asking which commits the remote already has), and two `git-lfs-transfer` — one
for the lock check, one for the objects (measured with git-lfs 3.4.1, see
`crates/rg-ssh/tests/integration/ssh_lfs_transfer_stock_client_tests.rs`).
Nothing goes over HTTPS. Where `ssh` can multiplex, git-lfs runs its transfer
workers as extra channels on the second connection, without new
authentications. `git lfs pull` takes one connection.

Where every connection needs a person — a key with a passphrase and no agent,
an agent that asks for confirmation, a FIDO key that wants a touch — make it
one authentication for all of them, client side:

```bash
# In ~/.ssh/config:
#   Host git.example.com
#     ControlMaster auto
#     ControlPath ~/.ssh/cm-%C
#     ControlPersist 60
```

To see where a push stops, run it with
`GIT_TRACE=1 GIT_TRANSFER_TRACE=1 GIT_SSH_COMMAND="ssh -v" git push`: each
`exec git-lfs-…` and its authentication is in the output, with timings. The
server logs every SSH key it accepts with the number of unregistered keys the
client offered before it (`rejected_publickeys`), a connection that ends
without authenticating, every `git-lfs-transfer` session it starts and ends,
and every LFS request over HTTPS, refusals included
(`lfs request refused … status=401`): a `401` on the batch means a password was
given where a personal access token is needed.

An import brings the LFS objects along with the history. After the
clone the server asks the source's own batch endpoint
(`<repo>.git/info/lfs/objects/batch`, with the import's token) for every
object the pointers name, checks each against its oid and stores it. An object
the source does not give does not fail the import: the finished task says how
many are missing, and its `stats` list each one by oid and path. A committed
`.lfsconfig` is not followed, and download addresses the source hands back go
through the same private-address guard as the import itself.

A pull mirror does the same after every sync that moves its refs, with the
mirror's own credential, reading only the commits since the last complete
pass. An object the upstream does not give leaves the mirror in `error`, with
the missing objects named by oid and path, and is asked for again on the next
sync.

The file view shows an LFS file as the file: an image inline, other content
as a download, from `GET /api/v1/repos/<owner>/<repo>/raw/<path>`. The blob
API marks a pointer file with `lfs: {oid, size, available}`; a pointer whose
object was never uploaded is reported as such and the raw route answers `404`.
**Download ZIP** and **tar.gz** (`GET /api/v1/repos/<owner>/<repo>/archive/<ref>.zip`)
carry LFS files as their content too, streamed from the LFS store. A pointer
whose object the server does not have stays a pointer, and the archive then
ends with `LFS-OBJECTS-MISSING.txt` naming each such path and oid.

File locking works with the stock client from either clone URL:
`git lfs lock <path>`, `git lfs locks`, `git lfs unlock <path>`. The server
enforces a lock itself: a push whose new commits change a path someone else
has locked is refused for that branch (`path '<path>' is locked by <owner>`),
over HTTPS and SSH alike, and so is an edit made inside a merge commit. The
same lock answers `409` to a web edit or applied suggestion that changes the
path, and to merging a fork's pull request whose commits change it; the lock
holder does all of these as usual. Setting `git config lfs.locksverify true` (or
`--global`) still helps — git-lfs then stops the push before uploading
anything instead of the server refusing it afterwards. Unlocking someone
else's lock takes `--force` from a repository administrator. Repository
settings list the locks (**LFS locks**) and show what the LFS store holds
(**LFS storage**): its size, its objects, and the objects no branch, tag or
pull request points at any more, which an administrator can remove. Objects
uploaded in the last 24 hours are never offered, since a push uploads them
before it moves its ref.
