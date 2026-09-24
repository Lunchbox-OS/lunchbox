//! Remove one NetworkManager Wi-Fi definition from netplan's YAML, and nothing
//! else (issue #194).
//!
//! On Ubuntu, NetworkManager stores a profile as a netplan definition named
//! `NM-<uuid>`, in `/etc/netplan/90-NM-<uuid>.yaml`. Saving one writes only
//! that file. Deleting one does not: NetworkManager calls
//! `netplan_delete_connection`, which parses every file in the hierarchy and
//! writes each of them back out from what it parsed. Every comment in
//! `/etc/netplan` is lost, and a file whose definitions a later file also
//! mentions is unlinked, because netplan credits them to the later file. That
//! is how a forget once removed the installer's `00-installer-config.yaml`.
//!
//! So forgetting a network does not go through NetworkManager's delete. This
//! crate finds the definition, removes that one key with a lossless editor,
//! and leaves every other byte of every file as it was.
//!
//! Every edit is checked three ways before it is allowed near the disk, because
//! this runs as root and the editor is young:
//!
//! 1. **The meaning is exactly the old meaning, minus the definition.** Both
//!    texts are parsed by `yaml-rust2`, a separate and mature parser, and
//!    compared. The editor's own parser never gets the last word.
//! 2. **The new text is the old text with one piece cut out.** Nothing is
//!    reformatted, requoted or reindented.
//! 3. **No comment is cut except those inside what was removed.** A comment
//!    just above the definition stays, orphaned rather than guessed at.
//!
//! An edit that fails any of them is refused, and nothing is written.

pub mod files;

use std::fmt;
use std::str::FromStr;

use yaml_rust2::{Yaml, YamlLoader};

/// The netplan type a NetworkManager Wi-Fi profile is stored under.
const WIFIS: &str = "wifis";

/// The netplan definition id NetworkManager gives the profile with `uuid`.
pub fn netdef_id(uuid: &str) -> String {
    format!("NM-{uuid}")
}

/// The file NetworkManager writes a profile to, and nothing else to.
pub fn own_file_name(uuid: &str) -> String {
    format!("90-NM-{uuid}.yaml")
}

/// `s`, if it is a UUID in the form NetworkManager writes: 36 characters,
/// lowercase hex, hyphens at 8, 13, 18 and 23.
///
/// Strict on purpose. The argument comes from a systemd unit instance name,
/// and it ends up in file names and in the key this crate deletes.
pub fn parse_uuid(s: &str) -> Option<&str> {
    let ok = s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        });
    ok.then_some(s)
}

/// What removing a definition from one file comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    /// The file does not define it.
    Absent,
    /// The file's new text.
    Rewrite(String),
    /// Nothing but `version` would be left, in the file NetworkManager wrote
    /// for this profile alone. Remove the file, as NetworkManager would.
    Unlink,
}

/// Why a file cannot be edited. Nothing is written when any file refuses.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// netplan could not read this file either, so the device's network
    /// configuration is already broken, and editing it would be a guess.
    Unparseable(String),
    /// More than one YAML document. netplan reads the first; which one a
    /// person meant is not something to guess at as root.
    SeveralDocuments,
    /// The id is defined, but not as a Wi-Fi network.
    NotWifi(String),
    /// The lossless editor could not find what the parser did.
    NotFound,
    /// The edit changed something other than the definition.
    Unverified(&'static str),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unparseable(e) => write!(f, "it is not valid YAML ({e})"),
            Self::SeveralDocuments => write!(f, "it holds more than one YAML document"),
            Self::NotWifi(kind) => write!(f, "it defines the id under `{kind}`, not `{WIFIS}`"),
            Self::NotFound => write!(f, "the editor could not find the definition the parser did"),
            Self::Unverified(why) => write!(f, "the edit would have {why}"),
        }
    }
}

/// Remove the definition `id` from `text`.
///
/// `own_file` says whether this is the file NetworkManager wrote for the
/// profile alone (see [`own_file_name`]); only that one is ever unlinked. Any
/// other file keeps whatever is left, comments included, however little that
/// is.
pub fn remove_definition(text: &str, id: &str, own_file: bool) -> Result<Edit, Refusal> {
    let docs = YamlLoader::load_from_str(text).map_err(|e| Refusal::Unparseable(e.to_string()))?;
    let Some(before) = docs.first() else {
        return Ok(Edit::Absent);
    };
    let Some(kind) = defined_under(before, id) else {
        // Not in the first document. A later one is invisible to netplan, so a
        // definition there is not a definition.
        return Ok(Edit::Absent);
    };
    if docs.len() > 1 {
        return Err(Refusal::SeveralDocuments);
    }
    if kind != WIFIS {
        return Err(Refusal::NotWifi(kind));
    }

    let expected = without(before, id);
    let after = excise(text, id)?;
    verify(text, &after, &expected)?;

    if own_file && is_bare(&expected) {
        return Ok(Edit::Unlink);
    }
    Ok(Edit::Rewrite(after.text))
}

