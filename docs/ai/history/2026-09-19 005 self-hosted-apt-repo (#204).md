# Self-hosted apt repository on Cloudflare Pages (#204)

## Prompt

> `/remote-control` — investigate the feasibility of #204
>
> yeah, that does seem like a lot of duplication — let's just go with the
> selfhosted apt repo instead. What are my options for this? As mentioned
> before, I have lunchboxos.com and lunchbox-os.com, and I'd prefer to stick to
> free static hosting (i.e. Cloudflare Pages) as much as possible

Design note only; nothing is implemented yet. The feasibility work that led
here is [`2026-09-19 004 launchpad-ppa-feasibility (#204).md`](2026-09-19%20004%20launchpad-ppa-feasibility%20(%23204).md),
which found a Launchpad PPA to be possible but to cost a second, parallel build
system for benefits the project mostly already has.

## Decision

Publish the apt repository ourselves as static files:

* **`dists/` on Cloudflare Pages** at `apt.lunchbox-os.com` — a few kilobytes
  of index, signed by us.
* **`pool/` nowhere at all.** The `.deb` files stay GitHub Release assets and
  are reached by a `302` from a `_redirects` rule. apt follows the redirect and
  verifies the download against the SHA256 in the signed index, so the trust
  chain is unaffected by where the bytes live.

The user-facing instructions in `docs/INSTALL.md` do not change: the hostname,
the keyring path, the `stable main` suite and the `repository.key` URL are all
already what this produces.

```
apt.lunchbox-os.com            (Cloudflare Pages, ~10 KB per deployment)
├── dists/stable/InRelease                        clearsigned
├── dists/stable/Release, Release.gpg             detached-signed
├── dists/stable/main/binary-amd64/Packages{,.gz}
├── dists/stable/main/binary-arm64/Packages{,.gz}
├── repository.key                                public half of the signing key
└── _redirects
      /pool/0.5.1/lunchbox_0.5.1_amd64.deb \
        https://github.com/Lunchbox-OS/lunchbox/releases/download/v0.5.1/lunchbox_0.5.1_amd64.deb  302
      /pool/0.5.1/lunchbox_0.5.1_arm64.deb \
        https://github.com/Lunchbox-OS/lunchbox/releases/download/v0.5.1/lunchbox_0.5.1_arm64.deb  302
```

`Packages` carries `Filename: pool/<version>/<file>.deb`, which is what makes
the redirect reachable through an ordinary apt fetch.

### Why the redirect rather than simply uploading the `.deb` to Pages

**Cloudflare Pages caps a single asset at 25 MiB**
([limits](https://developers.cloudflare.com/pages/platform/limits/)).
`lunchbox_0.5.1_amd64.deb` is **20.1 MiB** today. That is 80% of the ceiling on
a package that grows every time a binary or an embedded asset is added, and the
failure mode is a release that cannot be published, discovered at publish time.
Keeping the payload off Pages removes the ceiling from the picture permanently,
and has the side benefit that each `.deb` exists in exactly one place instead of
two.

Static rules, one per indexed file, rather than a placeholder rule
(`/pool/:ver/* → …/download/v:ver/:splat`): the publish script regenerates
`_redirects` from the set of versions it is indexing, so the static form is
trivially correct and depends on no placeholder-matching subtleties. With every
release indexed (see Decisions) the file grows by two lines per release against
a limit of 2,000 static redirects — a thousand releases of headroom. The
placeholder form stays available as a compaction if that ever runs out.

## Alternatives considered

| Option | Why not |
|---|---|
| **Everything on Pages** (`pool/` + `dists/`) | Fewest moving parts, atomic rollback, and it works *today* — but the 25 MiB ceiling above, and every deployment re-uploads the whole indexed pool. Kept as the fallback: identical URLs, so switching later is invisible to users. |
| **R2 bucket + custom domain** | Technically the best fit — no size cap, incremental writes, whole pool history inside the 10 GB free tier, zero egress. But R2 [requires a payment method on file](https://developers.cloudflare.com/r2/pricing/) even on the free tier, needs a new token scope, and has no atomic deploy (write order matters, no rollback). |
| **GitHub Pages** | Works (100 MB per file, 1 GB site), but it is a second host and a second deploy path when Cloudflare is already wired for the config editor. |
| **Launchpad PPA** | See note 004. |

On terms: Cloudflare's old §2.8 restriction on non-HTML content
[now applies to the CDN in front of someone else's origin](https://blog.cloudflare.com/updated-tos/),
and explicitly permits large files hosted on a Cloudflare service (Pages, R2,
Images, Stream). Neither this shape nor the Pages-only fallback is in a grey
area.

## Verified before choosing

Built the whole arrangement in the session scratchpad and ran a real apt
against it, with a throwaway key and an apt configuration that touched nothing
on the host (`Dir::Etc::*`, `Dir::State::*`, `Dir::Cache` all redirected into
the scratch tree):

* `dists/` generated with `apt-ftparchive packages` + `apt-ftparchive release`,
  `Filename:` rewritten to `pool/0.5.1/…`, `InRelease` clearsigned.
* the `.deb` served by a *second* HTTP server, with the "site" server
  answering `/pool/…` with a `302` at it — the `_redirects` behaviour.

```
--- apt update ---
Get:1 http://127.0.0.1:8801 stable InRelease [2,158 B]
Get:2 http://127.0.0.1:8801 stable/main amd64 Packages [941 B]
--- apt policy ---
  Candidate: 0.5.1
--- download through the redirect ---
Get:1 http://127.0.0.1:8801 stable/main amd64 lunchbox amd64 0.5.1 [21.1 MB]
79452888…a49eebd  lunchbox_0.5.1_amd64.deb   (matches the .deb that was indexed)
```

So: apt follows the cross-host redirect, and the signature and checksum chain
holds across it.

## What has to be built

1. **`scripts/ci/publish-apt.sh`, rewritten.** It currently `PUT`s each `.deb`
   at Forgejo's registry and lets the server build the index. The replacement
   takes the built `.deb`s and the release tag, and emits a directory to
   deploy:
   * the previously published `Packages` per architecture, fetched from the
     live site and checked against the `SHA256` recorded in the `InRelease` it
     was published with — or, with `--rebuild`, regenerated from every
     non-prerelease release's assets;
   * `apt-ftparchive packages` for the new `.deb`s, with `Filename:` rewritten
     to `pool/<version>/<basename>`, appended to that;
   * `apt-ftparchive release` for `dists/stable`, with `Origin`, `Label`,
     `Suite`, `Codename`, `Components`, `Architectures` set;
   * `gpg --clearsign` → `InRelease` and `gpg -abs` → `Release.gpg`;
   * `_redirects` with one line per indexed file, pointing at
     `https://github.com/<repo>/releases/download/<tag>/<basename>`;
   * `repository.key` (public half, ASCII-armoured).

   `apt-ftparchive` rather than `aptly` or `reprepro`: both of those keep a
   local database that a fresh runner would have to persist and restore, while
   `apt-ftparchive` needs nothing but the `.deb`s in hand. The accumulated
   index is the one piece of state, and it lives in the published site — signed
   by us, verifiable on the way back in, and reconstructible with `--rebuild`.

2. **A `publish-apt` job in `release.yml`**, deploying with the same
   `wrangler pages deploy` + Direct Upload pattern as the existing config-editor
   job, reusing `CLOUDFLARE_API_TOKEN` (scoped *Cloudflare Pages: Edit*) and
   `CLOUDFLARE_ACCOUNT_ID`. It must run **after** the release exists and both
   `.deb` assets are uploaded — the index it publishes points at those assets,
   and a draft release's assets are not public. Prereleases are skipped, as
   they are today for the registry and the config editor.

3. **A Pages project and DNS.** A second project (e.g. `lunchbox-apt`) with
   `apt.lunchbox-os.com` as its custom domain, production branch `main`, to
   match how `config.lunchbox-os.com` is set up.

4. **The signing key (#203).** This is the repository-metadata key: private
   half in repository secrets for the job, public half published at
   `/repository.key`. Signing the `.deb` itself remains the separate half of
   #203 — it covers the manual-download path, which a repository key does not.

## Decisions

* **Index every release, not just the current one.** `apt install
  lunchbox=0.4.1` and a downgrade path keep working, and the repository stays a
  complete record of what was published rather than a pointer to the newest
  thing.

  *Implemented by accumulation, not by re-downloading.* The obvious reading of
  "every release" is that each publish fetches every past `.deb` to re-derive
  its `Packages` stanza, which costs more network every release forever. It
  does not have to: a stanza is a pure function of the `.deb`, so the publish
  job fetches **the previously published `Packages`** (a few hundred KB, from
  the live site, checked against the `SHA256` in the `InRelease` we signed last
  time) and appends the stanzas for the `.deb`s it just built. Cost per release
  is constant and small.

  The full rebuild — download every asset from every non-prerelease tag and
  regenerate from scratch — stays available as the disaster-recovery path, for
  a first run and for the case where the published index is lost or wrong. It
  is the same script with a `--rebuild` flag, which also keeps it exercised.

  Consequence: **release assets become load-bearing.** A `.deb` deleted from a
  past release breaks installs of that version against an index that still
  validates. See the failure modes below.

* *Keep `stable`/`main` and the prerelease exclusion*, so no published
  instruction changes.
* *Both architectures indexed*, matching the two `.deb`s the release builds.
* **`lunchboxos.com` 301s to `lunchbox-os.com`**, apex and wildcard, path
  preserved — a zone-level Redirect Rule with a dynamic expression
  (`concat("https://lunchbox-os.com", http.request.uri.path)`), not a Pages
  project. It needs a proxied DNS record on the redirecting zone to have
  something for the rule to run on. The hyphenated name is canonical from here
  on, which is the name `config.` and now `apt.` already use.
* *Pages project named `lunchbox-apt`*, following `lunchbox-config-editor`.
* *`repository.key` committed to the repo* (`dist/apt/repository.key`) as well
  as deployed, so the key users trust is reviewable in `git log`.

Settled in note 006 rather than here: the key itself, and signing the `.deb`
for the manual-download path.

## Failure modes worth knowing

* **Ordering.** Deploying the index before the release assets are public gives
  apt a signed index whose files 404. The job order above is the fix; a
  post-deploy `apt-get download` smoke test against the live repo would catch a
  regression.
* **A deleted release asset** breaks installs of that version even though the
  index still validates — and with every release indexed, that applies to the
  whole history, not just the newest tag. Release assets are immutable as a
  rule, not as a habit.
* **A lost or corrupted published index** is not fatal: `--rebuild` regenerates
  it from the release assets. But it is the only copy, so the rebuild path has
  to stay exercised rather than existing only on paper.
* **Rollback** is a Pages deployment rollback, which is atomic and instant —
  one of the reasons to prefer Pages over R2 for the metadata.
