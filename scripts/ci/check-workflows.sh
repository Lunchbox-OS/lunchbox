#!/usr/bin/env bash
# Guard the workflow files against duplicate job names.
#
# YAML forbids a repeated mapping key, and Forgejo's parser rejects the whole
# file when it finds one:
#
#   Unable to parse supported events in workflow: yaml: unmarshal errors:
#   line 550: mapping key "webui" already defined at line 239
#
# Nothing runs when that happens — not even a job that would have checked the
# syntax — so this cannot be a CI-only guard. Run it before pushing a workflow
# change; CI runs it too, which still catches a break in release.yml or
# images.yml while ci.yml itself is fine.
#
# The way in is a merge: two branches each add a job, they land far enough apart
# in the file that git merges both without a conflict, and the result is a valid
# diff and an invalid workflow. `webui` arrived exactly that way.
#
# Scope: duplicate job names, which is the failure a merge produces. It does not
# validate the rest of the schema — a duplicate key *inside* a job, an unknown
# field, a bad `needs:` — so it is a fast check against one specific mistake,
# not a linter. Prefer `actionlint` if that ever gets added to the images.

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

status=0

for file in .github/workflows/*.yml .github/workflows/*.yaml; do
  [ -e "$file" ] || continue

  # Job names are the keys indented exactly two spaces under `jobs:`. Restrict
  # to that block so `on:`'s own keys (push, pull_request) are not counted.
  dupes=$(
    awk '
      /^jobs:[[:space:]]*$/ { in_jobs = 1; next }
      in_jobs && /^[^[:space:]#]/ { in_jobs = 0 }
      in_jobs && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
        key = $0
        sub(/^  /, "", key)
        sub(/:[[:space:]]*$/, "", key)
        if (key in first) {
          printf "  %s: line %d, already defined at line %d\n", key, NR, first[key]
        } else {
          first[key] = NR
        }
      }
    ' "$file"
  )

  if [ -n "$dupes" ]; then
    echo "$file has duplicate job names:" >&2
    echo "$dupes" >&2
    status=1
  fi
done

if [ "$status" -ne 0 ]; then
  echo >&2
  echo "Merge the duplicates into one job. Forgejo refuses to run the whole" >&2
  echo "workflow file until this is fixed, so nothing else in it will report." >&2
  exit 1
fi

echo "[OK] No duplicate job names in .github/workflows/"
