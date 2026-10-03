#!/usr/bin/env sh
# Activate the repository-owned hooks for this clone only.  The directory is
# relative to the Git directory, so the setting remains valid if the clone is
# moved on disk.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(git -C "$script_dir/.." rev-parse --show-toplevel)
git -C "$repo_root" config --local core.hooksPath .githooks

printf '%s\n' 'Installed Plombir Git Git hooks from .githooks/.'
