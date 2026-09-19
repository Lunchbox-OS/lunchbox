//! The comment-preserving document model.
//!
//! A [`ConfigDoc`] owns a [`toml_edit::DocumentMut`] and mutates it in place.
//! Nothing is ever re-serialized from a model, so comments, key order, table
//! style and number formatting survive by construction rather than by effort.
//!
//! Everything on this type's edges is a string or plain Rust value, so the
//! whole model builds and tests natively; only the binding layer in `lib.rs`
//! is wasm-gated.

use crate::path::{Path, Seg};
use crate::report::Report;
use serde_json::Value as Json;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

/// A single mutation. Mirrors the `Patch` union in the TypeScript client.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Patch {
    /// Write a value at `path`, creating intermediate tables as needed.
    Set { path: String, value: Json },
    /// Remove a table key, or an array element when `path` ends in a selector.
    Unset { path: String },
    /// Splice a value into the array at `path` (appending when `index` is None).
    Insert {
        path: String,
        #[serde(default)]
        index: Option<usize>,
        value: Json,
    },
    /// Reorder within the array at `path`.
    Move {
        path: String,
        from: usize,
        to: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocError(pub String);

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DocError {}

type DocResult<T> = Result<T, DocError>;

fn err<T>(msg: impl Into<String>) -> DocResult<T> {
    Err(DocError(msg.into()))
}

/// How deep the undo stack goes. Snapshots are whole documents (tens of KB at
/// worst), so this is bounded for memory rather than for correctness.
const UNDO_LIMIT: usize = 200;

pub struct ConfigDoc {
    doc: DocumentMut,
    undo: Vec<String>,
    redo: Vec<String>,
    /// Key of the gesture that produced the top undo snapshot, if any. A patch
    /// arriving under the same key extends that snapshot instead of pushing a
    /// new one, which is what makes a slider drag one undo step.
    coalescing: Option<String>,
}

impl ConfigDoc {
    pub fn open(src: &str) -> DocResult<Self> {
        let doc: DocumentMut = src
            .parse()
            .map_err(|e: toml_edit::TomlError| DocError(e.to_string()))?;
        Ok(Self {
            doc,
            undo: Vec::new(),
            redo: Vec::new(),
            coalescing: None,
        })
    }

    /// A minimal valid starting point for "new config".
    pub fn blank() -> Self {
        Self::open("config_version = 1\n").expect("blank document parses")
    }

    /// The live document. This is the file that would be written.
    pub fn text(&self) -> String {
        self.doc.to_string()
    }

    /// `RawConfig` projection as JSON, for rendering. Reuses the production
    /// parser rather than reading the document tree directly, so the editor and
    /// the daemon can never disagree about what a file means.
    pub fn view(&self) -> DocResult<String> {
        let text = self.text();
        let raw: lunchbox_config::RawConfig =
            toml::from_str(&text).map_err(|e| DocError(e.to_string()))?;
        serde_json::to_string(&raw).map_err(|e| DocError(e.to_string()))
    }

    pub fn validate(&self) -> String {
        let report = Report::of(&self.text());
        serde_json::to_string(&report).expect("report serializes")
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.text());
        self.doc = prev.parse().expect("undo snapshot reparses");
        self.coalescing = None;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.text());
        self.doc = next.parse().expect("redo snapshot reparses");
        self.coalescing = None;
        true
    }

    /// Ends the current coalescing gesture, so the next patch starts a fresh
    /// undo step. Called on pointer-up.
    pub fn end_gesture(&mut self) {
        self.coalescing = None;
    }

    /// Apply one patch. Returns whether the document actually changed — a `set`
    /// whose value already matches is a no-op, so a slider dragged away and
    /// back leaves the file byte-identical and costs no undo step.
    pub fn apply(&mut self, patch: &Patch, coalesce_key: Option<&str>) -> DocResult<bool> {
        let before = self.text();

        // Try the mutation on a scratch copy so a failure can't leave the
        // document half-edited.
        let mut scratch: DocumentMut = before.parse().expect("live document reparses");
        let changed = apply_to(&mut scratch, patch)?;
        if !changed {
            return Ok(false);
        }
        let after = scratch.to_string();
        if after == before {
            return Ok(false);
        }

        let extending = match (coalesce_key, self.coalescing.as_deref()) {
            (Some(k), Some(active)) => k == active,
            _ => false,
        };
        if !extending {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.coalescing = coalesce_key.map(str::to_owned);
        self.redo.clear();
        self.doc = scratch;
        Ok(true)
    }

    /// Replace the whole document, as when the raw-TOML pane is edited. Costs
    /// exactly one undo step however much changed.
    pub fn replace_text(&mut self, src: &str) -> DocResult<bool> {
        let doc: DocumentMut = src
            .parse()
            .map_err(|e: toml_edit::TomlError| DocError(e.to_string()))?;
        let before = self.text();
        let after = doc.to_string();
        if after == before {
            return Ok(false);
        }
        self.undo.push(before);
        self.redo.clear();
        self.coalescing = None;
        self.doc = doc;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

/// A mutable position in the document. Three variants because `toml_edit`
/// stores standard tables as `Item`, array-of-tables elements as `Table`, and
/// everything inside an inline table or array as `Value`.
enum Slot<'a> {
    Item(&'a mut Item),
    Table(&'a mut Table),
    Value(&'a mut Value),
}

impl<'a> Slot<'a> {
    fn child_key(self, key: &str) -> Option<Slot<'a>> {
        match self {
            Slot::Item(item) => match item {
                Item::Table(t) => t.get_mut(key).map(Slot::Item),
                Item::Value(Value::InlineTable(it)) => it.get_mut(key).map(Slot::Value),
                _ => None,
            },
            Slot::Table(t) => t.get_mut(key).map(Slot::Item),
            Slot::Value(v) => match v {
                Value::InlineTable(it) => it.get_mut(key).map(Slot::Value),
                _ => None,
            },
        }
    }

    fn child_index(self, i: usize) -> Option<Slot<'a>> {
        match self {
            Slot::Item(item) => match item {
                Item::ArrayOfTables(aot) => aot.get_mut(i).map(Slot::Table),
                Item::Value(Value::Array(a)) => a.get_mut(i).map(Slot::Value),
                _ => None,
            },
            Slot::Table(_) => None,
            Slot::Value(v) => match v {
                Value::Array(a) => a.get_mut(i).map(Slot::Value),
                _ => None,
            },
        }
    }

    fn child_id(self, id: &str) -> Option<Slot<'a>> {
        let i = self.find_id(id)?;
        self.child_index(i)
    }

    /// Index of the array element whose `id` field equals `id`.
    fn find_id(&self, id: &str) -> Option<usize> {
        match self {
            Slot::Item(Item::ArrayOfTables(aot)) => aot
                .iter()
                .position(|t| t.get("id").and_then(|i| i.as_str()) == Some(id)),
            Slot::Item(Item::Value(Value::Array(a))) | Slot::Value(Value::Array(a)) => {
                a.iter().position(|v| {
                    v.as_inline_table()
                        .and_then(|t| t.get("id"))
                        .and_then(|v| v.as_str())
                        == Some(id)
                })
            }
            _ => None,
        }
    }
}

/// Walk `segs` from the document root. When `create` is set, missing table keys
/// are created; the style of a created table follows its parent, so a child of
/// a standard table becomes a standard table and a child of an inline table
/// stays inline.
fn navigate<'a>(doc: &'a mut DocumentMut, segs: &[Seg], create: bool) -> DocResult<Slot<'a>> {
    let mut slot = Slot::Table(doc.as_table_mut());

    for (i, seg) in segs.iter().enumerate() {
        let here = || {
            segs[..=i]
                .iter()
                .map(|s| match s {
                    Seg::Key(k) => format!(".{k}"),
                    Seg::Index(n) => format!("[{n}]"),
                    Seg::Id(id) => format!("[id={id}]"),
                })
                .collect::<String>()
                .trim_start_matches('.')
                .to_string()
        };

        slot = match seg {
            Seg::Key(k) => {
                if create {
                    ensure_key(slot, k)?
                } else {
                    slot.child_key(k)
                        .ok_or_else(|| DocError(format!("no such key: {}", here())))?
                }
            }
            Seg::Index(n) => slot
                .child_index(*n)
                .ok_or_else(|| DocError(format!("no such element: {}", here())))?,
            Seg::Id(id) => slot
                .child_id(id)
                .ok_or_else(|| DocError(format!("no element with id: {}", here())))?,
        };
    }

    Ok(slot)
}

