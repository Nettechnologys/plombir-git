#!/usr/bin/env bash

# The acceptance contract is two identical journeys over two different empty
# databases. Reusing one stand would only prove that names happen not to clash;
# it would not prove teardown removed the repository, issue, session and Chrome
# state produced by the first run.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
JOURNEY_RUNS=${JOURNEY_RUNS:-2}

if [[ ! ${JOURNEY_RUNS} =~ ^[1-9][0-9]*$ ]]; then
  echo "first-user journey: JOURNEY_RUNS must be a positive integer" >&2
  exit 2
fi

for run in $(seq 1 "${JOURNEY_RUNS}"); do
  echo "first-user journey: clean stand ${run}/${JOURNEY_RUNS}"
  STAND_REBUILD_FRONTEND=1 "${ROOT_DIR}/scripts/ephemeral-stand.sh" \
    --frontend \
    --no-founder \
    -- \
    node "${ROOT_DIR}/scripts/first-user-journey-e2e.mjs"
done

echo "first-user journey: ${JOURNEY_RUNS} clean stand(s) passed"
