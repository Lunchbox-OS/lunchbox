//! Apply a CLI-selected ordering to a loaded `Library` in place.
//!
//! Pure transformation of `library.items` — no I/O, no UI coupling. The
//! sort is stable, so the library file order is the tiebreaker for any
//! key. `--reverse` is applied after sorting.

use std::cmp::Ordering;

use lunchbox_media_core::{ItemKind, Library};

use crate::cli::SortBy;

/// Sort `library.items` by `sort_by`, then reverse if requested.
///
/// For optional fields (`category`, `duration_seconds`) items with a value
/// sort before items without, so missing data ends up at the bottom of an
/// ascending sort (and the top of a reversed one).
pub fn apply_ordering(library: &mut Library, sort_by: SortBy, reverse: bool) {
    match sort_by {
        SortBy::Library => {}
        SortBy::Title => library.items.sort_by_key(|i| i.title.to_lowercase()),
        SortBy::Id => library.items.sort_by(|a, b| a.id.cmp(&b.id)),
        SortBy::Kind => library.items.sort_by_key(|i| match i.kind {
            ItemKind::Audio => 0,
            ItemKind::Video => 1,
        }),
        SortBy::Category => library
            .items
            .sort_by(|a, b| match (&a.category, &b.category) {
                (Some(ca), Some(cb)) => ca.to_lowercase().cmp(&cb.to_lowercase()),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }),
        SortBy::Duration => {
            library
                .items
                .sort_by(|a, b| match (a.duration_seconds, b.duration_seconds) {
                    (Some(da), Some(db)) => da.cmp(&db),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                })
        }
    }
    if reverse {
        library.items.reverse();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use lunchbox_media_core::{ClassifiedUri, Item, ItemKind, Library, Platform, Source};
    use url::Url;

    use super::*;

    fn item(
        id: &str,
        title: &str,
        kind: ItemKind,
        category: Option<&str>,
        dur: Option<u64>,
    ) -> Item {
        Item {
            id: id.to_string(),
            title: title.to_string(),
            kind,
            category: category.map(str::to_string),
            poster: None,
            duration_seconds: dur,
            sources: vec![Source {
                platforms: vec![Platform::Any],
                uri: ClassifiedUri::DirectHttp(Url::parse("https://example.com/x.mp4").unwrap()),
                player_hint: None,
            }],
        }
    }

    fn library(items: Vec<Item>) -> Library {
        Library {
            schema_version: 1,
            library_id: "demo".to_string(),
            title: "Demo".to_string(),
            items,
            source_path: PathBuf::from("/tmp/test.toml"),
        }
    }

    fn ids(lib: &Library) -> Vec<&str> {
        lib.items.iter().map(|i| i.id.as_str()).collect()
    }

    #[test]
    fn library_order_is_unchanged_by_default() {
        let mut lib = library(vec![
            item("c", "C", ItemKind::Video, None, None),
            item("a", "A", ItemKind::Video, None, None),
            item("b", "B", ItemKind::Video, None, None),
        ]);
        apply_ordering(&mut lib, SortBy::Library, false);
        assert_eq!(ids(&lib), vec!["c", "a", "b"]);
    }

    #[test]
    fn reverse_alone_flips_library_order() {
        let mut lib = library(vec![
            item("c", "C", ItemKind::Video, None, None),
            item("a", "A", ItemKind::Video, None, None),
            item("b", "B", ItemKind::Video, None, None),
        ]);
        apply_ordering(&mut lib, SortBy::Library, true);
        assert_eq!(ids(&lib), vec!["b", "a", "c"]);
    }

    #[test]
    fn title_sort_is_case_insensitive() {
        let mut lib = library(vec![
            item("1", "banana", ItemKind::Video, None, None),
            item("2", "Apple", ItemKind::Video, None, None),
            item("3", "cherry", ItemKind::Video, None, None),
        ]);
        apply_ordering(&mut lib, SortBy::Title, false);
        assert_eq!(ids(&lib), vec!["2", "1", "3"]);
    }

    #[test]
    fn duration_sort_puts_none_last() {
        let mut lib = library(vec![
            item("a", "A", ItemKind::Video, None, Some(300)),
            item("b", "B", ItemKind::Video, None, None),
            item("c", "C", ItemKind::Video, None, Some(60)),
        ]);
        apply_ordering(&mut lib, SortBy::Duration, false);
        assert_eq!(ids(&lib), vec!["c", "a", "b"]);
    }

    #[test]
    fn category_sort_puts_none_last_and_is_case_insensitive() {
        let mut lib = library(vec![
            item("a", "A", ItemKind::Video, Some("Zeta"), None),
            item("b", "B", ItemKind::Video, None, None),
            item("c", "C", ItemKind::Video, Some("alpha"), None),
        ]);
        apply_ordering(&mut lib, SortBy::Category, false);
        assert_eq!(ids(&lib), vec!["c", "a", "b"]);
    }

    #[test]
    fn kind_sort_puts_audio_before_video_and_is_stable() {
        let mut lib = library(vec![
            item("v1", "V1", ItemKind::Video, None, None),
            item("a1", "A1", ItemKind::Audio, None, None),
            item("v2", "V2", ItemKind::Video, None, None),
            item("a2", "A2", ItemKind::Audio, None, None),
        ]);
        apply_ordering(&mut lib, SortBy::Kind, false);
        assert_eq!(ids(&lib), vec!["a1", "a2", "v1", "v2"]);
    }

    #[test]
    fn sort_then_reverse_applies_in_that_order() {
        let mut lib = library(vec![
            item("a", "A", ItemKind::Video, None, Some(100)),
            item("b", "B", ItemKind::Video, None, Some(50)),
            item("c", "C", ItemKind::Video, None, None),
        ]);
        // ascending duration with None last → b, a, c; reversed → c, a, b
        apply_ordering(&mut lib, SortBy::Duration, true);
        assert_eq!(ids(&lib), vec!["c", "a", "b"]);
    }
}