/// The netplan type `id` is defined under, if it is.
fn defined_under(doc: &Yaml, id: &str) -> Option<String> {
    let network = doc["network"].as_hash()?;
    network.iter().find_map(|(kind, defs)| {
        defs.as_hash()?
            .contains_key(&Yaml::String(id.into()))
            .then(|| kind.as_str().unwrap_or("?").to_owned())
    })
}

/// `doc` with the definition removed, and with `wifis` removed too if that
/// leaves it empty: the meaning the edited text must have.
fn without(doc: &Yaml, id: &str) -> Yaml {
    let mut doc = doc.clone();
    if let Yaml::Hash(root) = &mut doc
        && let Some(Yaml::Hash(network)) = root.get_mut(&Yaml::String("network".into()))
    {
        let wifis_key = Yaml::String(WIFIS.into());
        if let Some(Yaml::Hash(wifis)) = network.get_mut(&wifis_key) {
            wifis.remove(&Yaml::String(id.into()));
            if wifis.is_empty() {
                network.remove(&wifis_key);
            }
        }
    }
    doc
}

/// Whether nothing is left but `network: {version: …}`.
fn is_bare(doc: &Yaml) -> bool {
    let Some(root) = doc.as_hash() else {
        return false;
    };
    root.len() == 1
        && doc["network"]
            .as_hash()
            .is_some_and(|network| network.keys().all(|k| k.as_str() == Some("version")))
}

/// The edited text, and the source text of the entry the edit removed: the
/// definition, or the `wifis` block the definition was the last of.
struct Excised {
    text: String,
    removed_entry: String,
}

/// Remove the definition with the lossless editor.
fn excise(text: &str, id: &str) -> Result<Excised, Refusal> {
    let file = yaml_edit::YamlFile::from_str(text).map_err(|_| Refusal::NotFound)?;
    let network = file
        .document()
        .and_then(|doc| doc.as_mapping())
        .and_then(|root| root.get_mapping("network"))
        .ok_or(Refusal::NotFound)?;
    let wifis = network.get_mapping(WIFIS).ok_or(Refusal::NotFound)?;

    // Read before the edit: once removed, an entry has no text to read.
    let definition = wifis
        .find_entry_by_key(id)
        .ok_or(Refusal::NotFound)?
        .to_string();
    let parent = network
        .find_entry_by_key(WIFIS)
        .ok_or(Refusal::NotFound)?
        .to_string();

    wifis.remove(id).ok_or(Refusal::NotFound)?;
    let removed_entry = if wifis.is_empty() {
        // An empty `wifis:` is a null, which netplan rejects. It goes too, and
        // with it anything inside it, which by now is only whitespace and
        // comments about the definition.
        network.remove(WIFIS).ok_or(Refusal::NotFound)?;
        parent
    } else {
        definition
    };
    Ok(Excised {
        text: file.to_string(),
        removed_entry,
    })
}

/// The three checks in the module documentation.
fn verify(before: &str, after: &Excised, expected: &Yaml) -> Result<(), Refusal> {
    let parsed = YamlLoader::load_from_str(&after.text)
        .map_err(|_| Refusal::Unverified("left text that does not parse"))?;
    if parsed.len() != 1 || &parsed[0] != expected {
        return Err(Refusal::Unverified("changed the meaning of something else"));
    }

    let cut = cut_out(before, &after.text)
        .ok_or(Refusal::Unverified("rewritten text outside the definition"))?;
    let Some(rest) = cut.split_once(after.removed_entry.as_str()) else {
        return Err(Refusal::Unverified(
            "removed text that is not the definition",
        ));
    };
    if rest.0.contains('#') || rest.1.contains('#') {
        return Err(Refusal::Unverified(
            "removed a comment outside the definition",
        ));
    }
    Ok(())
}

