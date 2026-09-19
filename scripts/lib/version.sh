#!/usr/bin/env bash
# Version management for shepherd-launcher.
#
# The canonical version string lives in the repo-root VERSION file. It is the
# single place to bump; everything else derives from it:
#
#   * scripts/shepherd reads it at runtime (see `VERSION=` in scripts/shepherd).
#   * companion-android's Gradle build reads it at configure time
#     (companion-android/app/build.gradle.kts).
#
# Cargo and npm cannot read a file at manifest-parse time, so their version
# literals are *written* from the canonical file by `shepherd version set` and
# *verified* against it by `shepherd version check` (which CI runs so drift
# fails loudly). The synced literals live in:
#
#   * Cargo.toml                              [workspace.package] version
#   * crates/lunchbox-firewall-bpf/Cargo.toml (excluded from the workspace, so
#                                             it can't use version.workspace)
#   * shepherd-webui/package.json + package-lock.json

# Absolute path to the canonical VERSION file.
version_file() {
    echo "$(get_repo_root)/VERSION"
}

# Print the canonical version (first line, whitespace stripped).
version_read() {
    local f
    f="$(version_file)"
    [[ -f "$f" ]] || die "Version file not found: $f"
    local v
    v="$(head -n1 "$f" | tr -d '[:space:]')"
    [[ -n "$v" ]] || die "Version file is empty: $f"
    echo "$v"
}

