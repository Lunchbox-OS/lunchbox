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

Not covered here: the **Android release keystore**
(`LUNCHBOX_KEYSTORE_B64` and friends), which signs the APKs and is a separate
key with its own lifetime.

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
gh api --method PUT repos/aarmea/lunchbox/environments/release --input - <<EOF
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
  repos/aarmea/lunchbox/environments/release/deployment-branch-policies \
  -f name='v*' -f type=tag
```

`prevent_self_review: false` because there is one maintainer; with a second,
set it to `true` and add them.

Then the two secrets (`gh secret set` reads the value from stdin when
`--body` is omitted, which keeps it out of your shell history):

```sh
base64 -w0 lunchbox-apt-ci.asc \
  | gh secret set LUNCHBOX_APT_KEY_B64 --env release --repo aarmea/lunchbox

gh secret set LUNCHBOX_APT_KEY_PASSPHRASE --env release --repo aarmea/lunchbox
# (paste the passphrase, then Ctrl-D)
```

## How the publish job uses it

The signing half of the release job, for reference — the key lands in a
per-run `GNUPGHOME` under `$RUNNER_TEMP` and never in the workspace:

```yaml
    environment: release          # gates the secrets + requires approval
    steps:
      - name: Import the signing subkey
        env:
          KEY_B64: ${{ secrets.LUNCHBOX_APT_KEY_B64 }}
        run: |
          set -euo pipefail
          export GNUPGHOME="$RUNNER_TEMP/gnupg"
          install -d -m 700 "$GNUPGHOME"
          printf 'allow-loopback-pinentry\n' > "$GNUPGHOME/gpg-agent.conf"
          echo "$KEY_B64" | base64 -d | gpg --batch --quiet --import
          echo "GNUPGHOME=$GNUPGHOME" >> "$GITHUB_ENV"
```

and then, wherever a signature is made:

```sh
gpg --batch --yes --pinentry-mode loopback \
    --passphrase "$LUNCHBOX_APT_KEY_PASSPHRASE" \
    --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --pinentry-mode loopback \
    --passphrase "$LUNCHBOX_APT_KEY_PASSPHRASE" \
    --armor --detach-sign -o "$deb.asc" "$deb"
```

`--local-user` is unnecessary in CI: the imported keyring holds exactly one
usable signing key.

## Verifying, as a user

`apt` does it automatically — that is what the `signed-by` keyring in
[docs/INSTALL.md](INSTALL.md) is for.

### Verifying a downloaded `.deb`

```sh
# Authenticity, against the same key the apt instructions install:
curl -fsSLO https://apt.lunchbox-os.com/repository.key
gpg --import repository.key
gpg --verify lunchbox_0.5.1_amd64.deb.asc lunchbox_0.5.1_amd64.deb

# Provenance — which workflow run, from which commit, built this file:
gh attestation verify lunchbox_0.5.1_amd64.deb -R aarmea/lunchbox
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
4. Re-sign the current index with the new key and redeploy — an old index
   signed by a revoked key verifies against nothing once devices have the new
   keyring.

Steps 3 and 4 are why the choice was "no expiry, rotate on compromise" rather
than a dated key: a rotation is a coordinated event with a manual step on every
device, and it should happen because something went wrong, not because a date
passed.
