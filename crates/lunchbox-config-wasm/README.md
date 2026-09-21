# lunchbox-config-wasm

The comment-preserving document model behind the web config editor
(`lunchbox-webui/src/config/`).

## Why this exists

The editor must not regenerate `config.toml`. A regenerated file loses every
comment — `config.example.toml` is 36 KB of which only 10 KB survives a
serde round-trip — and a config people hand-annotate is a config they will not
let a tool rewrite.

So this crate holds a `toml_edit::DocumentMut` as the single source of truth and
exposes a small patch vocabulary over it. The UI never owns a copy of the
config; it sends patches and renders a projection. Comments, key order,
inline-vs-standard table style and number formatting survive because nothing is
ever re-serialized from a model.

Validation is the real thing: `lunchbox_config::parse_config`, compiled to
wasm32, not a reimplementation.

## The patch vocabulary

Four operations over paths like `entries[id=minecraft].limits.max_run_seconds`:

| op | meaning |
|---|---|
| `set` | write a scalar/array/object at the path, creating intermediates |
| `unset` | remove a key, or an array element when the path ends in an index |
| `insert` | append or splice a value into an array |
| `move` | reorder within an array |

`move`'s `to` is where the element lands once it has been lifted out, so the
last slot is `len - 1`.

Three rules make this preserve everything:

- **Identity.** `[[entries]]` and `[[groups]]` are addressed by `id`
  (`entries[id=foo]`), which validation already requires to be unique, so
  deleting or reordering one never misattributes another's comments. Other
  arrays are index-addressed.
- **Minimal writes.** `set` compares the existing value first and does nothing
  if it is unchanged, so a slider dragged away and back leaves the file
  byte-identical.
- **Decor carries over.** Replacing a value copies the old value's prefix and
  suffix decor, so `max_run_seconds = 3600  # one hour` keeps its comment.

## What reordering has to do by hand

`move` on `[[entries]]` or `[[groups]]` (issue #210) is the one operation where
`toml_edit`'s model does not do the obvious thing, in two ways that are silent:

- **Tables render by remembered position, not by vector order**, and the
  sub-tables count. An entry is `[[entries]]` plus `[entries.kind]`,
  `[entries.availability]` and so on; moving only the header leaves those
  behind to be re-read as fields of whichever entry now sits above them. The
  file still parses and still validates — it is simply a different config. So
  every table in an entry's sub-tree is given the same position: the encoder's
  sort is stable, so a tie falls back to structural order and the sub-tree
  stays together.
- **The text above a header belongs to the table**, section banners included,
  so moving the first entry would drag `# --- Entries ---` down the file with
  it. The trailing run of comment lines — the block touching the header —
  travels with the entry; anything cut off by a blank line stays at the
  position. That is the rule people already write by.

An array whose tables are not written consecutively (something else is written
in among them) has no honest answer for where the moved one goes, so it is
refused rather than guessed at.

## Undo, redo, coalescing

`apply` takes an optional coalesce key. Consecutive patches sharing a key
collapse into one undo step, which is what makes a slider drag one undo entry
instead of two hundred. Snapshots are whole document strings — kilobytes each,
and exact down to whitespace.

## The types the editor decodes back

`view()` hands over a `RawConfig` projection, `validate()` a `Report`, and
`availabilityForEntry()` an `AvailabilityView`. All three have generated
TypeScript mirrors, so none of them is a shape anyone keeps in step by hand:

- `RawConfig` -> `lunchbox-webui/src/config/model/config.generated.ts`, from
  `lunchbox-config`'s own schema.
- `Report` and `AvailabilityView` -> `.../model/wasm-types.generated.ts`, from
  this crate's, behind the `schema` feature.

That feature is off by default, so `schemars` never reaches the browser
artifact — `wasm-pack` builds without it. Only `lunchbox-wire-codegen` turns it
on, and the drift test there fails CI if a checked-in mirror goes stale.

`Issue::kind` is an `IssueKind` enum rather than the `&'static str` it started
as, because `schemars` renders a `&'static str` as a bare `string`: generating
from that would have lost the union of names the mirror is worth having.

## Testing

`cargo test -p lunchbox-config-wasm` runs natively; the model is
target-independent. The crate is a workspace member but not a default member,
so it is reached explicitly with `-p` or by `--workspace`.