/// Get `key` from `slot`, creating an empty table of the parent's style if it
/// is missing or currently null.
fn ensure_key<'a>(slot: Slot<'a>, key: &str) -> DocResult<Slot<'a>> {
    match slot {
        Slot::Item(item) => match item {
            Item::Table(t) => {
                if !t.contains_key(key) || t[key].is_none() {
                    t.insert(key, Item::Table(Table::new()));
                }
                Ok(Slot::Item(t.get_mut(key).expect("just inserted")))
            }
            Item::Value(Value::InlineTable(it)) => {
                if it.get(key).is_none() {
                    it.insert(key, Value::InlineTable(InlineTable::new()));
                }
                Ok(Slot::Value(it.get_mut(key).expect("just inserted")))
            }
            other => err(format!(
                "cannot descend into {} to reach '{key}'",
                kind_of(other)
            )),
        },
        Slot::Table(t) => {
            if !t.contains_key(key) || t[key].is_none() {
                t.insert(key, Item::Table(Table::new()));
            }
            Ok(Slot::Item(t.get_mut(key).expect("just inserted")))
        }
        Slot::Value(v) => match v {
            Value::InlineTable(it) => {
                if it.get(key).is_none() {
                    it.insert(key, Value::InlineTable(InlineTable::new()));
                }
                Ok(Slot::Value(it.get_mut(key).expect("just inserted")))
            }
            other => err(format!(
                "cannot descend into {} to reach '{key}'",
                value_kind_of(other)
            )),
        },
    }
}

