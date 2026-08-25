#!/usr/bin/env bash

# One private stand, two real accounts, one scenario matrix. The browser runner
# refuses non-loopback URLs as a second boundary under the stand's mktemp+trap
# ownership, because these scenarios create repositories and mutate data.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

"${ROOT_DIR}/scripts/ephemeral-stand.sh" \
  --frontend \
  --no-founder \
  -- \
  node "${ROOT_DIR}/scripts/ui-access-sweep-e2e.mjs"