# Reject anything that isn't a plausible semver (major.minor.patch with an
# optional -prerelease / +build suffix).
version_validate() {
    local v="$1"
    if [[ ! "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
        die "Invalid version '$v' (expected semver like 1.2.3)"
    fi
}

# Read the [workspace.package] version literal from the root Cargo.toml. The key
# is anchored to the start of a line so it can't match the inline `version = `
# inside [workspace.dependencies] entries.
_version_cargo_workspace() {
    grep -m1 -E '^version = "' "$(get_repo_root)/Cargo.toml" \
        | sed -E 's/^version = "(.*)"/\1/'
}

# Read the [package] version literal from the excluded bpf crate.
_version_cargo_bpf() {
    grep -m1 -E '^version = "' \
        "$(get_repo_root)/crates/lunchbox-firewall-bpf/Cargo.toml" \
        | sed -E 's/^version = "(.*)"/\1/'
}

# Read the lunchbox-firewall-bpf pin from that crate's *own* Cargo.lock.
#
# The bpf crate is excluded from the workspace (it builds for a different
# target), so it carries a separate lockfile that `cargo update --workspace`
# at the repo root never reaches. Left unchecked it drifts one release
# behind and then resurfaces as an unexplained modified file the next time
# anyone builds that crate.
_version_cargo_bpf_lock() {
    awk '/^name = "lunchbox-firewall-bpf"$/ { found = 1; next }
         found && /^version = "/ {
             sub(/^version = "/, ""); sub(/"$/, ""); print; exit
         }' "$(get_repo_root)/crates/lunchbox-firewall-bpf/Cargo.lock"
}

# Read the top-level "version" field from a package.json / package-lock.json.
_version_npm() {
    node -p "require('$1').version"
}

# Rewrite an `^version = "..."` line in a Cargo manifest in place.
_version_set_cargo() {
    local file="$1" new="$2"
    sed -i -E "0,/^version = \".*\"/s//version = \"$new\"/" "$file"
}

# `shepherd version` / `shepherd version get` — print the canonical version.
version_get() {
    version_read
}

# `shepherd version set X.Y.Z` — bump the canonical file and every synced
# literal so nothing drifts.
version_set() {
    local new="${1:-}"
    [[ -n "$new" ]] || die "Usage: shepherd version set <X.Y.Z>"
    version_validate "$new"

    local root
    root="$(get_repo_root)"
    local old
    old="$(version_read)"

    # Canonical file first, so the derived writers below could read it if needed.
    printf '%s\n' "$new" > "$(version_file)"

    _version_set_cargo "$root/Cargo.toml" "$new"
    _version_set_cargo "$root/crates/lunchbox-firewall-bpf/Cargo.toml" "$new"

    # npm owns the package.json + package-lock.json pair; `npm version` rewrites
    # both while preserving their formatting (a hand-rolled JSON edit would
    # reflow the multi-thousand-line lockfile). --allow-same-version keeps
    # re-runs idempotent; --no-git-tag-version leaves committing to the caller.
    if command_exists npm; then
        (cd "$root/shepherd-webui" \
            && npm version --no-git-tag-version --allow-same-version "$new" \
                >/dev/null) \
            || die "npm version failed for shepherd-webui"
    else
        warn "npm not found; shepherd-webui version left unchanged"
    fi

    # Refresh the workspace members' pins in Cargo.lock. Offline + best-effort:
    # a normal build fixes the lock too, so a failure here is not fatal.
    if command_exists cargo; then
        (cd "$root" && cargo update --workspace --offline >/dev/null 2>&1) || true
        # And again for the excluded bpf crate, which the line above cannot
        # see. See _version_cargo_bpf_lock for why this is worth its own call.
        (cd "$root/crates/lunchbox-firewall-bpf" \
            && cargo update --workspace --offline >/dev/null 2>&1) || true
    fi

    success "Bumped version: $old -> $new"
    info "Review the changes and commit them together: VERSION, Cargo.toml,"
    info "Cargo.lock, crates/lunchbox-firewall-bpf/Cargo.{toml,lock}, and"
    info "shepherd-webui/package*.json."
}

# `shepherd version check` — fail if any synced literal has drifted from the
# canonical VERSION file. CI runs this so a hand-edited manifest can't ship a
# mismatched version.
version_check() {
    local canonical
    canonical="$(version_read)"
    local root
    root="$(get_repo_root)"

    local -a mismatches=()
    local actual

    actual="$(_version_cargo_workspace)"
    [[ "$actual" == "$canonical" ]] || mismatches+=("Cargo.toml [workspace.package]: $actual")

    actual="$(_version_cargo_bpf)"
    [[ "$actual" == "$canonical" ]] || mismatches+=("lunchbox-firewall-bpf: $actual")

    actual="$(_version_cargo_bpf_lock)"
    [[ "$actual" == "$canonical" ]] \
        || mismatches+=("lunchbox-firewall-bpf Cargo.lock: $actual")

    if command_exists node; then
        actual="$(_version_npm "$root/shepherd-webui/package.json")"
        [[ "$actual" == "$canonical" ]] || mismatches+=("shepherd-webui/package.json: $actual")

        actual="$(_version_npm "$root/shepherd-webui/package-lock.json")"
        [[ "$actual" == "$canonical" ]] || mismatches+=("shepherd-webui/package-lock.json: $actual")
    else
        warn "node not found; skipping shepherd-webui version check"
    fi

    if [[ ${#mismatches[@]} -gt 0 ]]; then
        error "Version drift from canonical VERSION ($canonical):"
        local m
        for m in "${mismatches[@]}"; do
            error "  $m"
        done
        die "Run 'shepherd version set $canonical' to resync."
    fi

    success "All versions match VERSION ($canonical)"
}

# Dispatch for `shepherd version [get|set|check]`.
version_main() {
    local subcmd="${1:-get}"
    shift || true
    case "$subcmd" in
        get|"")
            version_get
            ;;
        set)
            version_set "$@"
            ;;
        check)
            version_check
            ;;
        -h|--help|help)
            cat <<EOF
Usage: shepherd version [command]

The canonical version lives in the repo-root VERSION file.

Commands:
    get              Print the current version (default)
    set <X.Y.Z>      Bump the version everywhere (VERSION, Cargo manifests,
                     shepherd-webui package.json/lock)
    check            Verify all synced version literals match VERSION

Examples:
    shepherd version
    shepherd version set 0.2.0
    shepherd version check
EOF
            ;;
        *)
            die "Unknown version command: $subcmd (try: shepherd version help)"
            ;;
    esac
}