fn kind_of(item: &Item) -> &'static str {
    match item {
        Item::None => "nothing",
        Item::Value(v) => value_kind_of(v),
        Item::Table(_) => "a table",
        Item::ArrayOfTables(_) => "an array of tables",
    }
}

fn value_kind_of(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "a string",
        Value::Integer(_) => "an integer",
        Value::Float(_) => "a float",
        Value::Boolean(_) => "a boolean",
        Value::Datetime(_) => "a datetime",
        Value::Array(_) => "an array",
        Value::InlineTable(_) => "an inline table",
    }
}

// ---------------------------------------------------------------------------
// Patch application
// ---------------------------------------------------------------------------

fn apply_to(doc: &mut DocumentMut, patch: &Patch) -> DocResult<bool> {
    match patch {
        Patch::Set { path, value } => {
            let p = Path::parse(path).map_err(|e| DocError(e.to_string()))?;
            let (head, last) = p
                .split_last()
                .ok_or_else(|| DocError("empty path".into()))?;
            let parent = navigate(doc, head, true)?;
            set_child(parent, last, value)
        }
        Patch::Unset { path } => {
            let p = Path::parse(path).map_err(|e| DocError(e.to_string()))?;
            let (head, last) = p
                .split_last()
                .ok_or_else(|| DocError("empty path".into()))?;
            let parent = navigate(doc, head, false)?;
            unset_child(parent, last)
        }
        Patch::Insert { path, index, value } => {
            let p = Path::parse(path).map_err(|e| DocError(e.to_string()))?;
            let (head, last) = p
                .split_last()
                .ok_or_else(|| DocError("empty path".into()))?;
            let Seg::Key(key) = last else {
                return err("insert expects a path ending in an array key");
            };
            let parent = navigate(doc, head, true)?;
            insert_into(parent, key, *index, value)
        }
        Patch::Move { path, from, to } => {
            let p = Path::parse(path).map_err(|e| DocError(e.to_string()))?;
            let slot = navigate(doc, &p.0, false)?;
            move_within(slot, *from, *to)
        }
    }
}

