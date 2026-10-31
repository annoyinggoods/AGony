#!/usr/bin/env bash
# Release notes from the roadmap (Part12 12.4.2): every landed part, by
# its title — the "# Part NN — Title" line of REVIEW/roadmap/Part*.md,
# in part order.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "## What landed"
for file in REVIEW/roadmap/Part*.md; do
    grep -m1 '^# ' "$file" | sed 's/^# /- /'
done
