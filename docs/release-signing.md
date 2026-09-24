# Release signing

Lunchbox signs two things with one OpenPGP key:

* the **apt repository index** (`InRelease` / `Release.gpg`) at
  <https://apt.lunchbox-os.com>, which is what makes `apt install lunchbox`
  trustworthy — apt then checks each `.deb` against the SHA256 in the signed
  index; and
* a detached **`.asc` beside each `.deb`** attached to a GitHub release, for
  people who download the package by hand instead.

A third guarantee needs no key at all: each `.deb` also carries a
[build-provenance attestation](#verifying-a-downloaded-deb) signed through
Sigstore with a short-lived certificate issued to the workflow run.

The **Android release keystore**, which signs the APKs, is a separate key with
its own lifetime. It has [its own section](#the-android-release-keystore) at
the end.

## The key

| | |
|---|---|
| Identity | `Lunchbox Archive Signing Key <apt@lunchbox-os.com>` |
| Primary | Ed25519, `[SC]`, **no expiry**, never leaves the workstation |
| Subkey | Ed25519, `[S]`, no expiry — this is the half CI gets |
| Offline backup | password manager, with the other release key material |
| CI copy | `release` environment secrets, subkey only |

Ed25519 because every target is Ubuntu 26.04 or newer; `apt` verifies an
Ed25519-signed `InRelease` there without complaint (checked end to end against
a real `apt-get update`, not assumed).

No expiry on either key: an expired signing key does not degrade, it stops
every `apt update` on every installed device at once, and the fix cannot travel
over apt. Rotation therefore happens on compromise only — see
[Rotation](#rotation-and-compromise). The reasoning behind this and the other
choices is in
[`docs/ai/history/2026-09-19 006 release-signing (#203).md`](ai/history/2026-09-19%20006%20release-signing%20(%23203).md).

Only the **signing subkey** goes to CI. `gpg --export-secret-subkeys` produces
a key that can sign and nothing else — it cannot certify, add subkeys or
revoke — so a compromised runner cannot take over the identity, and recovery
stays possible from the primary, which never touches a runner.

## One-time ceremony

Run this on a trusted workstation, not in CI. It uses an isolated `GNUPGHOME`
so nothing lands in your personal keyring.

```sh
# Scratch keyring for the ceremony. Everything below writes here.
export GNUPGHOME="$(mktemp -d)"
chmod 700 "$GNUPGHOME"
printf 'default-cache-ttl 0\nmax-cache-ttl 0\nallow-loopback-pinentry\n' \
    > "$GNUPGHOME/gpg-agent.conf"
gpgconf --kill gpg-agent

# The passphrase protects both the offline backup and the CI copy. Generate it
# in your password manager first and paste it here; do not invent it at the
# prompt and hope to remember it.
read -rsp 'passphrase: ' PASS; echo
```

Generate the primary, then the signing subkey:

```sh
gpg --batch --pinentry-mode loopback --passphrase "$PASS" \
    --quick-generate-key 'Lunchbox Archive Signing Key <apt@lunchbox-os.com>' \
    ed25519 sign never

FPR="$(gpg --list-keys --with-colons | awk -F: '/^fpr:/{print $10; exit}')"

gpg --batch --pinentry-mode loopback --passphrase "$PASS" \
    --quick-add-key "$FPR" ed25519 sign never

SUB="$(gpg --list-keys --with-colons "$FPR" \
       | awk -F: '$1=="sub"{want=1;next} want&&$1=="fpr"{print $10; exit}')"

echo "primary $FPR"
echo "subkey  $SUB"
```

`never` is the expiry argument in both commands. `gpg --list-secret-keys
--with-subkey-fingerprints` should now show `sec ed25519 … [SC]` and
`ssb ed25519 … [S]`.

Export the four artifacts:

```sh
# 1. Public half — deployed to the site and committed to the repo.
gpg --armor --export "$FPR" > repository.key

# 2. Offline backup of the whole key — password manager only.
gpg --batch --pinentry-mode loopback --passphrase "$PASS" \
    --armor --export-secret-keys "$FPR" > lunchbox-apt-primary.asc

# 3. Signing subkey alone — this becomes the CI secret.
gpg --batch --pinentry-mode loopback --passphrase "$PASS" \
    --armor --export-secret-subkeys "${SUB}!" > lunchbox-apt-ci.asc

# 4. Revocation certificate, written automatically at generation time.
cp "$GNUPGHOME/openpgp-revocs.d/$FPR.rev" lunchbox-apt-revoke.asc
```

The `!` after `${SUB}` is not optional: it means *that subkey exactly*, rather
than "the key this fingerprint belongs to".

### Check the CI export before trusting it

The one mistake worth catching here is exporting the primary by accident. Import
the CI copy into another throwaway keyring and look at the sigils:

```sh
CIHOME="$(mktemp -d)"; chmod 700 "$CIHOME"
GNUPGHOME="$CIHOME" gpg --batch --import lunchbox-apt-ci.asc
GNUPGHOME="$CIHOME" gpg --list-secret-keys --with-subkey-fingerprints
```

Expected — `sec#` with the trailing hash means the primary secret is *absent*,
which is the whole point:

```
sec#  ed25519 [SC]
      <primary fingerprint>
uid           Lunchbox Archive Signing Key <apt@lunchbox-os.com>
ssb   ed25519 [S]
      <subkey fingerprint>
```

Then prove it can do the two jobs it exists for, and that the public half
verifies them the way apt will:

```sh
printf 'Origin: Lunchbox\nSuite: stable\n' > Release

GNUPGHOME="$CIHOME" gpg --batch --yes --pinentry-mode loopback \
    --passphrase "$PASS" --local-user "${SUB}!" \
    --clearsign -o InRelease Release
GNUPGHOME="$CIHOME" gpg --batch --yes --pinentry-mode loopback \
    --passphrase "$PASS" --local-user "${SUB}!" \
    --armor --detach-sign -o Release.gpg Release

gpg --dearmor < repository.key > repo.gpg
gpgv --keyring "$PWD/repo.gpg" InRelease        # must print a good signature
gpgv --keyring "$PWD/repo.gpg" Release.gpg Release
```

`gpgv` is the verifier apt itself uses, with no keyring but the one it is
handed — if this passes, apt will too.

### Put each artifact where it belongs

| Artifact | Destination |
|---|---|
| `lunchbox-apt-primary.asc` | password manager (attachment) |
| the passphrase | password manager (field) |
| `lunchbox-apt-revoke.asc` | password manager (attachment) |
| `lunchbox-apt-ci.asc` | GitHub, as `LUNCHBOX_APT_KEY_B64` (below) |
| the passphrase, again | GitHub, as `LUNCHBOX_APT_KEY_PASSPHRASE` |
| `repository.key` | committed to `dist/apt/repository.key`, deployed by the publish job |

Then clean up, but **restore from the backup first** — it is the only copy:

```sh
RESTORE="$(mktemp -d)"; chmod 700 "$RESTORE"
GNUPGHOME="$RESTORE" gpg --batch --import lunchbox-apt-primary.asc
GNUPGHOME="$RESTORE" gpg --list-secret-keys   # expect `sec` with NO trailing #
rm -rf "$RESTORE" "$CIHOME" "$GNUPGHOME"
shred -u lunchbox-apt-primary.asc lunchbox-apt-ci.asc lunchbox-apt-revoke.asc
unset PASS
```

## GitHub setup

The secrets live in a `release` **environment**, not on the repository, so only
the publish job can read them and a human approves each release before anything
is signed.

```sh
# Your numeric user id, for the reviewers list.
ME="$(gh api user --jq .id)"

# Create the environment with yourself as a required reviewer.
gh api --method PUT repos/Lunchbox-OS/lunchbox/environments/release --input - <<EOF
{
  "wait_timer": 0,
  "prevent_self_review": false,
  "reviewers": [{"type": "User", "id": $ME}],
  "deployment_branch_policy": {
    "protected_branches": false,
    "custom_branch_policies": true
  }
}
EOF

# Only vX.Y.Z tags may use this environment — a branch cannot reach the key.
gh api --method POST \
  repos/Lunchbox-OS/lunchbox/environments/release/deployment-branch-policies \
  -f name='v*' -f type=tag
```

`prevent_self_review: false` because there is one maintainer; with a second,
set it to `true` and add them.

Then the two secrets (`gh secret set` reads the value from stdin when
`--body` is omitted, which keeps it out of your shell history):

```sh
base64 -w0 lunchbox-apt-ci.asc \
  | gh secret set LUNCHBOX_APT_KEY_B64 --env release --repo Lunchbox-OS/lunchbox

gh secret set LUNCHBOX_APT_KEY_PASSPHRASE --env release --repo Lunchbox-OS/lunchbox
# (paste the passphrase, then Ctrl-D)
```

## Before the first release

A `v*` tag cannot publish until every one of these is done, and fails safely
until then — at the signing step, before anything is public:

1. The [ceremony](#one-time-ceremony) above, and the environment and secrets
   under [GitHub setup](#github-setup). Create the environment *before* the
   first tag: GitHub creates a missing environment on first use, with no
   reviewer.
2. `repository.key` committed at `dist/apt/repository.key`. The publish job
   checks every signature it makes against this file, so a secret that is not
   the key users trust fails the release rather than every device's
   `apt update`.
3. A Cloudflare Pages project named **`lunchbox-apt`**, production branch
   `main`, with **`apt.lunchbox-os.com`** as its custom domain — set up the way
   `lunchbox-config-editor` and `config.lunchbox-os.com` are. The job deploys
   to it with the existing `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`.
   The dashboard wants an upload to create the project, so give it a
   placeholder with an `index.html` **and a top-level `404.html`**. Without a
   `404.html`, Pages treats the site as a single-page app and answers every
   path with `index.html` and a 200, so the first release takes the
   placeholder for a published index and fails its signature check.
4. A dry run: *Actions → Release → Run workflow* from `main` with `publish`
   unticked, which builds and signs every package with the Android key but
   publishes nothing and never reaches the archive key.

The first release finds no published index (`InRelease` answers 404) and
builds one from every release's assets instead of appending; after that each
release appends.

## Cutting a release

```sh
./scripts/lunchbox version set X.Y.Z      # commit, and merge to main
git tag -a vX.Y.Z -m vX.Y.Z               # on the merged commit
git push origin vX.Y.Z
```

Then approve the `publish` job when the run asks. **The tag comes from git,
never from the GitHub UI.** Release immutability is on for this repository,
so a release is sealed the moment it is published: its assets can never be
added or changed, its tag cannot move or be deleted, and deleting the release
does not free the tag name. `release.yml` creates the release as a draft,
attaches every asset, and publishes it last. "Draft a new release → Publish"
in the UI publishes it first, empty, and that version is gone for good. The
guard job refuses a tag whose release was published by anyone but
`github-actions[bot]`. So a mistake costs the version number, but not a
twenty-minute build that fails at the upload.

v0.6.0 was lost exactly this way, so the first release through this workflow
is 0.6.1.

## What a release does with it

`release.yml`'s `publish` job, which runs in the `release` environment and
waits for approval before its first step:

| Step | Script | Fails closed on |
|---|---|---|
| import the subkey into a per-run `GNUPGHOME` under `$RUNNER_TEMP` | inline | an empty `LUNCHBOX_APT_KEY_B64` |
| `.sha256` for every asset, `.asc` for each `.deb` | `scripts/ci/sign-release-assets.sh` | a `.asc` that does not verify against `dist/apt/repository.key` |
| build-provenance attestation for every `.deb` and `.apk` | `actions/attest-build-provenance` | |
| create the GitHub release — draft, upload, then public | `scripts/ci/publish-release.sh` | replacing an asset of a published release |
| build and sign the apt index | `scripts/ci/publish-apt.sh` | a published index that does not verify, a changed release asset, an index signature that does not verify against `repository.key` |
| deploy it to Pages, then `apt-get download` each architecture from the live site | inline | a downloaded `.deb` that differs from the one built |

Everything is signed before anything is public, and the apt index is deployed
only after the release it points at is public. Prerelease tags (`v0.6.0-rc1`)
get a signed, attested release but never reach the apt index.

The passphrase reaches `gpg` on stdin (`--passphrase-fd 0`), not on its
command line. No `--local-user`: the imported keyring holds exactly one usable
signing key.

To test a change to the apt half without a release, run
`./scripts/ci/test-publish-apt.sh` (it needs `apt-utils`). It stands the
arrangement up offline with a throwaway key and runs a real `apt-get` against
it. CI runs it as the *apt repository* job.

### Recovering the apt index

The published index is the only copy of the accumulated `Packages`. If it is
lost or wrong, regenerate it from the release assets: *Actions → Release → Run
workflow*, **from the latest release tag**, with both `publish` and
`apt_rebuild` ticked. The release step finds everything already uploaded and
changes nothing. The apt step then downloads every non-prerelease release's
`.deb` and `.asc`, refuses any whose `.asc` does not verify, and indexes the
lot.

One thing breaks an index that still verifies: **deleting or replacing an
asset of a published release.** apt fetches each `.deb` through a redirect to
its release asset, so that version stops installing. `publish-release.sh` never
replaces one, and `publish-apt.sh` refuses to index a `.deb` whose bytes differ
from what it already indexed.

`_redirects` is regenerated on every publish from `github.repository`, so if
the repository moves again, the next release points every indexed version at
the new name. No rebuild is needed.

## Verifying, as a user

`apt` does it automatically — that is what the `signed-by` keyring in
[docs/INSTALL.md](INSTALL.md) is for.

### Verifying a downloaded `.deb`

```sh
# Authenticity, against the same key the apt instructions install -- and
# only that key, the way apt checks, without touching your own keyring:
curl -fsSL https://apt.lunchbox-os.com/repository.key | gpg --dearmor > lunchbox.gpg
gpgv --keyring ./lunchbox.gpg lunchbox_0.5.1_amd64.deb.asc lunchbox_0.5.1_amd64.deb

# Provenance — which workflow run, from which commit, built this file:
gh attestation verify lunchbox_0.5.1_amd64.deb -R Lunchbox-OS/lunchbox
```

The two claims differ: the signature says the key holder released it, the
attestation says `release.yml` produced it from a specific commit.

## Rotation and compromise

With no expiry, there is no scheduled rotation. If the CI secret or the
primary leaks:

1. Publish the revocation certificate and rotate the GitHub secrets
   immediately, so nothing new can be signed with the old subkey.
2. Generate a new key with this same ceremony, commit the new
   `dist/apt/repository.key`, and publish it.
3. **Tell users to re-fetch the key.** This is the part revocation does not do
   for you: apt does not consume OpenPGP revocations usefully, and a device
   holding only the old key sees signature failures on every `apt update` until
   someone installs the new keyring by hand. Announce it in the release notes
   and in `docs/INSTALL.md`.
4. Re-sign the index with the new key and redeploy. An old index signed by a
   revoked key verifies against nothing once devices have the new keyring.
   This takes more than an ordinary [rebuild](#recovering-the-apt-index): a
   rebuild refuses any `.deb` whose `.asc` does not verify against the
   *current* `repository.key`, and every past `.asc` was made by the old key.
   Before re-signing them, check each past `.deb` with
   `gh attestation verify`. The attestation involves no key of ours, so a
   leaked key cannot forge one, and it is the check that still means
   something here. Then make each new `.asc` with the new key, replace the old
   `.asc` asset by hand (`gh release upload --clobber`, `.asc` files only, never
   a `.deb`), and run the rebuild.

Steps 3 and 4 are why the choice was "no expiry, rotate on compromise" rather
than a dated key: a rotation is a coordinated event with a manual step on every
device, and it should happen because something went wrong, not because a date
passed.

## The Android release keystore

Separate from everything above: an RSA key in a PKCS12 keystore that signs both
APKs (`com.lunchboxos.companion`, `com.lunchboxos.media`). Android refuses an
update signed by a different key than the installed app, and the F-Droid
metadata pins its certificate, so **this key cannot be rotated without every
user uninstalling the apps**. Losing it is permanent.

| | |
|---|---|
| Certificate | `CN=Lunchbox, O=Lunchbox OS, C=US`, RSA 4096, 100-year validity |
| SHA-256 | `6959cc58afb8cde34da782a5b57a3028bf04141e8b106b80b69da7650f3b330f` |
| Pinned in | `AllowedAPKSigningKeys` in both `dist/fdroid/metadata/*.yml` |
| Offline copy | password manager: the `.p12`, its password, the alias |
| CI copy | repository secrets, not the `release` environment: dry runs from `main` sign APKs too |

It replaced the Shepherd-era key (`CN=Shepherd Launcher`, `b96abc13…041e`)
when releases moved to GitHub, before any release-signed `com.lunchboxos.*`
APK had shipped, which was the last point where that stranded nobody.

### Generating one

Only if the key above is lost or compromised. The commands were tested with a
throwaway keystore through the project's own Gradle release build and
`apksigner`.

```sh
umask 077
read -rsp 'keystore password: ' KS_PASS; echo; export KS_PASS

keytool -genkeypair \
  -keystore lunchbox-release.p12 -storetype PKCS12 -storepass:env KS_PASS \
  -alias lunchbox-release \
  -keyalg RSA -keysize 4096 -sigalg SHA256withRSA \
  -validity 36500 \
  -dname 'CN=Lunchbox, O=Lunchbox OS, C=US'

# The value AllowedAPKSigningKeys pins -- the same digest
# `apksigner verify --print-certs` reports for a signed APK:
keytool -list -v -keystore lunchbox-release.p12 -storepass:env KS_PASS \
  | awk '/SHA256:/{gsub(":","",$2); print tolower($2); exit}'
```

PKCS12 has one password for the store and the key. The Gradle build still
reads both, so both secrets get the same value:

```sh
R=Lunchbox-OS/lunchbox
base64 -w0 lunchbox-release.p12 | gh secret set LUNCHBOX_KEYSTORE_B64 --repo $R
printf %s "$KS_PASS" | gh secret set LUNCHBOX_KEYSTORE_PASSWORD --repo $R
printf %s "$KS_PASS" | gh secret set LUNCHBOX_KEY_PASSWORD --repo $R
gh secret set LUNCHBOX_KEY_ALIAS --repo $R --body lunchbox-release

shred -u lunchbox-release.p12; unset KS_PASS
```

Then update both `AllowedAPKSigningKeys` pins and the table above in the same
commit. The release's `apk` job runs `lunchbox package fdroid` against every
APK it signs, so a pin that does not match the secret fails the release.
