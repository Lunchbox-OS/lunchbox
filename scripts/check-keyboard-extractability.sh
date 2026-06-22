#!/usr/bin/env bash
# Extractability guard (host spec §5): the swipe-keyboard crates must depend only on the
# external decoder (shepherd-swipe-core) plus small, portable utility crates — never on
# shepherd-launcher's internal crates or its compositor internals. This keeps the whole
# keyboard movable to its own repository later with no surgery.
#
# This asserts none of the keyboard crates' dependency closures include a shepherd-launcher
# workspace member (other than the keyboard crates themselves). shepherd-swipe-core is an
# external git dependency and is allowed.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
# shellcheck source=lib/common.sh
source "$SCRIPT_DIR/lib/common.sh"

command -v cargo >/dev/null 2>&1 || die "cargo is required"

cd "$REPO_ROOT"

# The keyboard crates whose dependencies we constrain.
KEYBOARD_CRATES="shepherd-keyboard-core shepherd-keyboard-wlroots shepherd-keyboard-gnome-daemon"

META_FILE="$(mktemp)"
trap 'rm -f "$META_FILE"' EXIT
cargo metadata --format-version 1 > "$META_FILE" || die "cargo metadata failed"

KEYBOARD_CRATES="$KEYBOARD_CRATES" META_FILE="$META_FILE" python3 - <<'PY'
import json, os, sys

with open(os.environ["META_FILE"]) as f:
    meta = json.load(f)
keyboard = set(os.environ["KEYBOARD_CRATES"].split())

# Workspace member crate names (the shepherd-launcher internals). A path dependency on any
# of these (other than a keyboard crate) breaks extractability.
members = {}
for pkg in meta["packages"]:
    if pkg["id"] in meta["workspace_members"]:
        members[pkg["name"]] = pkg

internal = set(members) - keyboard

violations = []
for name in sorted(keyboard):
    pkg = members.get(name)
    if pkg is None:
        print(f"  ! keyboard crate not found in workspace: {name}", file=sys.stderr)
        sys.exit(2)
    for dep in pkg["dependencies"]:
        # Only normal/build deps matter for the shipped artifact; dev-deps are fine.
        if dep["kind"] in (None, "build") and dep["name"] in internal:
            violations.append((name, dep["name"]))

if violations:
    print("Extractability check FAILED — keyboard crates depend on launcher internals:", file=sys.stderr)
    for crate, dep in violations:
        print(f"  - {crate} -> {dep}", file=sys.stderr)
    sys.exit(1)

print("Extractability OK: keyboard crates depend on no shepherd-launcher internal crates.")
for name in sorted(keyboard):
    deps = sorted(d["name"] for d in members[name]["dependencies"] if d["kind"] in (None, "build"))
    print(f"  {name}: {', '.join(deps)}")
PY
success "extractability check passed"