/// Write `value` at `seg` under `parent`.
fn set_child(parent: Slot<'_>, seg: &Seg, value: &Json) -> DocResult<bool> {
    match seg {
        Seg::Key(key) => match parent {
            Slot::Item(Item::Table(t)) | Slot::Table(t) => set_in_table(t, key, value),
            Slot::Item(Item::Value(Value::InlineTable(it)))
            | Slot::Value(Value::InlineTable(it)) => set_in_inline(it, key, value),
            Slot::Item(other) => err(format!("cannot set '{key}' inside {}", kind_of(other))),
            Slot::Value(other) => err(format!(
                "cannot set '{key}' inside {}",
                value_kind_of(other)
            )),
        },
        Seg::Index(i) => set_at_index(parent, *i, value),
        Seg::Id(id) => {
            let Some(i) = parent.find_id(id) else {
                return err(format!("no element with id '{id}'"));
            };
            set_at_index(parent, i, value)
        }
    }
}

fn set_in_table(t: &mut Table, key: &str, value: &Json) -> DocResult<bool> {
    // Minimal write: leave the document alone when nothing actually changes.
    if let Some(existing) = t.get(key)
        && item_to_json(existing).as_ref() == Some(value)
    {
        return Ok(false);
    }

    // A table that is being replaced by another object keeps its style, so
    // `[entries.limits]` does not silently become an inline table.
    if value.is_object()
        && let Some(Item::Table(existing)) = t.get_mut(key)
    {
        return merge_object_into_table(existing, value);
    }

    let new = json_to_value(value)?;
    match t.get_mut(key) {
        Some(slot @ Item::Value(_)) => {
            replace_item_value(slot, new);
        }
        Some(slot) => {
            *slot = Item::Value(new);
        }
        None => {
            t.insert(key, Item::Value(new));
        }
    }
    Ok(true)
}

fn set_in_inline(it: &mut InlineTable, key: &str, value: &Json) -> DocResult<bool> {
    if let Some(existing) = it.get(key)
        && value_to_json(existing).as_ref() == Some(value)
    {
        return Ok(false);
    }
    let new = json_to_value(value)?;
    match it.get_mut(key) {
        Some(slot) => replace_value(slot, new),
        None => {
            it.insert(key, new);
        }
    }
    Ok(true)
}

fn set_at_index(parent: Slot<'_>, i: usize, value: &Json) -> DocResult<bool> {
    match parent {
        Slot::Item(Item::ArrayOfTables(aot)) => {
            let Some(existing) = aot.get_mut(i) else {
                return err(format!("index {i} out of range"));
            };
            if table_to_json(existing).as_ref() == Some(value) {
                return Ok(false);
            }
            merge_object_into_table(existing, value)
        }
        Slot::Item(Item::Value(Value::Array(a))) | Slot::Value(Value::Array(a)) => {
            let Some(existing) = a.get_mut(i) else {
                return err(format!("index {i} out of range"));
            };
            if value_to_json(existing).as_ref() == Some(value) {
                return Ok(false);
            }
            let new = json_to_value(value)?;
            replace_value(existing, new);
            Ok(true)
        }
        _ => err(format!("cannot index into a non-array to set [{i}]")),
    }
}

/// Apply an object onto an existing standard table key-by-key, so keys the
/// object doesn't mention (and their comments) stay put.
fn merge_object_into_table(t: &mut Table, value: &Json) -> DocResult<bool> {
    let Some(obj) = value.as_object() else {
        return err("expected an object");
    };
    let mut changed = false;
    for (k, v) in obj {
        // A null member is an absent key, as in `json_to_table` and
        // `json_to_value`. The editor's view spells every unset `Option` as
        // null, so a table it read and hands back is full of them (#192).
        if v.is_null() {
            changed |= t.remove(k).is_some();
            continue;
        }
        changed |= set_in_table(t, k, v)?;
    }
    // Keys present in the table but absent from the object are removed, which
    // is what makes `set` on a whole table a replacement rather than a merge.
    let stale: Vec<String> = t
        .iter()
        .map(|(k, _)| k.to_string())
        .filter(|k| !obj.contains_key(k))
        .collect();
    for k in stale {
        t.remove(&k);
        changed = true;
    }
    Ok(changed)
}

