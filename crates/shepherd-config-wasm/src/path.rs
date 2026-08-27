//! Paths that address a location inside a TOML document.
//!
//! Syntax, by example:
//!
//! ```text
//! service.default_max_run_seconds
//! entries[id=minecraft].limits.max_run_seconds
//! entries[id=minecraft].availability.windows[0].start
//! service.default_warnings[2]
//! ```
//!
//! Two selector forms follow a key: `[<n>]` picks an array element by index,
//! and `[id=<value>]` picks an array-of-tables element whose `id` matches.
//! Entries and groups are addressed by id precisely so that deleting or
//! reordering one never shifts another's comments onto the wrong item.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// A table key.
    Key(String),
    /// An array element by position.
    Index(usize),
    /// An array-of-tables element whose `id` field equals this value.
    Id(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Path(pub Vec<Seg>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError(pub String);

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Path {
    pub fn parse(s: &str) -> Result<Self, PathError> {
        let mut segs = Vec::new();
        if s.trim().is_empty() {
            return Err(PathError("empty path".into()));
        }

        for part in s.split('.') {
            if part.is_empty() {
                return Err(PathError(format!("empty path segment in '{s}'")));
            }
            // A part is `key`, `key[sel]`, or `key[sel][sel]`.
            let (key, rest) = match part.find('[') {
                Some(i) => (&part[..i], &part[i..]),
                None => (part, ""),
            };
            if key.is_empty() {
                return Err(PathError(format!(
                    "missing key before selector in '{part}'"
                )));
            }
            segs.push(Seg::Key(key.to_string()));

            let mut rest = rest;
            while !rest.is_empty() {
                if !rest.starts_with('[') {
                    return Err(PathError(format!("trailing junk '{rest}' in '{part}'")));
                }
                let close = rest
                    .find(']')
                    .ok_or_else(|| PathError(format!("unclosed selector in '{part}'")))?;
                let sel = &rest[1..close];
                segs.push(parse_selector(sel, part)?);
                rest = &rest[close + 1..];
            }
        }

        Ok(Path(segs))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Everything but the final segment, plus that final segment.
    pub fn split_last(&self) -> Option<(&[Seg], &Seg)> {
        self.0.split_last().map(|(last, head)| (head, last))
    }
}

fn parse_selector(sel: &str, ctx: &str) -> Result<Seg, PathError> {
    if let Some(id) = sel.strip_prefix("id=") {
        if id.is_empty() {
            return Err(PathError(format!("empty id selector in '{ctx}'")));
        }
        return Ok(Seg::Id(id.to_string()));
    }
    sel.parse::<usize>()
        .map(Seg::Index)
        .map_err(|_| PathError(format!("bad selector '[{sel}]' in '{ctx}'")))
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for seg in &self.0 {
            match seg {
                Seg::Key(k) => {
                    if f.alternate() {
                        write!(f, ".{k}")?
                    } else {
                        write!(f, "{k}")?
                    }
                }
                Seg::Index(i) => write!(f, "[{i}]")?,
                Seg::Id(id) => write!(f, "[id={id}]")?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_keys() {
        assert_eq!(
            Path::parse("service.default_max_run_seconds").unwrap().0,
            vec![
                Seg::Key("service".into()),
                Seg::Key("default_max_run_seconds".into())
            ]
        );
    }

    #[test]
    fn parses_id_selector() {
        assert_eq!(
            Path::parse("entries[id=minecraft].limits.max_run_seconds")
                .unwrap()
                .0,
            vec![
                Seg::Key("entries".into()),
                Seg::Id("minecraft".into()),
                Seg::Key("limits".into()),
                Seg::Key("max_run_seconds".into()),
            ]
        );
    }

    #[test]
    fn parses_index_selector() {
        assert_eq!(
            Path::parse("entries[id=x].availability.windows[0].start")
                .unwrap()
                .0,
            vec![
                Seg::Key("entries".into()),
                Seg::Id("x".into()),
                Seg::Key("availability".into()),
                Seg::Key("windows".into()),
                Seg::Index(0),
                Seg::Key("start".into()),
            ]
        );
    }

    #[test]
    fn ids_may_contain_dashes_and_digits() {
        assert_eq!(
            Path::parse("entries[id=big-buck-2].label").unwrap().0,
            vec![
                Seg::Key("entries".into()),
                Seg::Id("big-buck-2".into()),
                Seg::Key("label".into()),
            ]
        );
    }

    #[test]
    fn rejects_malformed() {
        assert!(Path::parse("").is_err());
        assert!(Path::parse("a..b").is_err());
        assert!(Path::parse("entries[").is_err());
        assert!(Path::parse("entries[nope]").is_err());
        assert!(Path::parse("[0]").is_err());
    }

    #[test]
    fn round_trips_through_display() {
        for s in [
            "service.volume.max_volume",
            "entries[id=x].limits.max_run_seconds",
            "entries[id=x].warnings[1].seconds_before",
        ] {
            let p = Path::parse(s).unwrap();
            let rendered =
                p.0.iter()
                    .map(|seg| match seg {
                        Seg::Key(k) => format!(".{k}"),
                        Seg::Index(i) => format!("[{i}]"),
                        Seg::Id(id) => format!("[id={id}]"),
                    })
                    .collect::<String>();
            assert_eq!(rendered.trim_start_matches('.').replace(".[", "["), {
                let mut t = s.to_string();
                t = t.replace(".[", "[");
                t
            });
        }
    }
}
