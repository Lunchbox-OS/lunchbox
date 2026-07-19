# Forgejo APT repository (binary-releases follow-up)

## Prompt

> `/remote-control` — scope out enabling Forgejo's apt repository for this
> project.

Scoping only (no implementation in this pass). This is the deferred
"**GPG-signed `.deb` + APT repo**" follow-up listed under *Open follow-ups* in
[`2026-07-04 003 binary-releases.md`](2026-07-04%20003%20binary-releases.md).

## Where we are today

The release pipeline already:

- builds `shepherd-launcher_X.Y.Z_amd64.deb` in CI (`shepherd package deb`,
  staged from `install.sh` via `DESTDIR` — layout single-sourced), and
- attaches it to a **Forgejo release** as a download asset + `.sha256` sidecar
  (`scripts/ci/upload-release-asset.sh`, `deb` job in `release.yml`).

So users install with `sudo apt install ./shepherd-launcher_X.Y.Z_amd64.deb`
(a one-off file download). There is **no `apt update` / `apt upgrade` path** —
that is exactly what a hosted apt repo adds.

The design doc deferred this on the assumption it needed "a signing key and
appetite." **The signing-key half is already handled by the platform** (see
Finding 1) — so the remaining work is small.

## Findings (verified against the live instance)

Forgejo instance: `15.0.0+gitea-1.22.0` (`/api/v1/version`). The Debian package
registry has been stable in this lineage for years; it is enabled here.

**1. The metadata signing key is auto-provisioned — no admin GPG setup.**
Forgejo signs the repository metadata (the `Release`/`InRelease` files) itself
with a per-owner key it generates on demand. Confirmed live:

```
$ curl -fsS https://git.armeafamily.com/api/packages/albert/debian/repository.key
-----BEGIN PGP PUBLIC KEY BLOCK-----
...
```

Consequence: **the individual `.deb` does not need to be `dpkg-sig`-signed.**
apt trusts packages transitively via the signed `Release` file. The design doc's
"GPG-signed `.deb`" framing is moot — we sign nothing; Forgejo signs the index.
No keystore, no key management, no secret to rotate.

**2. Publishing is a single authenticated `PUT` per `.deb`.**

```
PUT {server}/api/packages/{owner}/debian/pool/{distribution}/{component}/upload
```

`{owner}` = `albert`. `{distribution}` and `{component}` are arbitrary labels we
choose (e.g. `stable` / `main`). Forgejo reads `Package`, `Version`,
`Architecture`, `Depends`, … straight out of the `.deb` control file — which
`scripts/lib/package.sh` already writes correctly — and builds the pool + index.

**3. Consumption is the standard two-line apt setup.**

```
sudo curl -fsSL https://git.armeafamily.com/api/packages/albert/debian/repository.key \
  -o /etc/apt/keyrings/forgejo-albert.asc
echo "deb [signed-by=/etc/apt/keyrings/forgejo-albert.asc] \
  https://git.armeafamily.com/api/packages/albert/debian stable main" \
  | sudo tee /etc/apt/sources.list.d/shepherd.list
sudo apt update && sudo apt install shepherd-launcher
```

`Depends:` (sway, mpv, bluez, the GTK4 runtime libs, …) resolve from the Ubuntu
archive as usual — the Forgejo repo only carries `shepherd-launcher` itself.

**4. Delete / re-run semantics.**

```
DELETE .../debian/pool/{distribution}/{component}/{package}/{version}/{arch}
```

Re-uploading an already-present `{package,version,arch}` returns **409
Conflict**. Release jobs are re-runnable, so the publish step must treat 409 as
success (idempotent) — or `DELETE` then `PUT`. Forgejo retains every uploaded
version in the pool, so `apt upgrade` sees each new release; old versions stay
installable until explicitly deleted.

**5. Packages are owned at the user level, not per-repo.**
The repo lives at `git.armeafamily.com/api/packages/**albert**/debian`, shared
across all of `albert`'s repositories. Fine for us (only shepherd ships a
`.deb`), but worth knowing: the `{distribution}`/`{component}` namespace is the
only separation from any future package.

## Design

Minimal, additive, and consistent with the existing `deb` job. No new toolchain,
no cross-job artifacts, no signing infrastructure.

### 1. `scripts/ci/publish-apt.sh` (new, sibling to `upload-release-asset.sh`)

