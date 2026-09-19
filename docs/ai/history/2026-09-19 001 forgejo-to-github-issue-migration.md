# Moving the issues and pull requests from Forgejo to GitHub

> Status: **done**, 2026-09-19. The issues are live at `aarmea/lunchbox`, the
> pull requests are archived at `aarmea/lunchbox-legacy-issues-prs`, and the
> wiki is at `aarmea/lunchbox`'s GitHub wiki.

## Prompt

> I am migrating this repository from Forgejo to GitHub at aarmea/lunchbox.
> First, let's get all of issues (both open and closed) to the new repository,
> preserving the numbering. `gh` is already authenticated, and should have write
> access to the destination repository. PR descriptions will go into the
> `aarmea/lunchbox-legacy-issues-prs` repository instead. It's currently empty:
> make a new directory below this one, `git init` it, copy the PRs in there,
> making sure to add metadata like the branch name, merge git ref, date merged,
> and CI status, then push. Make sure that URLs are consistent in both
> directions between the (live) issues and (archived) PR descriptions.

## What was there

Numbers **1–201** on Forgejo, drawn from one sequence shared by issues and
pull requests, with no gaps and no number used twice:

| | |
|---|---|
| Issues | 95 (15 open, 80 closed) |
| Pull requests | 106 (104 merged, 1 closed unmerged `#22`, 1 open `#75`) |
| Comments | 117 (27 on issues, 90 on pull requests) |
| Labels in use | one — `future`, on 4 issues |
| Authors | one — `albert` |

Forgejo's API allowed anonymous reads, so no Forgejo token was needed.

## The numbering problem, and the shape of the answer

GitHub also draws issues and pull requests from **one** sequence, and it will
not accept a pull request that was merged somewhere else. So if only the 95
issues were created, they would land on 1–95 and all 351 `#N` references in the
corpus would point at the wrong things.

Every number that belonged to a pull request is therefore held in
`aarmea/lunchbox` by a **closed placeholder issue** labelled
`legacy-pull-request`, whose body carries the pull request's metadata and links
to the archived file. `#N` means on GitHub what it meant on Forgejo, from either
side. Real issues are listed with `is:issue -label:legacy-pull-request`.

Issues were created strictly in ascending order, and each API response was
checked against the number it was supposed to receive — a mismatch aborts the
run rather than leaving a quietly misaligned tracker.

The placeholder for `#75` is left **open**, because that pull request was still
open: the work is a real loose end, not something the migration finished.

## Gotchas worth keeping

**Forgejo forgets branch names.** `head.ref` comes back as `refs/pull/N/head`
once the source branch is deleted — true for 101 of the 106 pull requests. The
name survives in the merge commit subject, which Forgejo writes as:

```
Merge pull request 'Remote file manager (#195)' (#200) from feat/195-remote-file-manager into main
```

Parsing `git log` on `main` recovered all 104 merged branches; their merge SHAs
agreed with the API in every case, and the 5 names the API did report agreed
with git too. The remaining two (`#22`, `#75`) still had live branches. So all
106 source branch names are recorded.

**GitHub's Issue Import API is gone.** `POST /repos/{o}/{r}/import/issues` —
the endpoint that could set `created_at` and import comments in one call —
returns 404. Original dates and authorship therefore cannot be replayed onto
the issues, and every migrated issue shows the migrating account and today's
date. Each body ends in a `<sub>` provenance footer naming the original Forgejo
issue, its author, and the dates it was opened and closed.

**Don't rewrite `#N` inside code.** Several bodies contain `#154` in fenced
blocks and inline spans, where a markdown link would render as literal text.
The rewriter splits on fences and backticks and only substitutes outside them.
It also leaves `#1347` (not an issue) and `#123456` (a hex colour) alone by
requiring the number to match something that exists.

## How the two sides link

| From | To | Form |
|---|---|---|
| Archived PR | issue | `https://github.com/aarmea/lunchbox/issues/N` |
| Archived PR | archived PR | relative `./NNNN.md` |
| Placeholder issue | archived PR | blob URL to `prs/NNNN.md` |
| Real issue | archived PR that closed it | blob URL in the footer, from `Fixes`/`Closes` |
| Real issue | real issue | bare `#N`, which GitHub autolinks |

Bare `#N` is deliberately left alone in live issue bodies: every number 1–201
exists in `aarmea/lunchbox`, so it always resolves — to the issue, or to the
placeholder that points at the archive.

## Not migrated

* **CI run history.** The archived files record each check's name, result and
  duration, and link to the original Forgejo run, which dies with the instance.
* **Diffs.** They are in the git history, reachable from the merge commit
  recorded in each archived file.
* **`albert/shepherd-swipe`** is a separate repository; its link was left alone.

## The wiki

Migrated in a second pass, after the issues. A Forgejo wiki is a git repository
at `<repo>.wiki.git`, so it was cloned rather than copied page by page: 6 pages
(`Home`, `Initial setup`, `Steam`, `Minecraft`, `Windows XP`, `_Sidebar`) and 38
commits of history, plus one commit on top that repoints two links.

**Forgejo serves files at `/src/branch/<branch>/<path>`; GitHub uses
`/blob/<branch>/<path>`.** `Initial-setup.md` linked to `docs/INSTALL.md` and
`crates/shepherd-config/README.md` that way, and both were rewritten.

Two things did *not* need rewriting, and it is worth knowing why:

* **Page links.** Both wikis resolve a bare page name, so `[Steam](Steam)` and
  `[Initial setup](Initial-setup)` work unchanged. `_Sidebar.md` is honoured by
  both as well. The sidebar's `[ScummVM](ScummVM)` was already a red link on
  Forgejo and still is.
* **`[[entries]]` and friends.** These look like wiki links but are TOML
  array-of-tables headers, and all 8 occurrences sit inside fenced code blocks,
  where neither renderer touches them.

**GitHub will not create `<repo>.wiki.git` until a first page exists**, and
there is no REST or GraphQL endpoint for wiki content — `/repos/{o}/{r}/wiki`
and `.../wiki/pages` both 404, and a push to the wiki remote fails with
"Repository not found". One page has to be created in the web UI before the
real history can be force-pushed over it. This is the only step in the whole
migration that cannot be automated.

The two references pointing at the Forgejo wiki (issue `#2`'s body and a comment
on issue `#3`) were repointed at `https://github.com/aarmea/lunchbox/wiki/...`.

## Rerunning

The scripts live in the session scratchpad, not the repo. `migrate_issues.py`
records progress and resumes from the highest number already present, and backs
off on GitHub's secondary rate limit (the run needed 413 mutating calls against a
documented ceiling of 500/hour, and was never throttled). `verify.py` re-checks
numbering, titles, state, labels, comment counts, and that every link resolves in
both directions. `fix_wiki_links.py` repoints Forgejo wiki URLs and is
idempotent, so it doubles as a check.
