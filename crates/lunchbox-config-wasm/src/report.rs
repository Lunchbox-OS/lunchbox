//! Validation results, shaped for the UI.
//!
//! Mirrors what `lunchbox_config::parse_config` does — parse, version-check,
//! validate — but keeps the three failure modes distinct, because the editor
//! reacts to them differently. A syntax error has a caret position and blocks
//! everything; semantic errors are individually attributable to an activity.

use lunchbox_config::{CURRENT_CONFIG_VERSION, RawConfig, ValidationError, validate_config};
use serde::Serialize;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Report {
    /// The document is not valid TOML, or does not fit the schema's shape.
    Syntax {
        message: String,
        line: usize,
        column: usize,
    },
    /// Parsed, but written for a different schema version than this build knows.
    Version { found: u32, expected: u32 },
    /// Parsed and versioned correctly. An empty `errors` list means valid.
    Semantic { errors: Vec<Issue> },
}

/// Which `ValidationError` variant an issue came from.
///
/// An enum rather than the `&'static str` this started as, so the generated
/// TypeScript is the union of these eight names instead of a bare `string`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    /// Attributable to one activity.
    Entry,
    /// Attributable to one category.
    Group,
    /// Two activities share an id.
    DuplicateEntryId,
    /// Two categories share an id.
    DuplicateGroupId,
    /// A window's `start` or `end` is not `HH:MM`.
    InvalidTimeFormat,
    /// A window's `days` names something that is not a day.
    InvalidDaySpec,
    /// A warning fires after the session it belongs to would already have
    /// ended.
    WarningExceedsMaxRun,
    /// Belongs to no particular activity: service settings and the like.
    Global,
}

/// One validation error, flattened so the UI can index by activity.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Issue {
    /// Which `ValidationError` this came from.
    pub kind: IssueKind,
    /// Activity this is attributable to, when the error carries one.
    pub entry_id: Option<String>,
    /// Category this is attributable to, when the error carries one.
    pub group_id: Option<String>,
    /// The offending literal, for errors that carry one but no id. Lets the UI
    /// match a bad time or day string back to the field that holds it.
    pub value: Option<String>,
    /// Human-readable text, straight from the error's `Display`.
    pub message: String,
}

impl Report {
    pub fn of(text: &str) -> Report {
        let raw: RawConfig = match toml::from_str(text) {
            Ok(raw) => raw,
            Err(e) => {
                let (line, column) = e
                    .span()
                    .map(|s| offset_to_line_col(text, s.start))
                    .unwrap_or((1, 1));
                return Report::Syntax {
                    message: e.message().to_string(),
                    line,
                    column,
                };
            }
        };

        if raw.config_version != CURRENT_CONFIG_VERSION {
            return Report::Version {
                found: raw.config_version,
                expected: CURRENT_CONFIG_VERSION,
            };
        }

        Report::Semantic {
            errors: validate_config(&raw).iter().map(Issue::from).collect(),
        }
    }
}

impl From<&ValidationError> for Issue {
    fn from(e: &ValidationError) -> Issue {
        let message = e.to_string();
        match e {
            ValidationError::EntryError { entry_id, .. } => Issue {
                kind: IssueKind::Entry,
                entry_id: Some(entry_id.clone()),
                group_id: None,
                value: None,
                message,
            },
            ValidationError::DuplicateEntryId(id) => Issue {
                kind: IssueKind::DuplicateEntryId,
                entry_id: Some(id.clone()),
                group_id: None,
                value: None,
                message,
            },
            ValidationError::GroupError { group_id, .. } => Issue {
                kind: IssueKind::Group,
                entry_id: None,
                group_id: Some(group_id.clone()),
                value: None,
                message,
            },
            ValidationError::DuplicateGroupId(id) => Issue {
                kind: IssueKind::DuplicateGroupId,
                entry_id: None,
                group_id: Some(id.clone()),
                value: None,
                message,
            },
            ValidationError::InvalidTimeFormat { value, .. } => Issue {
                kind: IssueKind::InvalidTimeFormat,
                entry_id: None,
                group_id: None,
                value: Some(value.clone()),
                message,
            },
            ValidationError::InvalidDaySpec(value) => Issue {
                kind: IssueKind::InvalidDaySpec,
                entry_id: None,
                group_id: None,
                value: Some(value.clone()),
                message,
            },
            ValidationError::WarningExceedsMaxRun { entry_id, .. } => Issue {
                kind: IssueKind::WarningExceedsMaxRun,
                entry_id: Some(entry_id.clone()),
                group_id: None,
                value: None,
                message,
            },
            ValidationError::GlobalError(_) => Issue {
                kind: IssueKind::Global,
                entry_id: None,
                group_id: None,
                value: None,
                message,
            },
        }
    }
}

/// 1-based line and column for a byte offset.
fn offset_to_line_col(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before.rfind('\n').map(|i| offset - i).unwrap_or(offset + 1);
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_syntax_errors_with_a_position() {
        let r = Report::of("config_version = 1\nthis is not toml\n");
        match r {
            Report::Syntax { line, .. } => assert_eq!(line, 2),
            other => panic!("expected syntax error, got {other:?}"),
        }
    }

    #[test]
    fn reports_version_mismatch() {
        let r = Report::of("config_version = 99\n");
        match r {
            Report::Version { found, expected } => {
                assert_eq!(found, 99);
                assert_eq!(expected, CURRENT_CONFIG_VERSION);
            }
            other => panic!("expected version report, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_config_reports_no_issues() {
        let r = Report::of(
            r#"
            config_version = 1
            [[entries]]
            id = "a"
            label = "A"
            kind = { type = "process", command = "/bin/true" }
            "#,
        );
        match r {
            Report::Semantic { errors } => assert!(errors.is_empty(), "{errors:?}"),
            other => panic!("expected semantic report, got {other:?}"),
        }
    }

    #[test]
    fn semantic_errors_carry_their_entry() {
        let r = Report::of(
            r#"
            config_version = 1
            [[entries]]
            id = "a"
            label = "A"
            kind = { type = "process", command = "/bin/true" }
            [entries.availability]
            [[entries.availability.windows]]
            days = "weekdays"
            start = "not-a-time"
            end = "18:00"
            "#,
        );
        match r {
            Report::Semantic { errors } => {
                assert!(!errors.is_empty());
                assert!(
                    errors.iter().any(|e| e.entry_id.as_deref() == Some("a")
                        || e.value.as_deref() == Some("not-a-time")),
                    "expected the bad time to be attributable: {errors:?}"
                );
            }
            other => panic!("expected semantic report, got {other:?}"),
        }
    }

    #[test]
    fn line_col_counts_from_one() {
        assert_eq!(offset_to_line_col("abc", 0), (1, 1));
        assert_eq!(offset_to_line_col("abc\ndef", 4), (2, 1));
        assert_eq!(offset_to_line_col("abc\ndef", 6), (2, 3));
    }
}
