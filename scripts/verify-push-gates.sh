#!/usr/bin/env sh
# Run the push-time gates once and bind the successful result to this commit.
set -eu

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

# A successful exit is not enough for a green receipt: several toolchains emit
# warnings without failing. Stream each command's combined output, preserve its
# real status without relying on non-POSIX `pipefail`, and reject warning
# diagnostics before the receipt can be written.
run_warning_free() {
    gate_log=$(mktemp "${TMPDIR:-/tmp}/plombir-git-push-gate.XXXXXX")
    gate_status="${gate_log}.status"

    if (
        set +e
        "$@"
        command_status=$?
        printf '%s\n' "$command_status" > "$gate_status"
        exit 0
    ) 2>&1 | tee "$gate_log"; then
        tee_status=0
    else
        tee_status=$?
    fi

    if [ "$tee_status" -ne 0 ]; then
        printf '%s\n' 'push-gates: could not capture gate output' >&2
        rm -f "$gate_log" "$gate_status"
        return "$tee_status"
    fi
    if [ ! -s "$gate_status" ]; then
        printf '%s\n' 'push-gates: gate command ended without recording its status' >&2
        rm -f "$gate_log" "$gate_status"
        return 1
    fi

    command_status=$(cat "$gate_status")
    if [ "$command_status" -ne 0 ]; then
        rm -f "$gate_log" "$gate_status"
        return "$command_status"
    fi
    if grep -Ei '(^|[[:space:]])warning:' "$gate_log" >/dev/null; then
        printf '%s\n' 'push-gates: a gate command emitted warning diagnostics; receipt not recorded' >&2
        rm -f "$gate_log" "$gate_status"
        return 1
    fi

    rm -f "$gate_log" "$gate_status"
}

# The receipt is a claim about HEAD, not about an uncommitted working tree. Card
# work therefore runs this verifier after committing and before pushing.
if ! git diff --quiet -- || ! git diff --cached --quiet --; then
    printf '%s\n' 'push-gates: tracked changes are still uncommitted; commit them before recording a receipt' >&2
    exit 1
fi

printf '%s\n' 'push-gates: checking Rust formatting'
run_warning_free cargo fmt --all -- --check

printf '%s\n' 'push-gates: running strict workspace clippy'
run_warning_free cargo clippy --workspace --all-targets -j 6 -- -D warnings

printf '%s\n' 'push-gates: building the workspace documentation'
run_warning_free env RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps -j 6

printf '%s\n' 'push-gates: running cargo-free regression gates'
run_warning_free node scripts/run-local-gates.mjs

head_sha=$(git rev-parse HEAD)
receipt_path=$(git rev-parse --git-path plombir-git-push-gates.receipt)
receipt_tmp="${receipt_path}.tmp.$$"
trap 'rm -f "$receipt_tmp"' EXIT HUP INT TERM
umask 077
printf '%s\n' "$head_sha" > "$receipt_tmp"
mv "$receipt_tmp" "$receipt_path"
trap - EXIT HUP INT TERM

printf '%s\n' "push-gates: recorded green receipt for $head_sha"