fn unset_child(parent: Slot<'_>, seg: &Seg) -> DocResult<bool> {
    match seg {
        Seg::Key(key) => match parent {
            Slot::Item(Item::Table(t)) | Slot::Table(t) => Ok(t.remove(key).is_some()),
            Slot::Item(Item::Value(Value::InlineTable(it)))
            | Slot::Value(Value::InlineTable(it)) => Ok(it.remove(key).is_some()),
            _ => err(format!("cannot unset '{key}' here")),
        },
        Seg::Index(i) => remove_at(parent, *i),
        Seg::Id(id) => {
            let Some(i) = parent.find_id(id) else {
                return Ok(false);
            };
            remove_at(parent, i)
        }
    }
}

fn remove_at(parent: Slot<'_>, i: usize) -> DocResult<bool> {
    match parent {
        Slot::Item(Item::ArrayOfTables(aot)) => {
            if i >= aot.len() {
                return err(format!("index {i} out of range"));
            }
            aot.remove(i);
            Ok(true)
        }
        Slot::Item(Item::Value(Value::Array(a))) | Slot::Value(Value::Array(a)) => {
            if i >= a.len() {
                return err(format!("index {i} out of range"));
            }
            a.remove(i);
            Ok(true)
        }
        _ => err(format!("cannot remove [{i}] from a non-array")),
    }
}

fn insert_into(parent: Slot<'_>, key: &str, index: Option<usize>, value: &Json) -> DocResult<bool> {
    // Arrays of objects become arrays of tables (`[[entries]]`), matching the
    // house style in config.example.toml; anything else becomes a plain array.
    let as_table = value.is_object();

    match parent {
        Slot::Item(Item::Table(t)) | Slot::Table(t) => {
            if !t.contains_key(key) || t[key].is_none() {
                t.insert(
                    key,
                    if as_table {
                        Item::ArrayOfTables(ArrayOfTables::new())
                    } else {
                        Item::Value(Value::Array(Array::new()))
                    },
                );
            }
            match t.get_mut(key).expect("present") {
                Item::ArrayOfTables(aot) => {
                    let table = json_to_table(value)?;
                    let at = index.unwrap_or(aot.len()).min(aot.len());
                    // ArrayOfTables has no splice; rebuild around the insertion.
                    let mut rebuilt = ArrayOfTables::new();
                    for (i, existing) in aot.iter().enumerate() {
                        if i == at {
                            rebuilt.push(table.clone());
                        }
                        rebuilt.push(existing.clone());
                    }
                    if at >= aot.len() {
                        rebuilt.push(table);
                    }
                    *aot = rebuilt;
                    Ok(true)
                }
                Item::Value(Value::Array(a)) => {
                    let v = json_to_value(value)?;
                    let at = index.unwrap_or(a.len()).min(a.len());
                    a.insert(at, v);
                    Ok(true)
                }
                other => err(format!("'{key}' is {}, not an array", kind_of(other))),
            }
        }
        Slot::Item(Item::Value(Value::InlineTable(it))) | Slot::Value(Value::InlineTable(it)) => {
            if it.get(key).is_none() {
                it.insert(key, Value::Array(Array::new()));
            }
            let Some(Value::Array(a)) = it.get_mut(key) else {
                return err(format!("'{key}' is not an array"));
            };
            let v = json_to_value(value)?;
            let at = index.unwrap_or(a.len()).min(a.len());
            a.insert(at, v);
            Ok(true)
        }
        _ => err(format!("cannot insert into '{key}' here")),
    }
}

fn move_within(slot: Slot<'_>, from: usize, to: usize) -> DocResult<bool> {
    if from == to {
        return Ok(false);
    }
    match slot {
        Slot::Item(Item::ArrayOfTables(aot)) => {
            if from >= aot.len() || to >= aot.len() {
                return err("move index out of range");
            }
            // Reordering the vector is not enough. Every table remembers the
            // position it was parsed at, and that is what decides render order
            // — so a rebuilt array whose tables kept their old positions comes
            // back out in the old order, and the edit silently does nothing.
            // Reassigning the same set of positions in the new sequence is a
            // permutation, so it cannot collide with anything else in the
            // document.
            let positions: Vec<Option<usize>> = aot.iter().map(|t| t.position()).collect();
            let mut tables: Vec<Table> = aot.iter().cloned().collect();
            let t = tables.remove(from);
            tables.insert(to, t);

            let mut rebuilt = ArrayOfTables::new();
            for (mut table, position) in tables.into_iter().zip(positions) {
                if let Some(position) = position {
                    table.set_position(position);
                }
                rebuilt.push(table);
            }
            *aot = rebuilt;
            Ok(true)
        }
        Slot::Item(Item::Value(Value::Array(a))) | Slot::Value(Value::Array(a)) => {
            if from >= a.len() || to >= a.len() {
                return err("move index out of range");
            }
            let v = a.remove(from);
            a.insert(to, v);
            Ok(true)
        }
        _ => err("cannot reorder a non-array"),
    }
}

