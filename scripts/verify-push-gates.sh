#!/usr/bin/env sh
# Run the push-time gates once and bind the successful result to this commit.
set -eu

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

# The receipt is a claim about HEAD, not about an uncommitted working tree. Card
# work therefore runs this verifier after committing and before pushing.
if ! git diff --quiet -- || ! git diff --cached --quiet --; then
    printf '%s\n' 'push-gates: tracked changes are still uncommitted; commit them before recording a receipt' >&2
    exit 1
fi

printf '%s\n' 'push-gates: checking Rust formatting'
cargo fmt --all -- --check

printf '%s\n' 'push-gates: running strict workspace clippy'
cargo clippy --workspace --all-targets -j 6 -- -D warnings

printf '%s\n' 'push-gates: building the workspace documentation'
cargo doc --workspace --no-deps -j 6

printf '%s\n' 'push-gates: running cargo-free regression gates'
node scripts/run-local-gates.mjs

head_sha=$(git rev-parse HEAD)
receipt_path=$(git rev-parse --git-path forgekeep-push-gates.receipt)
receipt_tmp="${receipt_path}.tmp.$$"
trap 'rm -f "$receipt_tmp"' EXIT HUP INT TERM
umask 077
printf '%s\n' "$head_sha" > "$receipt_tmp"
mv "$receipt_tmp" "$receipt_path"
trap - EXIT HUP INT TERM

printf '%s\n' "push-gates: recorded green receipt for $head_sha"
