//! The Rust type strings the RPC schema records, rendered as client types.
//!
//! `#[management_rpc]` writes each parameter and result type into
//! `RPC_SCHEMA_JSON` as the source text from the trait signature — `EntryId`,
//! `Option<u8>`, `DateTime<Local>` — rather than as JSON Schema. That is what
//! kept the request half of the protocol out of codegen: the renderers in
//! [`crate::ts_types`] and [`crate::kotlin_types`] walk a schema, and there was
//! nothing here to hand them.
//!
//! The gap turns out to be small, because every non-primitive name in those
//! strings is already a key in [`crate::wire_schema::wire_schema`]'s `$defs` —
//! the wire schema is keyed by Rust type name, so the join is the name itself.
//! `window_action`, `stop_mode` and `display_mode` are rooted in `WireTypes`
//! for exactly this reason: they appear only as RPC parameters, never nested in
//! a response.
//!
//! So this module handles what is left: the primitives, `chrono`'s two
//! string-shaped types, and one level of `Option`/`Vec` unwrapping. Everything
//! else is a name passed straight through to the type the existing renderers
//! already emit.

use std::collections::BTreeSet;

/// A parsed RPC parameter or result type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustType {
    /// `()` — serde writes `null`.
    Unit,
    Bool,
    /// An integer that fits a Kotlin `Int`.
    Int,
    /// An integer that needs a Kotlin `Long`.
    Long,
    Float,
    Str,
    /// `DateTime<Local>`, an RFC 3339 string on the wire.
    Timestamp,
    /// `NaiveDate`, a `YYYY-MM-DD` string on the wire.
    Date,
    Option(Box<RustType>),
    Vec(Box<RustType>),
    /// A named wire type, resolved against the wire schema's `$defs`.
    Named(String),
}

impl RustType {
    /// Parse one type string from the RPC schema.
    ///
    /// Unknown names become [`RustType::Named`] rather than an error: a new
    /// wire type should generate on the strength of being in `$defs`, and one
    /// that is *not* there fails loudly at the point of rendering (an
    /// undefined TypeScript import, an unresolved Kotlin symbol) rather than
    /// silently here.
    pub fn parse(raw: &str) -> RustType {
        let s = raw.trim();

        if s == "()" {
            return RustType::Unit;
        }
        if let Some(inner) = wrapped(s, "Option") {
            return RustType::Option(Box::new(RustType::parse(inner)));
        }
        if let Some(inner) = wrapped(s, "Vec") {
            return RustType::Vec(Box::new(RustType::parse(inner)));
        }

        match s {
            "bool" => RustType::Bool,
            // Rust's 64-bit integers do not fit a Kotlin `Int`, and `usize` is
            // 64-bit on every target this daemon runs on. JSON has one number
            // type, so this distinction only reaches the Kotlin side.
            "i64" | "u64" | "usize" | "isize" => RustType::Long,
            "i8" | "i16" | "i32" | "u8" | "u16" | "u32" => RustType::Int,
            "f32" | "f64" => RustType::Float,
            "String" | "&str" | "str" => RustType::Str,
            "DateTime<Local>" | "DateTime<Utc>" => RustType::Timestamp,
            "NaiveDate" => RustType::Date,
            other => RustType::Named(other.to_string()),
        }
    }

    /// The TypeScript type, using the aliases the wire mirror declares.
    pub fn ts(&self) -> String {
        match self {
            RustType::Unit => "null".to_string(),
            RustType::Bool => "boolean".to_string(),
            RustType::Int | RustType::Long | RustType::Float => "number".to_string(),
            RustType::Str => "string".to_string(),
            RustType::Timestamp => "IsoTimestamp".to_string(),
            RustType::Date => "IsoDate".to_string(),
            RustType::Option(inner) => format!("{} | null", inner.ts()),
            // Parenthesise a union before `[]`, which binds tighter.
            RustType::Vec(inner) => {
                let rendered = inner.ts();
                if rendered.contains('|') {
                    format!("({rendered})[]")
                } else {
                    format!("{rendered}[]")
                }
            }
            RustType::Named(name) => name.clone(),
        }
    }

