# Spec: F-Droid repository service for fahrengit-451

**Audience:** an agent working in
[`aarmea/fahrengit-451`](https://github.com/aarmea/fahrengit-451), the Docker
Compose stack that hosts `git.armeafamily.com`. That agent has no access to the
shepherd-launcher repository, so this document is self-contained.

**Companion documents (in shepherd-launcher, for context only):**
[`2026-07-26 002 fdroid-repository.md`](2026-07-26%20002%20fdroid-repository.md)
is the scoping analysis this spec implements — read it if you want the *why*.
This file is the *what*.

**Tracking issue:** shepherd-launcher
[#110](https://git.armeafamily.com/albert/shepherd-launcher/issues/110).

## Goal

Serve an [F-Droid](https://f-droid.org) repository at
`https://git.armeafamily.com/fdroid/repo` so that the two Android apps built by
shepherd-launcher can be installed and **updated** from a phone, instead of being
sideloaded by hand with `adb install`.

An F-Droid repository is a directory of static files (a signed index plus the
APKs). Everything below is about generating that directory and serving it.

## Why this lives in fahrengit-451 rather than in CI

shepherd-launcher's Actions runner is in a homelab, on the far side of the
internet from this VPS. Pushing a generated repository from there would need an
inbound rsync-over-ssh path into the box — on a stack that sets
`FORGEJO__server__DISABLE_SSH=true` specifically so that every access path stays
HTTP and stays geofenceable.

Generating the repository *here* inverts that: a container on the `internal`
network reads Forgejo at `http://forgejo:3000` — no TLS, no geo-block, no
credentials, no public egress — and the release assets it downloads are already
on this machine's disk. **Nothing needs to authenticate to anything.**

## Deliverables

Six changes, all in this repository.

### 1. `fdroid/Dockerfile` — the generator image

Small Debian image with `fdroidserver` and a JDK. Follow the pattern of
`geoblock_watcher/`.

```dockerfile
FROM debian:trixie-slim

# --no-install-recommends matters: it takes the dependency tree from ~269
# packages / ~300 MB to ~67 / ~78 MB. The bulk is matplotlib + tk, pulled in as
# Recommends of androguard, which fdroid update does not need.
#
# default-jdk-headless is NOT redundant: fdroidserver signs the index jar with
# /usr/lib/jvm/default-java/bin/jarsigner and unconditionally prefers that path
# over every other JDK on the machine (common.py: "always prefer the built-in"),
# without checking that the binary exists. It ignores JAVA_HOME, and config.yml
# cannot override it. If the default happens to be a JRE, the run dies at the
# very last step with an opaque OSError. Verify in the built image:
#     docker run --rm IMAGE test -x /usr/lib/jvm/default-java/bin/jarsigner
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
        fdroidserver default-jdk-headless git ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

COPY sync.py /app/sync.py
WORKDIR /app
CMD ["python3", "/app/sync.py"]
```

`fdroidserver` parses APKs with androguard (pure Python — **no Android SDK
needed**). This tooling has been exercised end to end against the real v0.3.0
release APKs with fdroidserver 2.4.3 from Ubuntu 26.04, including the
signing-key rejection path.

### 2. `fdroid/sync.py` — the poll-and-publish loop

Pseudocode; the real thing should be about 150 lines with logging.

```
every POLL_INTERVAL (default 15 min), for each source in config:

  1. r = GET http://forgejo:3000/api/v1/repos/{source.repo}/releases/latest
  2. skip if r.prerelease is true            # RC tags never reach users
  3. skip if r.tag_name == last_published    # state file, see below
  4. fetch metadata for this exact tag:
        GET http://forgejo:3000/api/v1/repos/{source.repo}/archive/{tag}.tar.gz
        extract */dist/fdroid/metadata/*.yml -> WORK/metadata/
  5. download every asset matching source.asset_glob (default *.apk)
        -> WORK/repo/          (assets are public; no auth header)
  6. run: fdroid update --delete-unknown --pretty      (cwd = WORK)
  7. assert every downloaded APK still exists in WORK/repo/ and appears in
     WORK/repo/index-v2.json; if not, FAIL LOUDLY and do not publish (see
     "The signing-key pin" below — this is the security-relevant step)
  8. publish atomically (see below)
  9. write tag to the state file
```

**Working directory layout** (persistent, inside the `fdroid_repo` volume but
*outside* the tree nginx serves):

```
/srv/fdroid/work/config.yml        generated from config/fdroid.yml + env
/srv/fdroid/work/metadata/*.yml    from the shepherd-launcher tag
/srv/fdroid/work/repo/             APKs accumulate here across releases
/srv/fdroid/work/.last_published   the state file from step 9
```

Keeping `work/repo/` persistent is what lets old versions stay installable:
`fdroid update` rebuilds the index from whatever APKs are present, so every
release adds ~112 MB and users keep the ability to downgrade. Set
`archive_older: 0` (the default) so everything stays in the single `repo/`
directory and there is no second `archive/` tree to publish.

**Atomic publish** (step 8) — nginx is serving the previous index the whole time,
so never mutate the live directory in place:

```sh
cp -al  /srv/fdroid/work/repo  /srv/fdroid/repo-${tag}   # hardlinks: instant, no extra space
ln -sfn repo-${tag}            /srv/fdroid/.repo.tmp
mv -T   /srv/fdroid/.repo.tmp  /srv/fdroid/repo          # atomic rename(2)
ls -dt  /srv/fdroid/repo-*  | tail -n +3 | xargs -r rm -rf
```

`cp -al` is safe because source and destination are the same filesystem. `mv -T`
over a symlink is an atomic `rename(2)`; nginx follows symlinks by default and
`open_file_cache` is off, so there is no stale-descriptor window. Rollback is
re-pointing the symlink at an older `repo-*`.

**Generated `work/config.yml`:**

```yaml
repo_url: https://git.armeafamily.com/fdroid/repo
repo_name: Shepherd
repo_description: Android apps for the shepherd-launcher kiosk.
archive_older: 0
keystore: /keystore.jks
repo_keyalias: fdroid-index
keystorepass: ${FDROID_KEYSTOREPASS}
keypass: ${FDROID_KEYPASS}
```

### 3. `docker-compose.yml` — one service, one volume, one mount

```yaml
  fdroid:
    build:
      context: ./fdroid
      dockerfile: Dockerfile
    container_name: fdroid
    restart: unless-stopped
    environment:
      - DOMAIN=${DOMAIN}
      - FDROID_KEYSTOREPASS=${FDROID_KEYSTOREPASS}
      - FDROID_KEYPASS=${FDROID_KEYPASS}
    volumes:
      - ./config/fdroid.yml:/app/config/fdroid.yml:ro
      - ./config/fdroid-keystore.jks:/keystore.jks:ro
      - fdroid_repo:/srv/fdroid
    networks:
      - internal
    depends_on:
      - forgejo
```

Add `fdroid_repo:` to the top-level `volumes:`, and mount it **read-only** into
the existing nginx service:

```yaml
      - fdroid_repo:/srv/fdroid:ro
```

### 4. nginx — a `map` and a `location`

In `nginx/nginx.conf`, at `http{}` level next to the existing maps:

```nginx
# Index files change every release; an APK at a given versionCode never does.
map $uri $fdroid_cache {
    ~\.apk$  "public, max-age=31536000, immutable";
    default  "no-cache";
}
```

In `nginx/templates/git.conf.template`, inside the `443` server block and
**above** `location /`:

```nginx
location ^~ /fdroid/ {
    alias /srv/fdroid/;            # trailing slash on BOTH sides
    autoindex off;
    gzip off;                      # .apk/.jar are already deflated

    # add_header does NOT merge: declaring any add_header in a location
    # discards every one inherited from server{}, so the four security
    # headers have to be repeated here or they are silently lost for
    # /fdroid/*.
    add_header Strict-Transport-Security "max-age=63072000; includeSubDomains" always;
    add_header X-Frame-Options            SAMEORIGIN                            always;
    add_header X-Content-Type-Options     nosniff                               always;
    add_header Referrer-Policy            strict-origin-when-cross-origin       always;
    add_header Cache-Control              $fdroid_cache                         always;
}
```

Because `conf.d` is baked into the image rather than host-mounted, applying this
is `docker compose up -d --build nginx`, not a hot reload.

> **Known interaction with the geo-blocker — please add a comment recording it.**
> `geoblock_watcher` renders `location ^~ <path>` after `rstrip("/")`, so a rule
> for `/fdroid` would produce a *shorter* prefix than the `/fdroid/` location
> above. nginx picks the longest matching prefix, so the static location would
> win and the geo-block would silently never fire for anything under `/fdroid/`.
> Nothing is broken today (no rule targets that path, and geofencing the F-Droid
> repo is explicitly out of scope), but a rule added later would look configured
> and do nothing. Note it in `config/geo_rules.yml.example`'s comments. The clean
> fix, if it is ever wanted, is to have the watcher emit each rule's guard as an
> includable snippet (empty when no rule applies) that a static location can
> `include` unconditionally — the `$geoblock_*` variables are already computed at
> `http{}` level, but referencing one that does not exist is a config-load error,
> which is why the guard cannot simply be written inline ahead of time.

### 5. `config/fdroid.yml.example` + `.env.example`

Config, following the shape of `geo_rules.yml.example` (generic and
config-driven — this stack should not hardcode shepherd):

```yaml
# Which Forgejo repositories to publish APK releases from.
repo_url: https://git.armeafamily.com/fdroid/repo
repo_name: Shepherd
repo_description: Android apps for the shepherd-launcher kiosk.
poll_interval_minutes: 15

sources:
  - repo: albert/shepherd-launcher
    metadata_path: dist/fdroid/metadata   # in that repo, at the release tag
    asset_glob: "*.apk"
```

`.env.example` gains `FDROID_KEYSTOREPASS=` and `FDROID_KEYPASS=`.

### 6. Key bootstrap + README

A `bootstrap_fdroid_key.sh` beside `bootstrap_certs.sh`, or a documented command:

```sh
keytool -genkeypair -v \
  -keystore config/fdroid-keystore.jks -alias fdroid-index \
  -keyalg RSA -keysize 4096 -validity 10000 \
  -dname "CN=Shepherd F-Droid repo, O=armeafamily, C=US"
```

**This key is permanent.** Its SHA-256 fingerprint is embedded in the repository
URL that every device is configured with; losing or rotating it means every
device must remove and re-add the repo. It is the one piece of state on this box
that is not regenerable — say so in the README, and back it up off-box.

Print the fingerprint for the URL with:

```sh
keytool -list -keystore config/fdroid-keystore.jks -alias fdroid-index \
  | grep SHA256 | tr -d ': ' | tail -c 65
```

`fdroid update` also prints the full `?fingerprint=…` URL at the end of a run.

README section: what the service does, how to add a source, the keystore backup
warning, and the add-the-repo URL.

## The landing page is nearly free

`fdroid update` already writes `index.html` (a simple repo page) and `index.png`
(a **QR code of the repo URL**) into `repo/`, so
`https://git.armeafamily.com/fdroid/repo/` is a working onboarding page out of
the box — a phone can scan it instead of typing a 64-character fingerprint.

Two small things worth doing on top:

- Set `repo_icon` in `config.yml` to a real file, or accept the
  `WARNING: repo_icon "repo/icons/icon.png" does not exist, generating
  placeholder` on every run.
- Optionally add `/fdroid/index.html` redirecting to `repo/`, so the shorter URL
  works too.

shepherd-launcher's `docs/INSTALL.md` points users at
`https://git.armeafamily.com/fdroid/repo/` for exactly this reason — please keep
that URL working, or say what it should be instead.

## The contract with shepherd-launcher

What that repository guarantees, so this service can rely on it:

- Release assets are named `shepherd-companion_X.Y.Z.apk` and
  `shepherd-media_X.Y.Z.apk`, attached to a Forgejo release tagged `vX.Y.Z`.
- `dist/fdroid/metadata/<applicationId>.yml` exists at each release tag, one per
  app, in `fdroidserver` metadata format. Today:
  `com.armeafamily.shepherd.companion.yml`, `com.armeafamily.shepherd.media.yml`.
- Those files pin the release signing certificate (below). They deliberately
  contain **no version fields** — `fdroid update` reads `versionCode` and
  `versionName` out of the APK.
- Both apps are signed with one key, have stable `applicationId`s, and derive a
  monotonically increasing `versionCode` from the project version.

### The signing-key pin — do not skip step 7

Each metadata file contains:

```yaml
AllowedAPKSigningKeys: b96abc13004bb75dd4caa412714035e9ce172bf4cb125ea1228163f68636041e
```

That is the SHA-256 of the certificate both apps are signed with today
(`CN=Shepherd Launcher, O=armeafamily, C=US`, verified against the v0.3.0
assets). `fdroid update` refuses to include an APK signed by anything else, and
`--delete-unknown` removes it from `repo/` outright.

This matters because this service downloads and republishes release assets **with
no human in the loop**, and F-Droid installs are in-place upgrades on family
devices.

**The rejection path is not an error path — verify this for yourself before
trusting the exit code.** Pinning a wrong (but well-formed) key and running
`fdroid update` produces:

```
WARNING: Removing repo/shepherd-media_0.3.0.apk
INFO: Creating signed index with this key (SHA256): …
INFO: Finished
$ echo $?
0
```

A signed index, a success exit, and one app silently missing. That is why step 7
asserts every downloaded APK actually appears in `index-v2.json` and refuses to
publish otherwise. shepherd-launcher's `shepherd package fdroid` makes the same
assertion, which is how the above was observed.

## Acceptance criteria

1. `curl -fsS https://git.armeafamily.com/fdroid/repo/index-v2.json | head` returns
   JSON listing both `com.armeafamily.shepherd.companion` and
   `com.armeafamily.shepherd.media` at the current release version.
2. `curl -fsSI https://git.armeafamily.com/fdroid/repo/<apk>` returns 200 with
   `accept-ranges: bytes`, `Cache-Control: public, max-age=31536000, immutable`,
   and the four inherited security headers still present.
3. The F-Droid client on a phone can add the repo by URL + fingerprint and install
   both apps.
4. Publishing a new release tag results in the client offering an update within
   one poll interval, and the previous version remains installable.
5. Restarting the stack (`docker compose down && docker compose up -d`) does not
   change the published index or require a re-download.
6. Corrupting a metadata file, or substituting an APK signed with a different key,
   causes a loud failure and **leaves the previously published repo intact**.

## Non-goals

- **Geofencing this path.** Out of scope for the tracking issue — the apps are of
  very limited use without shepherd-launcher itself. Just record the prefix
  collision noted in §4 so it is not a surprise later.
- **Building anything.** APKs are built and signed by shepherd-launcher's CI; this
  service only republishes released artifacts.
- **Publishing to f-droid.org.** This is a private repository users add by URL.
- **Serving the `.deb`.** That already has an apt path via Forgejo's Debian
  package registry and is untouched by this work.