/// The one contiguous piece of `before` whose removal gives `after`, if there
/// is one.
fn cut_out<'a>(before: &'a str, after: &str) -> Option<&'a str> {
    if after.len() >= before.len() {
        return None;
    }
    let prefix = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    // The piece starts where the prefix ends, but no later than where the
    // suffix begins: a run of repeated characters matches either way.
    let cut = before.len() - after.len();
    let start = prefix.min(after.len());
    let start = (0..=start).rev().find(|&s| {
        before.is_char_boundary(s)
            && before.is_char_boundary(s + cut)
            && before[s + cut..] == after[s..]
    })?;
    (before[..start] == after[..start]).then(|| &before[start..start + cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "11111111-2222-3333-4444-555555555555";

    fn id() -> String {
        netdef_id(UUID)
    }

    fn rewrite(text: &str) -> String {
        match remove_definition(text, &id(), false) {
            Ok(Edit::Rewrite(t)) => t,
            other => panic!("expected a rewrite, got {other:?}"),
        }
    }

    /// What NetworkManager writes to `90-NM-<uuid>.yaml` on 1.54.3, as
    /// measured (`docs/ai/history/2026-09-21 004`).
    const OWN_FILE: &str = r#"network:
  version: 2
  wifis:
    NM-11111111-2222-3333-4444-555555555555:
      renderer: NetworkManager
      match:
        name: "wlan0"
      dhcp4: true
      dhcp6: true
      access-points:
        "lunchbox-hwsimtest":
          auth:
            key-management: "psk"
            password: "correcthorse"
          networkmanager:
            uuid: "11111111-2222-3333-4444-555555555555"
            name: "lunchbox-hwsimtest"
"#;

    #[test]
    fn a_profiles_own_file_is_unlinked() {
        assert_eq!(remove_definition(OWN_FILE, &id(), true), Ok(Edit::Unlink));
    }

    #[test]
    fn someone_elses_file_is_never_unlinked_however_little_is_left() {
        assert_eq!(rewrite(OWN_FILE), "network:\n  version: 2\n");
    }

    #[test]
    fn a_file_that_does_not_define_it_is_left_alone() {
        // The installer's file, as subiquity writes it: the case netplan's own
        // delete once unlinked.
        let installer = "# This is the network config written by 'subiquity'\n\
                         network:\n  ethernets:\n    enp1s0:\n      dhcp4: true\n  version: 2\n";
        assert_eq!(remove_definition(installer, &id(), false), Ok(Edit::Absent));
    }

    #[test]
    fn everything_around_the_definition_survives_byte_for_byte() {
        let before = "# managed by hand\n\
                      network:\n  version: 2\n  wifis:\n\
                      \x20   # the kitchen AP\n\
                      \x20   NM-11111111-2222-3333-4444-555555555555:\n      dhcp4: true\n\
                      \x20     access-points:\n        \"home\":\n          password: \"x # y\"\n\
                      \x20   # keep this one\n    wlan-static:\n      dhcp4: true\n\
                      \x20 ethernets:\n    enp1s0: {dhcp4: true}  # trailing\n";
        let after = rewrite(before);
        assert_eq!(
            after,
            "# managed by hand\n\
             network:\n  version: 2\n  wifis:\n\
             \x20   # the kitchen AP\n\
             \x20   # keep this one\n    wlan-static:\n      dhcp4: true\n\
             \x20 ethernets:\n    enp1s0: {dhcp4: true}  # trailing\n",
            "only the definition goes; the comment above it stays, orphaned"
        );
    }

    #[test]
    fn an_emptied_wifis_block_goes_with_it() {
        // A null `wifis:` is something netplan refuses to load.
        let before = "network:\n  version: 2 # v\n  wifis:\n    \
                      NM-11111111-2222-3333-4444-555555555555:\n      dhcp4: true\n\
                      \x20 # ethernet below\n  ethernets:\n    enp1s0:\n      dhcp4: true\n";
        assert_eq!(
            rewrite(before),
            "network:\n  version: 2 # v\n  # ethernet below\n  ethernets:\n    enp1s0:\n      dhcp4: true\n"
        );
    }

    #[test]
    fn a_quoted_key_is_the_same_key() {
        let before = "network:\n  wifis:\n    \"NM-11111111-2222-3333-4444-555555555555\":\n      \
                      dhcp4: true\n    other: {dhcp4: true}\n";
        assert_eq!(
            rewrite(before),
            "network:\n  wifis:\n    other: {dhcp4: true}\n"
        );
    }

    #[test]
    fn flow_style_is_edited_in_place() {
        let before = "network: {version: 2, wifis: {NM-11111111-2222-3333-4444-555555555555: \
                      {dhcp4: true}, other: {dhcp4: false}}}\n";
        assert_eq!(
            rewrite(before),
            "network: {version: 2, wifis: {other: {dhcp4: false}}}\n"
        );
    }

    #[test]
    fn a_definition_of_another_kind_is_refused() {
        let before = "network:\n  ethernets:\n    NM-11111111-2222-3333-4444-555555555555:\n      \
                      dhcp4: true\n";
        assert_eq!(
            remove_definition(before, &id(), false),
            Err(Refusal::NotWifi("ethernets".into()))
        );
    }

    #[test]
    fn a_file_netplan_cannot_read_is_refused() {
        let before = "network:\n  wifis:\n    NM-11111111-2222-3333-4444-555555555555: [\n";
        assert!(matches!(
            remove_definition(before, &id(), false),
            Err(Refusal::Unparseable(_))
        ));
    }

    #[test]
    fn a_duplicated_key_is_refused_rather_than_half_removed() {
        let before = "network:\n  wifis:\n    NM-11111111-2222-3333-4444-555555555555: {}\n    \
                      NM-11111111-2222-3333-4444-555555555555: {}\n";
        assert!(matches!(
            remove_definition(before, &id(), false),
            Err(Refusal::Unparseable(_))
        ));
    }

    #[test]
    fn a_definition_something_else_points_at_is_refused() {
        // Removing an anchored definition would leave a dangling alias.
        let before = "network:\n  wifis:\n    NM-11111111-2222-3333-4444-555555555555: &ap\n      \
                      dhcp4: true\n    other: *ap\n";
        assert!(remove_definition(before, &id(), false).is_err());
    }

    #[test]
    fn a_second_document_is_refused() {
        let before = "network:\n  wifis:\n    NM-11111111-2222-3333-4444-555555555555: {}\n\
                      ---\nnetwork: {}\n";
        assert_eq!(
            remove_definition(before, &id(), false),
            Err(Refusal::SeveralDocuments)
        );
    }

    /// Feed `verify` an edit the editor did not make, to show each check can
    /// refuse. The editor passing every test above proves nothing about the
    /// checks, only that this version of it behaves.
    fn verdict(before: &str, after: &str, removed_entry: &str) -> Result<(), Refusal> {
        let expected = without(&YamlLoader::load_from_str(before).unwrap()[0], &id());
        let after = Excised {
            text: after.into(),
            removed_entry: removed_entry.into(),
        };
        verify(before, &after, &expected)
    }

    const TWO: &str = "network:\n  wifis:\n    # mine\n    \
                       NM-11111111-2222-3333-4444-555555555555: {dhcp4: true}\n    \
                       other: {dhcp4: true}\n";
    const ENTRY: &str = "NM-11111111-2222-3333-4444-555555555555: {dhcp4: true}\n";

    #[test]
    fn verification_accepts_the_edit_it_should() {
        let after = "network:\n  wifis:\n    # mine\n    other: {dhcp4: true}\n";
        assert_eq!(verdict(TWO, after, ENTRY), Ok(()));
    }

    #[test]
    fn verification_refuses_an_edit_that_changes_meaning() {
        let after = "network:\n  wifis:\n    # mine\n    other: {dhcp4: tru}\n";
        assert!(matches!(
            verdict(TWO, after, ENTRY),
            Err(Refusal::Unverified(_))
        ));
    }

    #[test]
    fn verification_refuses_a_reformat_that_keeps_the_meaning() {
        // What netplan's own delete does to every file.
        let after = "network:\n  wifis:\n    other:\n      dhcp4: true\n";
        assert!(matches!(
            verdict(TWO, after, ENTRY),
            Err(Refusal::Unverified(_))
        ));
    }

    #[test]
    fn verification_refuses_losing_a_comment_outside_the_definition() {
        let after = "network:\n  wifis:\n    other: {dhcp4: true}\n";
        assert_eq!(
            verdict(TWO, after, ENTRY),
            Err(Refusal::Unverified(
                "removed a comment outside the definition"
            ))
        );
    }

    #[test]
    fn verification_refuses_a_cut_that_is_not_the_definition() {
        let after = "network:\n  wifis:\n    # mine\n    other: {dhcp4: true}\n";
        assert!(matches!(
            verdict(TWO, after, "something: else\n"),
            Err(Refusal::Unverified(_))
        ));
    }

    #[test]
    fn only_a_single_excision_passes_verification() {
        assert_eq!(cut_out("abcdef", "abef"), Some("cd"));
        assert_eq!(cut_out("aaab", "aab"), Some("a"));
        assert_eq!(cut_out("abcdef", "abXf"), None, "a rewrite is not a cut");
        assert_eq!(cut_out("abcdef", "bdf"), None, "two cuts are not one");
        assert_eq!(cut_out("abc", "abc"), None, "nothing removed");
    }

    #[test]
    fn a_uuid_is_accepted_only_in_the_form_networkmanager_writes() {
        assert_eq!(parse_uuid(UUID), Some(UUID));
        for bad in [
            "",
            "11111111-2222-3333-4444-55555555555",
            "11111111-2222-3333-4444-5555555555555",
            "11111111_2222-3333-4444-555555555555",
            "ABCDEF11-2222-3333-4444-555555555555",
            "11111111-2222-3333-4444-55555555555/",
            "../../../../etc/shadow-4444-55555555",
        ] {
            assert_eq!(parse_uuid(bad), None, "{bad:?}");
        }
    }
}