    /// The Kotlin type, using the typealiases the wire mirror declares.
    pub fn kotlin(&self) -> String {
        match self {
            RustType::Unit => "Unit".to_string(),
            RustType::Bool => "Boolean".to_string(),
            RustType::Int => "Int".to_string(),
            RustType::Long => "Long".to_string(),
            RustType::Float => "Double".to_string(),
            RustType::Str => "String".to_string(),
            RustType::Timestamp => "IsoTimestamp".to_string(),
            RustType::Date => "IsoDate".to_string(),
            RustType::Option(inner) => format!("{}?", inner.kotlin()),
            RustType::Vec(inner) => format!("List<{}>", inner.kotlin()),
            RustType::Named(name) => name.clone(),
        }
    }

    /// Names this type mentions that the wire mirror declares, for import
    /// lists. Includes the `Iso*` aliases, which are declared there too.
    pub fn imports(&self, out: &mut BTreeSet<String>) {
        match self {
            RustType::Timestamp => {
                out.insert("IsoTimestamp".to_string());
            }
            RustType::Date => {
                out.insert("IsoDate".to_string());
            }
            RustType::Named(name) => {
                out.insert(name.clone());
            }
            RustType::Option(inner) | RustType::Vec(inner) => inner.imports(out),
            _ => {}
        }
    }
}

/// `wrapped("Option<u8>", "Option")` is `Some("u8")`.
fn wrapped<'a>(s: &'a str, ctor: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(ctor)?.strip_prefix('<')?.strip_suffix('>')?;
    Some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_primitives() {
        assert_eq!(RustType::parse("bool"), RustType::Bool);
        assert_eq!(RustType::parse("u8"), RustType::Int);
        assert_eq!(RustType::parse("i64"), RustType::Long);
        assert_eq!(RustType::parse("usize"), RustType::Long);
        assert_eq!(RustType::parse("f64"), RustType::Float);
        assert_eq!(RustType::parse("String"), RustType::Str);
        assert_eq!(RustType::parse("()"), RustType::Unit);
    }

    #[test]
    fn chrono_types_are_strings_on_the_wire() {
        assert_eq!(RustType::parse("DateTime<Local>").ts(), "IsoTimestamp");
        assert_eq!(RustType::parse("NaiveDate").ts(), "IsoDate");
        assert_eq!(RustType::parse("NaiveDate").kotlin(), "IsoDate");
    }

    #[test]
    fn unwraps_option_and_vec() {
        assert_eq!(RustType::parse("Option<u8>").ts(), "number | null");
        assert_eq!(RustType::parse("Vec<EntryView>").ts(), "EntryView[]");
        assert_eq!(RustType::parse("Option<u8>").kotlin(), "Int?");
        assert_eq!(
            RustType::parse("Vec<EntryView>").kotlin(),
            "List<EntryView>"
        );
    }

    #[test]
    fn unwraps_a_nested_generic() {
        // The one result type in the schema that nests two deep.
        let t = RustType::parse("Option<DateTime<Local>>");
        assert_eq!(t.ts(), "IsoTimestamp | null");
        assert_eq!(t.kotlin(), "IsoTimestamp?");
    }

    #[test]
    fn a_union_inside_an_array_is_parenthesised() {
        assert_eq!(RustType::parse("Vec<Option<u8>>").ts(), "(number | null)[]");
    }

    #[test]
    fn unknown_names_pass_through() {
        assert_eq!(
            RustType::parse("EntryView"),
            RustType::Named("EntryView".to_string())
        );
    }

    #[test]
    fn imports_reach_through_generics() {
        let mut names = BTreeSet::new();
        RustType::parse("Vec<EntryView>").imports(&mut names);
        RustType::parse("Option<DateTime<Local>>").imports(&mut names);
        RustType::parse("u8").imports(&mut names);
        assert_eq!(
            names.into_iter().collect::<Vec<_>>(),
            ["EntryView", "IsoTimestamp"]
        );
    }
}