```sh
# Env: SERVER_URL, OWNER, PACKAGE_TOKEN, APT_DIST (default stable),
#      APT_COMPONENT (default main)
# Usage: publish-apt.sh FILE.deb [FILE.deb ...]
url="${SERVER_URL}/api/packages/${OWNER}/debian/pool/${APT_DIST}/${APT_COMPONENT}/upload"
for f in "$@"; do
  code="$(curl -sS -o /dev/null -w '%{http_code}' -X PUT "$url" \
            -H "Authorization: token ${PACKAGE_TOKEN}" --data-binary @"$f")"
  case "$code" in
    201) echo "published $(basename "$f")" ;;
    409) echo "already published $(basename "$f") (idempotent)" ;;
    *)   echo "::error::PUT $url -> $code"; exit 1 ;;
  esac
done
```

### 2. `release.yml` — one step in the existing `deb` job

Add after the "Upload .deb to the release" step, under the same
`if: push || inputs.publish` guard:

```yaml
      - name: Publish .deb to the apt registry
        if: ${{ github.event_name == 'push' || github.event.inputs.publish == 'true' }}
        env:
          SERVER_URL: ${{ github.server_url }}
          OWNER: ${{ github.repository_owner }}
          PACKAGE_TOKEN: ${{ secrets.RELEASE_TOKEN }}
        run: ./scripts/ci/publish-apt.sh dist/pkg/*.deb
```

Prereleases (`vX.Y.Z-rc1`) are the one nuance: today they publish as a Forgejo
*prerelease* so they aren't "latest". If we push them to the same `stable`
distribution, apt users would pick them up. Cheapest fix: send prereleases to a
separate distribution (e.g. `APT_DIST=testing`) or skip the apt publish for
tags containing `-`. Recommend **skip on `-`** to start (matches "release
candidates are for manually testing the signed artifacts").

### 3. Token scope — the one real prerequisite

The packages API needs a token with the **`write:package`** scope. `RELEASE_TOKEN`
is currently a repo-write PAT; verify/add `write:package` to it (Forgejo PATs are
granular). If we'd rather not widen `RELEASE_TOKEN`, mint a separate
`PACKAGE_TOKEN` secret. Either way this is a one-time settings change, no code.

### 4. `docs/INSTALL.md`

Add an "Installing from the apt repository" subsection above the existing
"Installing from a `.deb`" (keep the direct-`.deb` path as the offline/manual
alternative). Content = the three commands from Finding 3.

## Effort

Small. Roughly:

- `publish-apt.sh` (~30 lines, shellcheck-clean) + one `release.yml` step.
- Confirm/extend the token scope (settings, ~2 min).
- Pick `stable`/`main` labels; decide prerelease handling (skip on `-`).
- `INSTALL.md` apt section.
- Validate end-to-end by publishing the current `VERSION` (a `workflow_dispatch`
  with `publish=true` already builds + would push), then `apt install` on a
  clean 26.04 box.

No image changes, no signing keys, no arm64 dependency. arm64 in the repo stays
the separate `.deb` follow-up (#82) — when it lands, the *same* `PUT` publishes
the `arm64` `.deb` into the same distribution/component and apt serves both
architectures automatically.

## Decisions (implemented)

1. **Distribution/component:** `stable` / `main`.
2. **Token:** dedicated `PACKAGE_TOKEN` secret (`write:package` scope), leaving
   `RELEASE_TOKEN` unchanged.
3. **Prereleases skipped:** the apt-publish step is gated on
   `!contains(github.ref_name, '-')`, so `vX.Y.Z-rc1` tags publish the signed
   release asset but are not served to `apt upgrade` users.
4. **Release assets kept:** the release `.deb` + `.sha256` stays the
   offline/manual path; the apt repo is the `apt upgrade` path. The `deb` job
   produces the file once and publishes to both.

## Files

- `scripts/ci/publish-apt.sh` (new — single `PUT` per `.deb`, 409-idempotent)
- `.github/workflows/release.yml` ("Publish .deb to the apt registry" step in
  the `deb` job + `PACKAGE_TOKEN` in the secrets header)
- `docs/INSTALL.md` ("Installing from the apt repository" section)

## Remaining one-time setup (not in code)

- Create the `PACKAGE_TOKEN` repo/org secret from a Forgejo PAT with the
  `write:package` scope.
- First publish validates end-to-end: run `release.yml` via `workflow_dispatch`
  with `publish=true` (or push the next `vX.Y.Z` tag), then `apt install
  shepherd-launcher` on a clean 26.04 box.
