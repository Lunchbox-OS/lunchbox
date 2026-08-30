//! Where `yt-dlp` is allowed to run (issue #144).
//!
//! `shepherdd` accepts a client on its management socket only from its own
//! cgroup. Every subprocess it starts is a direct child, so it inherits that
//! cgroup and lands inside the allow-list. For most of shepherd's helpers that
//! is uninteresting — fixed argv, output read straight back. `yt-dlp` is not:
//! it runs on a background prefetch timer with no activity launched, and it
//! parses whatever a remote host returns, from an extractor with a recurring
//! history of parser bugs. The URLs come from admin-configured libraries, so an
//! activity cannot choose the target; the untrusted part is the response.
//!
//! So `yt-dlp` gets a cgroup of its own, like an activity. This crate does not
//! build that wrapper itself: it has no business knowing about systemd, is
//! shared with the player (and with the Android build, which has no user
//! manager at all), and the probe for whether scoping works at all lives in
//! `shepherd-host-linux`. Instead `shepherdd` **injects** a wrapper at startup
//! via [`set_scope_prefix_fn`]; anything that has not called it — a test, the
//! player, Android — runs `yt-dlp` bare, exactly as before.

use std::process::Command;
use std::sync::OnceLock;

/// Builds the argv prefix that runs a helper named `tag` in a cgroup of its
/// own, or an empty prefix meaning "run it bare".
///
/// A plain `fn` rather than a closure: the only implementation is
/// `shepherd_host_linux::helper_scope_argv_prefix`, and keeping it
/// non-capturing means no allocation and no lifetime to reason about.
pub type ScopePrefixFn = fn(&str) -> Vec<String>;

static SCOPE_PREFIX: OnceLock<ScopePrefixFn> = OnceLock::new();

/// Install the wrapper every `yt-dlp` invocation is launched through.
///
/// Called once by `shepherdd` at startup. Later calls are ignored rather than
/// panicking: losing the race would mean two callers disagreeing about
/// isolation, and the first one to win is the daemon's own startup.
pub fn set_scope_prefix_fn(f: ScopePrefixFn) {
    let _ = SCOPE_PREFIX.set(f);
}

/// A `yt-dlp` command, inside a cgroup of its own where one can be had.
///
/// `tag` names the call site so a stray scope is identifiable in
/// `systemctl --user list-units`.
pub fn ytdlp_command(tag: &str) -> Command {
    ytdlp_command_with(SCOPE_PREFIX.get().map(|f| f(tag)).unwrap_or_default())
}

/// [`ytdlp_command`] with the prefix supplied rather than looked up.
///
/// Split out so the wrapping is testable: the lookup goes through a `OnceLock`
/// that, once set, stays set for the whole test binary — so a test that
/// installed one would decide the outcome of every other test by running first.
fn ytdlp_command_with(prefix: Vec<String>) -> Command {
    match prefix.split_first() {
        Some((program, rest)) => {
            let mut cmd = Command::new(program);
            // `yt-dlp` goes after the prefix, which ends in `--`; before it, the
            // wrapper would read it as one of its own flags.
            cmd.args(rest).arg("yt-dlp");
            cmd
        }
        None => Command::new("yt-dlp"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(cmd: &Command) -> Vec<String> {
        std::iter::once(cmd.get_program())
            .chain(cmd.get_args())
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn without_a_wrapper_ytdlp_runs_bare() {
        // The player, the Android build, and anything that never calls
        // `set_scope_prefix_fn` take this path, unchanged from before #144.
        assert_eq!(argv(&ytdlp_command_with(Vec::new())), vec!["yt-dlp"]);
    }

    #[test]
    fn a_wrapper_runs_ytdlp_inside_it_after_the_separator() {
        // Order is the part worth pinning: `yt-dlp` before the `--` would be
        // parsed as a flag to systemd-run, which fails in a way that reads like
        // yt-dlp being missing.
        let prefix = vec![
            "systemd-run".to_string(),
            "--user".to_string(),
            "--scope".to_string(),
            "--unit=shepherd-ytdlp-download-7-0.scope".to_string(),
            "--".to_string(),
        ];
        assert_eq!(
            argv(&ytdlp_command_with(prefix)),
            vec![
                "systemd-run",
                "--user",
                "--scope",
                "--unit=shepherd-ytdlp-download-7-0.scope",
                "--",
                "yt-dlp",
            ]
        );
    }
}
