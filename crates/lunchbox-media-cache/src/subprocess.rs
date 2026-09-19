//! Where `yt-dlp` is allowed to run (issue #144).
//!
//! `lunchboxd` accepts a client on its management socket only from its own
//! cgroup. Every subprocess it starts is a direct child, so it inherits that
//! cgroup and lands inside the allow-list. For most of Lunchbox's helpers that
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
//! `lunchbox-host-linux`. Instead `lunchboxd` **injects** a wrapper at startup
//! via [`set_scope_prefix_fn`]; anything that has not called it — a test, the
//! player, Android — runs `yt-dlp` bare, exactly as before.

// This crate cannot call `helpers::command`: it must not depend on
// lunchbox-host-linux, which is the whole reason the resolver is injected here
// instead (issue #144). Every `Command::new` below takes a value that has
// already been through that resolver, or through the injected scope prefix.
#![allow(clippy::disallowed_methods)]

use std::process::Command;
use std::sync::OnceLock;

/// Builds the argv prefix that runs a helper named `tag` in a cgroup of its
/// own, or an empty prefix meaning "run it bare".
///
/// A plain `fn` rather than a closure: the only implementation is
/// `lunchbox_host_linux::helper_scope_argv_prefix`, and keeping it
/// non-capturing means no allocation and no lifetime to reason about.
pub type ScopePrefixFn = fn(&str) -> Vec<String>;

static SCOPE_PREFIX: OnceLock<ScopePrefixFn> = OnceLock::new();

/// Resolves a bare program name to an absolute path in a directory only root
/// can write.
///
/// Injected for the same reason as [`ScopePrefixFn`]: the scoping above
/// contains a hijacked `yt-dlp`, but the `--version` liveness probe runs
/// unscoped, so where the binary is *found* has to be safe on its own. On a
/// device `$PATH` is not — GDM's PAM stack reads `~/.pam_environment`, which the
/// kiosk user owns (issue #144).
pub type ProgramResolverFn = fn(&str) -> String;

static PROGRAM_RESOLVER: OnceLock<ProgramResolverFn> = OnceLock::new();

/// Install the resolver every `yt-dlp` lookup goes through.
pub fn set_program_resolver_fn(f: ProgramResolverFn) {
    let _ = PROGRAM_RESOLVER.set(f);
}

/// `yt-dlp`'s path: resolved if a resolver was injected, bare otherwise.
///
/// Bare is right for the player and the Android build, which have no daemon
/// cgroup to protect and no injected resolver.
pub fn ytdlp_program() -> String {
    PROGRAM_RESOLVER
        .get()
        .map(|f| f("yt-dlp"))
        .unwrap_or_else(|| "yt-dlp".to_string())
}

/// Install the wrapper every `yt-dlp` invocation is launched through.
///
/// Called once by `lunchboxd` at startup. Later calls are ignored rather than
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

/// A bare `yt-dlp` command — resolved, but in no scope of its own.
///
/// Only for the `--version` liveness probe, which parses no remote input. See
/// `playlist::ensure_ytdlp_available`.
pub fn ytdlp_probe_command() -> Command {
    Command::new(ytdlp_program())
}

/// [`ytdlp_command`] with the prefix supplied rather than looked up.
///
/// Split out so the wrapping is testable: the lookup goes through a `OnceLock`
/// that, once set, stays set for the whole test binary — so a test that
/// installed one would decide the outcome of every other test by running first.
fn ytdlp_command_with(prefix: Vec<String>) -> Command {
    let ytdlp = ytdlp_program();
    match prefix.split_first() {
        Some((program, rest)) => {
            let mut cmd = Command::new(program);
            // `yt-dlp` goes after the prefix, which ends in `--`; before it, the
            // wrapper would read it as one of its own flags.
            cmd.args(rest).arg(ytdlp);
            cmd
        }
        None => Command::new(ytdlp),
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
            "--unit=lunchbox-ytdlp-download-7-0.scope".to_string(),
            "--".to_string(),
        ];
        assert_eq!(
            argv(&ytdlp_command_with(prefix)),
            vec![
                "systemd-run",
                "--user",
                "--scope",
                "--unit=lunchbox-ytdlp-download-7-0.scope",
                "--",
                "yt-dlp",
            ]
        );
    }
}
