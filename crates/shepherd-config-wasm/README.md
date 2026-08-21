# shepherd-config-wasm

The comment-preserving document model behind the web config editor
(`shepherd-webui/src/config/`).

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

Validation is the real thing: `shepherd_config::parse_config`, compiled to
wasm32, not a reimplementation.

## The patch vocabulary

Four operations over paths like `entries[id=minecraft].limits.max_run_seconds`:

| op | meaning |
|---|---|
| `set` | write a scalar/array/object at the path, creating intermediates |
| `unset` | remove a key, or an array element when the path ends in an index |
| `insert` | append or splice a value into an array |
| `move` | reorder within an array |

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

## Undo, redo, coalescing

`apply` takes an optional coalesce key. Consecutive patches sharing a key
collapse into one undo step, which is what makes a slider drag one undo entry
instead of two hundred. Snapshots are whole document strings — kilobytes each,
and exact down to whitespace.

## Testing

`cargo test -p shepherd-config-wasm` runs natively; the model is
target-independent. The crate is a workspace member but not a default member,
so it is reached explicitly with `-p` or by `--workspace`.