// ---------------------------------------------------------------------------
// Value conversion
// ---------------------------------------------------------------------------

/// Replace an `Item::Value` in place, carrying the old value's decor across so
/// a trailing comment (`max_run_seconds = 3600  # one hour`) survives.
fn replace_item_value(slot: &mut Item, new: Value) {
    if let Item::Value(old) = slot {
        let decor = old.decor().clone();
        let mut new = new;
        *new.decor_mut() = decor;
        *slot = Item::Value(new);
    } else {
        *slot = Item::Value(new);
    }
}

fn replace_value(slot: &mut Value, new: Value) {
    let decor = slot.decor().clone();
    let mut new = new;
    *new.decor_mut() = decor;
    *slot = new;
}

fn json_to_value(j: &Json) -> DocResult<Value> {
    Ok(match j {
        Json::Null => return err("cannot write null; use unset"),
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::from(i)
            } else if let Some(f) = n.as_f64() {
                Value::from(f)
            } else {
                return err(format!("unrepresentable number: {n}"));
            }
        }
        Json::String(s) => Value::from(s.as_str()),
        Json::Array(items) => {
            let mut a = Array::new();
            for item in items {
                a.push(json_to_value(item)?);
            }
            Value::Array(a)
        }
        Json::Object(map) => {
            let mut it = InlineTable::new();
            for (k, v) in map {
                if v.is_null() {
                    continue;
                }
                it.insert(k, json_to_value(v)?);
            }
            Value::InlineTable(it)
        }
    })
}

fn json_to_table(j: &Json) -> DocResult<Table> {
    let Some(obj) = j.as_object() else {
        return err("expected an object");
    };
    let mut t = Table::new();
    for (k, v) in obj {
        if v.is_null() {
            continue;
        }
        // Nested objects become standard sub-tables, matching how entries write
        // `[entries.limits]` rather than an inline table.
        if v.is_object() {
            t.insert(k, Item::Table(json_to_table(v)?));
        } else {
            t.insert(k, Item::Value(json_to_value(v)?));
        }
    }
    Ok(t)
}

fn item_to_json(item: &Item) -> Option<Json> {
    match item {
        Item::None => None,
        Item::Value(v) => value_to_json(v),
        Item::Table(t) => table_to_json(t),
        Item::ArrayOfTables(aot) => {
            Some(Json::Array(aot.iter().filter_map(table_to_json).collect()))
        }
    }
}

fn table_to_json(t: &Table) -> Option<Json> {
    let mut map = serde_json::Map::new();
    for (k, v) in t.iter() {
        map.insert(k.to_string(), item_to_json(v)?);
    }
    Some(Json::Object(map))
}

fn value_to_json(v: &Value) -> Option<Json> {
    Some(match v {
        Value::String(s) => Json::String(s.value().clone()),
        Value::Integer(i) => Json::Number((*i.value()).into()),
        Value::Float(f) => serde_json::Number::from_f64(*f.value()).map(Json::Number)?,
        Value::Boolean(b) => Json::Bool(*b.value()),
        Value::Datetime(d) => Json::String(d.value().to_string()),
        Value::Array(a) => Json::Array(a.iter().filter_map(value_to_json).collect()),
        Value::InlineTable(it) => {
            let mut map = serde_json::Map::new();
            for (k, v) in it.iter() {
                map.insert(k.to_string(), value_to_json(v)?);
            }
            Json::Object(map)
        }
    })
}
