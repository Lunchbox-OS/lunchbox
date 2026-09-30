# Self-hosted F-Droid repository on Cloudflare R2 (#205)

## Prompt

> let's address #205 by "self"-hosting a repo on Cloudflare instead of using
> F-Droid proper. refer to both how this project already sets up its apt
> repository on Cloudflare and fahrengit-451's existing implementation for how
> to do an F-Droid repository. first just scope out the work

and, after the scoping was presented in chat:

> I'm good with all of the recommendations except I'd like everything since
> 0.6.0 on the repo. Write the scope doc

and, after the first draft of this note, which proxied the APKs from GitHub
Releases through a Pages Function:

> hm it appears that GitHub's ToS disallows this type of usage as whole --
> including the apt repo as implemented. let's go the R2 route, and also file
> an issue to transition the apt repo to R2

That issue is #253. Scoping only; nothing is implemented yet.

### The issue as filed

#205, *Publish the Android apps to f-droid.org proper*, points out that the
two apps are published to a self-hosted F-Droid repository generated inside the
[`fahrengit-451`](https://github.com/aarmea/fahrengit-451) stack on
`git.armeafamily.com`, which is the Forgejo host the project is leaving. It
proposes f-droid.org as the fix, and lists what that would cost: metadata moves
to `fdroiddata`, F-Droid builds the apps itself (Gradle plus the `cargo-ndk`
cross-compile, from a clean checkout), APKs are signed by F-Droid's key unless
the build is reproducible, and the media app's vendored `libmpv` goes through
their inclusion review. It also says that keeping a self-hosted repo is
reasonable.

This note takes the second route. It removes the dependency on the Forgejo
host, which was the urgent part of #205, and costs none of the above.
f-droid.org can still be done later on top of this. #205 should be retitled
to match, or closed in favour of a new issue, leaving f-droid.org as a
separate, optional follow-up.

## Where things stand

* **Metadata:** `dist/fdroid/metadata/com.lunchboxos.{companion,media}.yml`.
  They have no version fields, and `AllowedAPKSigningKeys` pins the Android
  release certificate (`6959cc58…330f`, `docs/release-signing.md`).
* **Validation:** `lunchbox package fdroid` (`scripts/lib/package.sh`) runs
  `fdroid update` with a throwaway index key. It then checks that every input
  APK reached `index-v2.json`, because `fdroid update` exits 0 after dropping
  an APK whose signer does not match the pin. `release.yml`'s `apk` job runs it
  against every APK it signs.
* **Generation:** only on the fahrengit-451 host, which polls Forgejo releases
  (`fdroid/sync.py`). It will never see a GitHub release. So `com.lunchboxos.*`
  has never been published anywhere.
* **The documented URL:** `docs/INSTALL.md` and `scripts/lib/admin.sh`
  send users to `https://lunchbox-os.com/fdroid/repo`, with fingerprint
  `b3dc61…b422`. That URL answers **522** today. The fingerprint belongs to the
  fahrengit-451 index key.
* **Releases:** v0.6.1 is the only one on GitHub (v0.6.0 was lost to a release
  published by hand; see `release.yml`). Its APKs are
  `lunchbox-companion_0.6.1.apk` at **45 MB** and `lunchbox-media_0.6.1.apk` at
  **63 MB**.

## What carries over

**From fahrengit-451's `sync.py`:**

* The generation step: metadata at the release tag, APKs into `repo/`,
  `fdroid update --delete-unknown --pretty`, and `archive_older: 0`.
* The check that every expected APK is in `index-v2.json`. It is the control
  that makes publishing without a human in the loop safe. It already exists
  here in `package_fdroid`.
* Keeping the APKs between runs, so every version stays listed. There that
  was a Docker volume; here it is the bucket.
* The known fdroidserver pitfalls, already written down in
  `dist/fdroid/README.md`: `--no-install-recommends`, and `default-jdk-headless`
  for `jarsigner`.

**Dropped:** the polling loop, the state file, and the snapshots with their
symlink swap. CI runs the step once per release.

**From the apt repository (notes 005 and 2026-09-23 001):**

* Its own subdomain.
* Publishing inside the `publish` job, after the release is public, in the
  `release` environment.
* Prereleases skipped.
* Keeping to what was already published, with a full rebuild from Release
  assets for the first run and for disaster recovery (apt's `--previous` and
  `--rebuild`).
* A smoke test against the live site.

**Not carried over:** the apt repository's `pool/` is a `302` from Pages
`_redirects` to GitHub Release assets. That now looks ruled out on its own
account (#253), and would not have worked here either (next section).

## Why the APKs cannot come from GitHub Releases

Two independent reasons:

* **GitHub's terms.** Sending every user download to Release assets uses
  GitHub as the file host for a package repository. The maintainer's reading
  of the terms is that this is not an allowed use (the closest written passage
  is the Acceptable Use Policies' section on excessive bandwidth use). That
  rules out a redirect and a proxy alike. #253 applies the same reasoning to
  the apt repository.
* **The client would not have followed a redirect anyway.** fdroidclient's
  `libs/download` `HttpManager.kt` builds its Ktor client with
  `followRedirects = false` (checked on `master`, 2026-09-29), and the
  maintainers say this is deliberate: redirects make phishing and
  machine-in-the-middle attacks easier. Pages caps a file at 25 MiB, and both
  APKs are over it.

So the APKs need a Cloudflare host that takes files of this size, which means
R2.

## Decision

The **whole repository lives in an R2 bucket** served on a custom domain:

```
fdroid.lunchbox-os.com                 R2 bucket lunchbox-fdroid (custom domain)
└── fdroid/repo/
    ├── entry.jar, entry.json          signed with the index key; uploaded LAST
    ├── index-v2.json, index-v1.jar, diff/…
    ├── index.html, index.png          landing page + QR code (fdroid update)
    ├── icons…, <applicationId>/…      extracted from the APKs
    └── lunchbox-{companion,media}_X.Y.Z.apk   every release since v0.6.0
```

The repository URL is **`https://fdroid.lunchbox-os.com/fdroid/repo`**:

* **A subdomain**, like `apt.` and `config.`, keeps the apex domain free for
  the landing site (`CONTRIBUTING.md`, *config editor*).
* **`/fdroid/repo` in the path**, even though it repeats the subdomain, is what
  the F-Droid client's link handling matches on. Tapping the link on a phone
  opens F-Droid instead of a browser.

Why the whole repository, not a Pages site with only the APKs in R2: the index
names each APK by a path relative to the repository URL, so a split would need
a Function in front to join the two hosts under one name. One bucket needs
nothing in front of it.

The cost is that **R2 has no atomic deploy**, which is one of the reasons note
005 kept apt's index on Pages. Here it is handled by upload order:

1. APKs, icons and other per-app files. Nothing refers to them yet.
2. `index-v2.json`, `index-v1.jar`, `diff/…`, `index.html`, `index.png`.
3. `entry.jar` and `entry.json` last.

A client reads `entry.jar` first and checks everything else against the
hashes it contains. A client that fetches during step 2 can see a hash
mismatch. It reports a failed refresh and gets the complete set next time. It
never installs anything unverified.

`Cache-Control` is set on each object when it is uploaded:

* `no-cache` on the entry and index files, because they change every release;
* `public, max-age=31536000, immutable` on APKs, because a filename is never
  reused.

The custom domain runs behind Cloudflare's cache, so this must be checked
after the first publish: fetch an index file twice and confirm the second
response is not a stale cached copy.

R2 custom domains don't serve `index.html` for a directory path. For
`https://fdroid.lunchbox-os.com/fdroid/repo/` (the QR landing page that
`docs/INSTALL.md` links) to work, a zone Rewrite Rule has to map the trailing
slash to `index.html`. Transform Rules are on the free plan. The public
`r2.dev` URL stays disabled.

Trust works as before: the index is signed by our key and records each APK's
SHA-256 and signer. The client checks both, and Android checks the APK
signature again on install.

### Alternatives considered

| Option | Why not |
|---|---|
| **Pages + a Function streaming APKs from GitHub Releases** | This note's first draft. GitHub's terms, above. |
| **Pages + a Function streaming APKs from an R2 binding** | Keeps the index deploy atomic, but adds a Function, `_routes.json` and a second deploy mechanism. The ordered upload does the same job with less. |
| **GitHub Pages** | Not Cloudflare, and its 1 GB site limit fills in about ten releases with full history kept. |
| **f-droid.org** | #205 as filed. Not needed to leave Forgejo, and it can come later. |

## Decisions

* **R2, with a payment method on file.** Note 005 turned R2 down only for that
  reason, and it is now accepted. What it costs:
  * 10 GB is free, which is about 90 releases at ~110 MB each. After that it
    is $0.015 per GB-month.
  * Egress is free.
  * Each publish is a few dozen Class A operations (uploads), against 1M free
    a month.
* **A new index signing key**, made in the release-signing ceremony
  (`docs/release-signing.md`), not the fahrengit-451 key behind `b3dc61…`.
  Replacing it costs nothing now:
  * nothing has been published at the new URL;
  * the devices that trust the old key have only the Shepherd-era
    `com.armeafamily.shepherd.*` apps, which are different application IDs, so
    they must add the new repository anyway;
  * the old key lives on the host being retired.

  Like the APK key, **it is permanent**. Its fingerprint is part of every
  configured repository URL, and replacing it means every device removes and
  re-adds the repository.
* **Every release since v0.6.0 is indexed**, matching the apt repository.
  That is every non-prerelease release whose tag is ≥ `v0.6.0`, which today
  means v0.6.1. Earlier tags are Forgejo-era, have `com.armeafamily.*` APKs
  signed by the retired key, and are not on GitHub.

### How indexing everything works

`fdroid update` builds the index from the APKs sitting in `repo/`, and drops
any version whose file is not there. There is no equivalent of apt appending
to its previous `Packages`. So:

* **A normal publish** downloads the APKs already in the bucket (egress is
  free), adds this release's, and regenerates the whole index. It uploads only
  the new APKs, followed by every index file in the order above.
* **`--rebuild`** fills `repo/` from the Release assets of every
  non-prerelease tag ≥ `v0.6.0` instead, checking each against its `.sha256`.
  It runs on the first publish, when the bucket is empty, and after the bucket
  is lost. This is one download per publish by our own CI, not user downloads,
  so it does not run into GitHub's terms the way serving users would.
* **Checking what gets downloaded:** everything in `repo/` is about to be
  signed into the index, so it must be ours, wherever it came from. The
  signing-key pin covers that: an APK not signed by `6959cc58…` is dropped,
  and the "reached the index" check turns the drop into a failure.
* **Nothing is deleted from the bucket.** An APK that is already published is
  never removed or overwritten; the publish script refuses to upload over an
  existing key with different contents. Rolling back means re-running an
  earlier tag's publish, which rewrites the index files.

## Work

In the order the commits should land. Each one builds and passes on its own.

1. **Split out the generation core** from `package_fdroid` into something both
   the local command and CI can call. Validation and publishing must not drift
   apart; the drift fahrengit-451 had is what #205's context section describes.
   No behaviour change: `lunchbox package fdroid` still uses a throwaway key.
2. **`scripts/ci/publish-fdroid.sh`**, alongside `publish-apt.sh`. Given the
   APKs just built, the tag, `--out DIR`, and either `--previous BUCKET` or
   `--rebuild`, it:
   * fills `repo/` as described in *How indexing everything works*, then adds
     this run's APKs;
   * writes `config.yml`:
     * `repo_url` as above;
     * `repo_name: Lunchbox Apps` (the name `docs/INSTALL.md` already uses);
     * `repo_icon` from the #217 icons;
     * `archive_older: 0`;
     * the index keystore from the environment;
   * checks the index key's fingerprint against the committed one (item 5)
     before anything is written. This is the same check `publish-apt.sh` makes
     against `dist/apt/repository.key`;
   * runs the shared core: `fdroid update` plus the "reached the index" check.

   Uploading is a separate script, `scripts/ci/upload-fdroid.sh`, which uploads
   in order with the per-object `Cache-Control`. A build can then be checked
   without credentials.

   Upload through the S3 API with the `aws` CLI, which the runner already has,
   or `rclone`. Recent `aws` versions send checksum headers that R2 has
   rejected before. Test with `AWS_REQUEST_CHECKSUM_CALCULATION=when_required`
   ready in case they still are.
3. **`scripts/ci/test-publish-fdroid.sh`** and a CI job, following
   `test-publish-apt.sh`. It runs offline with a throwaway index key and a
   small APK signed with a throwaway key; building one needs the Android image.
   A local S3-compatible server, or a directory standing in for the bucket,
   plays R2. Checks:
   * `--previous` keeps the earlier releases listed;
   * `--rebuild` from Release assets gives the same index;
   * a wrongly signed APK fails the run;
   * a wrong index-key fingerprint fails the run;
   * a missing `.sha256`, or one that does not match, fails the run;
   * an upload over an existing APK with different contents is refused;
   * the upload order is APKs, then index, then `entry.*`.
4. **The index key.**
   * A ceremony section in `docs/release-signing.md` covering: `keytool`,
     PKCS12, RSA 4096, no expiry, an offline copy in the password manager.
   * Secrets `LUNCHBOX_FDROID_KEYSTORE_B64` and
     `LUNCHBOX_FDROID_KEYSTORE_PASSWORD`, in the **`release` environment**
     (unlike the APK key, dry runs never need it).
   * The fingerprint committed as `dist/fdroid/index-key.fingerprint`, so the
     value users are told to trust shows up in `git log`.
5. **`release.yml`: steps in the `publish` job**, after the apt smoke test, and
   skipped for prereleases:
   * build the repo;
   * upload it;
   * smoke test: fetch `entry.jar` and check its signer against the committed
     fingerprint, check that `index-v2.json` lists both apps at `$VERSION`,
     download one APK and compare its SHA-256 with the index, and fetch
     `/fdroid/repo/` to confirm the rewrite rule.

   There should also be a `fdroid_rebuild` dispatch input, like
   `apt_rebuild`, and a 404 on the published `entry.jar` should trigger a
   rebuild automatically, as apt's first publish does.

   Two more changes to the job:
   * New secrets `LUNCHBOX_R2_ACCESS_KEY_ID` and
     `LUNCHBOX_R2_SECRET_ACCESS_KEY`, in the `release` environment. They come
     from an R2 API token with Object Read & Write on this bucket only.
     `CLOUDFLARE_ACCOUNT_ID` provides the S3 endpoint.
   * `publish` runs on `ubuntu-latest`, whose fdroidserver is older than the
     2.4.3 every other path here has been tested with. Either run this step in
     the Android image (Ubuntu 26.04) or confirm the runner's version works.

   Update the job's header comment with the new ordering.
6. **Docs, in the same PR:**
   * `docs/INSTALL.md`: the URL and the new fingerprint;
   * `scripts/lib/admin.sh:739`;
   * `dist/fdroid/README.md`: "Who reads these files" is now this repo's CI;
   * `CONTRIBUTING.md`: the F-Droid paragraph, and an R2 section next to the
     config editor's Pages one;
   * the `package_fdroid` header comment;
   * `release.yml`'s top comment.

### Left for a human

1. A payment method on the Cloudflare account, and R2 enabled.
2. The `lunchbox-fdroid` bucket, with `fdroid.lunchbox-os.com` as its custom
   domain, `r2.dev` access off, and the zone Rewrite Rule for
   `/fdroid/repo/` → `/fdroid/repo/index.html`.
3. An R2 API token scoped to that bucket, stored as the two environment
   secrets.
4. The index-key ceremony and its two environment secrets, then commit the
   fingerprint.
5. A dry run, then the next release tag.
6. On a real phone: add the repo from the QR page, install both apps, and
   interrupt a download to check it resumes. The `companion-pairing` skill's
   adb setup can drive this.
7. After cutover: remove the source from fahrengit-451's `config/fdroid.yml`,
   and retitle #205 or split it (see above).

Items 1 and 3's tooling are shared with #253, which moves the apt packages to
R2 as well. Doing this first lets #253 reuse the setup.

## Acceptance

1. `https://fdroid.lunchbox-os.com/fdroid/repo/` shows the QR page, and the
   F-Droid client adds the repo from the `?fingerprint=` URL in
   `docs/INSTALL.md`.
2. Both apps install, and every release since v0.6.0 is offered, so an older
   version can be installed.
3. A new tag leads to an update offer on the phone, and the previous version
   is still listed.
4. A wrongly signed APK, a bad metadata file, or the wrong index key fails the
   `publish` job before anything is uploaded.
5. An APK URL returns `200` directly, with `accept-ranges` and the immutable
   `Cache-Control`. Index files come back `no-cache` and are fresh on a second
   fetch.
6. Emptying a scratch copy of the bucket and running with `--rebuild` produces
   an index listing the same releases.

## Failure modes worth knowing

* **Ordering, twice.** The APKs have to be in the bucket before the index that
  lists them, and `entry.*` has to be uploaded after everything it covers. The
  upload script owns this; its test checks it.
* **A publish that fails partway** can leave new APKs in the bucket without an
  index listing them. That's harmless, and the next run picks them up. It can
  also leave new index files under an old `entry.jar`, which clients reject
  until a re-run completes the upload.
* **The bucket is now the only copy of the published repository.** Its APKs
  are also on GitHub Releases, so `--rebuild` can restore it. That path has to
  keep working, which is why the test runs it.
* **Losing the index key** means every device removes and re-adds the repo.
  Back it up offline, as with the APK key.

## As built

> how far can you go without additional input from me

> go ahead

Everything in *Work* except the pieces that wait on the index key, on branch
`fdroid-r2`: `fdroid-update.sh` split out of `package_fdroid`; listing and
repository icons; `publish-fdroid.sh`, `upload-fdroid.sh`,
`fetch-fdroid-index.sh`, `fetch-release-apks.sh`; `test-publish-fdroid.sh` and
CI's *F-Droid repository* job; the `publish` job's steps; and the runbook in
`docs/release-signing.md`.

### Where it departs from the scope above

* **Uploads are signed by curl (`--aws-sigv4`), not the `aws` CLI.** That
  needs nothing on either the runner or the Android image, and the test's
  stand-in for R2 can verify the signatures. It found the one real pitfall:
  curl signs the path as given, and fdroidserver names listing icons like
  `icon_tfm-…=.png`. An unencoded `=` is signed as `=` and checked by S3 as
  `%3D`, so the script percent-encodes keys itself. With that encoding
  removed, the stand-in rejects the upload, which is what shows the check
  works. The `aws` CLI checksum question goes away.
* **The published index is verified by fdroidserver's own
  `download_repo_index_v2`**, not by hand with `jarsigner`. `jarsigner
  -verify` passes an unsigned jar, and fdroidserver's check (`-strict`,
  expecting exit 4 for a self-signed certificate, then the single signer's
  fingerprint, then the index hash against `entry.json`) is the one written
  to match the client.
* **An APK already in the bucket is recognised by `x-amz-meta-sha256`**, set
  on upload. An `ETag` is not a usable content hash once an upload is
  multipart.
* **`status/` is not uploaded.** It is fdroidserver's report on the run (the
  runner's OS, tool paths), and nothing a client reads.
* **The fdroidserver steps run in the Android image through `docker
  exec`**, one container for all of them. It has fdroidserver 2.4.3, which is
  what CI tests with; the upload and the `gh` downloads stay on the runner.
* **Publishing is switched on by committing
  `dist/fdroid/index-key.fingerprint`.** Until then the steps warn and skip,
  so this can merge before the key ceremony without breaking the next
  release. After that, a missing secret fails the release like any other.

### Found on the way

* **Listing icons have never worked**, on fahrengit-451 either. fdroidserver
  2.x stopped extracting them from APKs and reads
  `metadata/<appId>/en-US/icon.png`, which never existed here. Each is now a
  symlink to the #217 artwork. `repo_icon` has to be a bare file name in the
  working directory: fdroidserver writes the value into the index as given.
* **`config.yml` values were written unquoted.** That was harmless with the
  fixed strings the local check used, but the real description has a colon.
  They are JSON-quoted now, which also protects the keystore password.
* **An empty `repo_description` crashes `fdroid update`** with a bare
  `TypeError` while writing `index.xml`, so `--description` is required.

### Checked, and not

Checked here:

* `test-publish-fdroid.sh`, 17 checks, including the refusals;
* the real v0.6.1 APKs through `--rebuild` and `--previous` against a local
  server, with a throwaway index key;
* `fetch-release-apks.sh` against the live GitHub releases;
* `release.yml` and `ci.yml` with actionlint. The three shellcheck findings it
  reports in `ci.yml` are already on `main`.

**Not run:** anything against R2 or Cloudflare, the `docker exec` steps, or
the live smoke test. Nothing can be until the bucket and key exist. The two
specific uncertainties:

* whether R2 accepts curl's SigV4 as the stand-in does, in particular the
  explicitly supplied `x-amz-content-sha256`;
* whether the custom domain's cache honours the per-object `Cache-Control`
  for `no-cache` index files.

The first release after the fingerprint lands exercises both. If a dry run is
wanted before that, running `upload-fdroid.sh` against the real bucket from a
workstation checks the first.

### Left for a human

1. The bucket, domain, rewrite rule and token (`docs/release-signing.md`,
   *The bucket*).
2. The index key ceremony and its secrets (*Making it*).
3. One commit with `dist/fdroid/index-key.fingerprint`, the new URL and
   fingerprint in `docs/INSTALL.md`, and `scripts/lib/admin.sh:739`.
4. The next release, then the phone check.
5. Removing this source from fahrengit-451, and retitling #205.
