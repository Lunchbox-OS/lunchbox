//! Who is on the other end of a connection to lunchboxd's socket (issue #144).
//!
//! Every activity lunchboxd launches runs as lunchboxd's own uid, so the uid
//! the kernel reports for a peer says nothing: a game, the launcher and the HUD
//! are all indistinguishable by it. The identity that *does* separate them is
//! the peer's **cgroup**. The kernel maintains it, every descendant inherits it
//! — including double-forked and reparented ones, where a PPID walk falls apart
//! — and an unprivileged process can neither forge it nor climb out of it.
//!
//! The mechanisms that suggest themselves first do not work here, and the
//! reasoning is recorded in `docs/ai/history/2026-08-29 001` and `002`:
//!
//! * **Not the socket's name.** Unlike sway's socket (#147), this one has
//!   short-lived clients that connect by path for the life of the session, by
//!   design, so there is no moment at which the name becomes removable.
//! * **Not a shared secret.** Under one uid, any token a trusted client can
//!   read — from the environment, from argv, from a file — an activity can read
//!   too. Peer credentials need no secret, which is why they survive.
//! * **Not the peer's binary.** `/usr/local/bin/lunchbox-launcher` is
//!   executable by that uid, so an activity can `exec` it and be byte-identical
//!   by `comm`, `/proc/pid/exe` and `cmdline`. Identity has to come from
//!   provenance, not from what is running.
//!
//! ## How the cgroup is read
//!
//! `SO_PEERPIDFD` hands back a pidfd for the process that called `connect()`,
//! captured by the kernel at that moment and not forgeable by the peer.
//! `PIDFD_GET_INFO` then reads that process's `cgroupid` straight off the
//! pidfd. Nothing goes through `/proc`, and nothing is ever looked up by pid —
//! so the pid-reuse race that would otherwise have to be reasoned about (the
//! peer exits, its pid is recycled, and the lookup inspects the wrong process)
//! does not arise at all.
//!
//! `cgroupid` is the cgroup directory's inode number, so comparing against our
//! own is a single `u64`. Both sides of that comparison come from the same
//! ioctl, so neither depends on where cgroup2 is mounted.
//!
//! ## It fails closed on a peer that has already exited
//!
//! `PIDFD_GET_INFO` returns `ESRCH` once the peer is gone, even though the
//! connection outlives it. That is the correct direction — an unclassifiable
//! peer is refused — and it is the concrete reason not to fall back to reading
//! `/proc/<pid>/cgroup`, which is exactly the race the pidfd removes. It does
//! mean a client that connects, writes and exits without waiting for its reply
//! would be rejected non-deterministically. Every one-shot in this repo blocks
//! on its response, so that is an invariant to keep rather than a limitation to
//! work around.

use lunchbox_api::ClientRole;
use nix::libc;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};

/// `SO_PEERPIDFD`, Linux 6.5+. Not in `libc` at the pinned version.
const SO_PEERPIDFD: libc::c_int = 77;

/// `PIDFS_IOCTL_MAGIC` from `linux/pidfd.h`. Worth spelling out because the
/// obvious guess is `'p'`, and getting it wrong yields `ENOTTY` — which reads
/// exactly like "this kernel does not support it".
const PIDFS_IOCTL_MAGIC: u32 = 0xFF;

/// `PIDFD_INFO_SIZE_VER2` — the third published `struct pidfd_info`. The size
/// is part of the ioctl request number, so it selects the struct version.
const PIDFD_INFO_SIZE_VER2: u32 = 80;

/// Bit in `pidfd_info.mask` saying `cgroupid` was actually filled in. The
/// kernel documents it as "always returned if available"; we still check,
/// because "available" is the part that is not guaranteed.
const PIDFD_INFO_CGROUPID: u64 = 1 << 2;

/// `_IOWR(magic, nr, size)` from `asm-generic/ioctl.h`.
const fn iowr(magic: u32, nr: u32, size: u32) -> libc::c_ulong {
    ((3u32 << 30) | (size << 16) | (magic << 8) | nr) as libc::c_ulong
}

/// `struct pidfd_info` at `PIDFD_INFO_SIZE_VER2`. Field order and types are
/// from `linux/pidfd.h`; the trailing `supported_mask` is what takes the struct
/// from 72 to 80 bytes.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct PidfdInfo {
    mask: u64,
    cgroupid: u64,
    pid: u32,
    tgid: u32,
    ppid: u32,
    ruid: u32,
    rgid: u32,
    euid: u32,
    egid: u32,
    suid: u32,
    sgid: u32,
    fsuid: u32,
    fsgid: u32,
    exit_code: i32,
    coredump_mask: u32,
    coredump_signal: u32,
    supported_mask: u64,
}

/// Why a peer could not be identified.
///
/// Every variant is a refusal — the caller must not treat any of them as "give
/// it the benefit of the doubt" — but they are kept apart because they mean
/// very different things to whoever reads the log: one is a kernel that cannot
/// support the check at all, the others are ordinary races and errors.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// The kernel does not offer `SO_PEERPIDFD` or `PIDFD_GET_INFO`. Below the
    /// 26.04 floor this cannot happen; it is reported rather than assumed away
    /// so a stripped-down kernel says so instead of silently accepting.
    #[error("this kernel cannot report a peer's cgroup ({0})")]
    Unsupported(io::Error),
    /// The peer exited between `connect()` and this check.
    #[error("the peer exited before it could be identified")]
    PeerGone,
    /// The kernel answered, but without a cgroup id — a kernel built without
    /// cgroup2, or a peer the id is not available for.
    #[error("the kernel reported no cgroup for this peer")]
    NoCgroup,
    #[error("could not identify the peer: {0}")]
    Io(#[from] io::Error),
}

fn pidfd_info(fd: BorrowedFd<'_>) -> Result<PidfdInfo, PeerError> {
    let mut info = PidfdInfo {
        // Ask for the cgroup id; PID and CREDS come back regardless.
        mask: PIDFD_INFO_CGROUPID,
        ..Default::default()
    };
    let request = iowr(PIDFS_IOCTL_MAGIC, 11, PIDFD_INFO_SIZE_VER2);

    // SAFETY: `fd` is a live pidfd for the duration of the call, and `info` is
    // a `PIDFD_INFO_SIZE_VER2`-sized `pidfd_info` — the size the request number
    // encodes, so the kernel writes exactly this many bytes.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), request, &raw mut info) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            Some(libc::ESRCH) => PeerError::PeerGone,
            Some(libc::ENOTTY) | Some(libc::EINVAL) => PeerError::Unsupported(err),
            _ => PeerError::Io(err),
        });
    }
    Ok(info)
}

/// The pidfd for whoever called `connect()` on this socket.
///
/// Captured by the kernel at connect time like `SO_PEERCRED`, so the peer
/// cannot set it, change it afterwards, or lie about it — and unlike the pid in
/// `SO_PEERCRED`, a pidfd is never recycled.
fn peer_pidfd(fd: BorrowedFd<'_>) -> Result<OwnedFd, PeerError> {
    let mut peer: libc::c_int = -1;
    let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    // SAFETY: `peer`/`len` are a correctly sized out-param pair for an int
    // socket option on a live fd.
    let rc = unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            SO_PEERPIDFD,
            (&raw mut peer).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            // The peer is already gone, so the kernel has no process to make a
            // pidfd for. Same meaning as ESRCH below.
            Some(libc::ESRCH) | Some(libc::EINVAL) => PeerError::PeerGone,
            Some(libc::ENOPROTOOPT) => PeerError::Unsupported(err),
            _ => PeerError::Io(err),
        });
    }
    // SAFETY: the kernel just handed us an owned fd.
    Ok(unsafe { OwnedFd::from_raw_fd(peer) })
}

/// The cgroup id of the process on the other end of `fd`.
pub fn peer_cgroup_id(fd: BorrowedFd<'_>) -> Result<u64, PeerError> {
    let pidfd = peer_pidfd(fd)?;
    let info = pidfd_info(pidfd.as_fd())?;
    if info.mask & PIDFD_INFO_CGROUPID == 0 {
        return Err(PeerError::NoCgroup);
    }
    Ok(info.cgroupid)
}

/// The pid of the process on the other end of `fd`, for logging only.
///
/// Deliberately separate from [`peer_cgroup_id`] and never part of a decision:
/// a pid is only meaningful while its process lives, and the whole point of
/// going through the pidfd is that the decision never depends on that.
pub fn peer_pid(fd: BorrowedFd<'_>) -> Option<u32> {
    let pidfd = peer_pidfd(fd).ok()?;
    pidfd_info(pidfd.as_fd()).ok().map(|i| i.pid)
}
/// The uid the kernel recorded for whoever called `connect()`.
///
/// `SO_PEERCRED`, captured at connect time, so the peer cannot set or change
/// it. On its own it separates nothing here — every activity shares lunchboxd's
/// uid, which is the whole of #144 — but root is unforgeable in it, and that is
/// what keeps `sudo` able to reach a daemon from outside the trusted cgroup.
pub fn peer_uid(fd: BorrowedFd<'_>) -> Option<u32> {
    nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::PeerCredentials)
        .ok()
        .map(|cred| cred.uid())
}

/// Our own cgroup id, read the same way we read a peer's.
///
/// Via a pidfd rather than by `stat`ing a path under `/sys/fs/cgroup`, so both
/// sides of the comparison come from one kernel interface and neither depends
/// on where cgroup2 happens to be mounted.
pub fn own_cgroup_id() -> Result<u64, PeerError> {
    // SAFETY: plain syscall with no pointer arguments.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
    if raw < 0 {
        return Err(PeerError::Io(io::Error::last_os_error()));
    }
    // SAFETY: the kernel just handed us an owned fd.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) };
    let info = pidfd_info(pidfd.as_fd())?;
    if info.mask & PIDFD_INFO_CGROUPID == 0 {
        return Err(PeerError::NoCgroup);
    }
    Ok(info.cgroupid)
}

/// Our own cgroup as a path, e.g. `/user.slice/…/session-2.scope`.
///
/// Only ever used for messages a human reads and for the "am I somewhere this
/// check can mean anything" test in [`PeerPolicy::new`] — never for a decision
/// about a peer, which goes through the ids above.
pub fn own_cgroup_path() -> io::Result<String> {
    let raw = std::fs::read_to_string("/proc/self/cgroup")?;
    // cgroup2 puts everything on one `0::<path>` line.
    raw.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
        .ok_or_else(|| io::Error::other("no cgroup2 line in /proc/self/cgroup"))
}

/// Whether `path` sits inside the user manager's delegated subtree.
///
/// It matters because delegation is what makes cgroup identity forgeable:
/// inside `user@<uid>.service` every cgroup is owned by that uid, so any
/// process at that uid can move itself into any other cgroup there — verified,
/// not assumed. Outside it (a logind session scope, a system-manager scope) the
/// cgroups are root-owned and it cannot.
///
/// A daemon started from a display-manager session is outside it. One started
/// from a shell, or as a `systemd --user` unit, is inside it — which is why the
/// dev harness cannot exercise this check and a device can.
pub fn is_delegated_user_cgroup(path: &str) -> bool {
    path.split('/').any(|seg| {
        seg.starts_with("user@")
            && seg.ends_with(".service")
            && seg["user@".len()..seg.len() - ".service".len()]
                .chars()
                .all(|c| c.is_ascii_digit())
    })
}

/// Whether this kernel can report a peer's cgroup at all.
///
/// `SO_PEERPIDFD` needs Linux 6.5 and `PIDFD_GET_INFO` is newer still; below
/// that the ioctl answers `ENOTTY` and every check here is a refusal. A device
/// is above the floor by definition — 26.04 is the minimum and it ships a
/// kernel far newer — but **CI is not**: the Rust jobs run in a container on a
/// hosted runner, so they get the runner's kernel however new the image is.
///
/// Tests that need the real mechanism use this to skip loudly rather than fail
/// on a kernel the test environment does not choose. That is a genuine coverage
/// gap, not a fix: the peer check is exercised for real only on a host new
/// enough to run it, so a regression in the cgroup comparison would show up on
/// a developer's machine and in the e2e suite, not in CI.
pub fn kernel_supports_peer_cgroup() -> bool {
    // `NoCgroup` counts as unsupported, not as a failure. It is what a kernel
    // new enough for `PIDFD_GET_INFO` but not for `PIDFD_INFO_CGROUPID` answers
    // — an intermediate a distribution upgrade can genuinely land on — and
    // "this kernel cannot report a peer's cgroup" is exactly what this function
    // is asked. Treating it as support would run the tests and fail them.
    !matches!(
        own_cgroup_id(),
        Err(PeerError::Unsupported(_) | PeerError::NoCgroup)
    )
}

/// Environment variable that turns a skip into a failure.
///
/// Skips are invisible: `cargo test` captures a passing test's output, so
/// `[SKIP]` never reaches the log and a green run says nothing about whether
/// the peer check was actually exercised. Set this on a runner whose kernel is
/// known to be above the floor and the suite starts insisting on it, so the
/// coverage cannot quietly disappear again.
pub const REQUIRE_PEER_CGROUP_ENV: &str = "LUNCHBOX_REQUIRE_PEER_CGROUP";

/// Whether `test` should bail out because this kernel cannot support it.
///
/// Panics instead when [`REQUIRE_PEER_CGROUP_ENV`] is set, which is how a
/// runner that has been upgraded stops accepting a silent skip.
pub fn skip_without_peer_cgroup(test: &str) -> bool {
    if kernel_supports_peer_cgroup() {
        return false;
    }
    assert!(
        std::env::var_os(REQUIRE_PEER_CGROUP_ENV).is_none(),
        "{test}: {REQUIRE_PEER_CGROUP_ENV} is set, but this kernel cannot report a peer's \
         cgroup — the host is below the floor #144 needs, or the container cannot see cgroup2"
    );
    eprintln!(
        "[SKIP] {test}: this kernel cannot report a peer's cgroup; set \
         {REQUIRE_PEER_CGROUP_ENV}=1 to make this a failure"
    );
    true
}

/// What a *client* found on the other end of its connection (issue #144).
///
/// The mirror of [`PeerPolicy`], and needed for the same reason read backwards.
/// The socket lives in a directory owned by the uid every activity runs as, so
/// an activity can `unlink()` it and `bind()` its own listener at the same
/// path: existing connections survive, but every new one — a relaunched
/// launcher or HUD, a keybinding one-shot, `swayidle`'s screen blank — arrives
/// at the impostor instead. No file mode prevents this. A root-owned directory
/// stops `lunchboxd` binding at all, and the sticky bit restricts deletion to
/// the file's *owner*, which an activity is: it shares the daemon's uid. Both
/// were measured, not assumed.
///
/// So the client checks who answered, the same way the daemon checks who
/// called. Lunchbox's own clients — the launcher, the HUD, the one-shots sway
/// starts — live in the daemon's cgroup, so "the server is in my cgroup" is
/// exactly the question, and an activity's listener cannot pass it: it is in a
/// scope of its own by construction.
#[derive(Debug)]
pub enum ServerCheck {
    /// The daemon shares our cgroup — it is this session's own.
    Ours,
    /// Something in another cgroup answered. Either an impostor, or a
    /// legitimate client talking from outside the session.
    Foreign { server: u64, ours: u64 },
    /// The server could not be identified at all.
    Unknown(PeerError),
    /// *We* could not be identified, so there is nothing to compare against.
    /// Distinct from [`Self::Unknown`] because the two fail in opposite
    /// directions — see [`classify_server`].
    SelfUnknown(PeerError),
}

/// Identify the process on the other end of a client connection.
///
/// The two failure cases are deliberately separate, because an activity can
/// reach one and not the other:
///
/// * [`ServerCheck::Unknown`] means the *server's* identity could not be read.
///   An impostor can induce that — accept the connection, then exit, and
///   `PIDFD_GET_INFO` answers `ESRCH` — so it has to be treated as a refusal.
///   Trusting it would hand back everything the check was for.
/// * [`ServerCheck::SelfUnknown`] means we could not read our *own* cgroup.
///   Nothing an activity does causes that; it is a kernel that cannot answer,
///   and refusing would leave a device with a launcher that will not start. The
///   caller warns and continues, which is the same trade the daemon makes when
///   it cannot read its own cgroup at startup.
pub fn classify_server(fd: BorrowedFd<'_>) -> ServerCheck {
    let ours = match own_cgroup_id() {
        Ok(id) => id,
        Err(e) => return ServerCheck::SelfUnknown(e),
    };
    match peer_cgroup_id(fd) {
        Ok(server) if server == ours => ServerCheck::Ours,
        Ok(server) => ServerCheck::Foreign { server, ours },
        Err(e) => ServerCheck::Unknown(e),
    }
}

/// Why a peer was refused, in the words an administrator should see.
#[derive(Debug, Clone)]
pub struct Rejection {
    /// What went wrong, phrased for a human.
    pub reason: String,
    /// The peer's cgroup path, when it could still be read. Best-effort and
    /// **only** for the message: it comes from `/proc`, which is exactly the
    /// racy lookup the decision itself avoids. Since an activity's scope is
    /// named after its session id, this usually names the offending activity.
    pub peer_cgroup: Option<String>,
    /// The peer's pid, likewise for the message only.
    pub peer_pid: Option<u32>,
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.reason)?;
        if let Some(cg) = &self.peer_cgroup {
            write!(f, " (peer cgroup {cg})")?;
        }
        Ok(())
    }
}

/// Which peers this server will talk to.
///
/// The test is an **allow-list**: accept only peers positively recognised as
/// legitimate, refuse everything else including anything that cannot be
/// classified. A deny-list — "refuse peers I recognise as activities" — fails
/// open on precisely the cases it cannot classify, and there is a verified
/// escape that lands in exactly that gap: an activity can ask `systemd --user`
/// to start a process for it in a cgroup that is in no Lunchbox scope at all.
/// That process is refused here because it is not *in lunchboxd's own cgroup*,
/// which is a different question from whether it is in a scope we made.
#[derive(Debug, Clone)]
pub struct PeerPolicy {
    /// `Some` when the allow-list is armed, carrying the cgroup id every
    /// accepted peer must match. `None` disarms it (the dev opt-out), and the
    /// uid-only classification from before this check applies.
    ///
    /// Usually this is our *own* cgroup ([`PeerPolicy::restricted`]) — but not
    /// necessarily. A server that is deliberately outside the session it serves
    /// names the trusted cgroup instead ([`PeerPolicy::for_cgroup`]), so the
    /// field is what it is trusted to be rather than where we happen to live.
    trusted_cgroup: Option<u64>,
    own_uid: u32,
}

impl PeerPolicy {
    /// Accept anything at this uid, as lunchboxd did before #144.
    ///
    /// For development, where the whole stack runs inside one shell's cgroup
    /// and any client started from another terminal would otherwise be refused.
    pub fn unrestricted() -> Self {
        Self {
            trusted_cgroup: None,
            own_uid: nix::unistd::getuid().as_raw(),
        }
    }

    /// Accept only root, and peers in lunchboxd's own cgroup.
    ///
    /// Fails if our own cgroup cannot be read, because a policy that cannot
    /// name what it accepts would refuse every client — better to say so at
    /// startup than to bring up a session nothing can talk to.
    pub fn restricted() -> Result<Self, PeerError> {
        Ok(Self::for_cgroup(own_cgroup_id()?))
    }

    /// Accept only root, and peers in the cgroup `trusted`.
    ///
    /// For a server that is **not in the session it serves** and so cannot
    /// compare against itself: `lunchbox-stated` runs under the system manager
    /// as its own uid, and the cgroup it trusts is the kiosk's logind session
    /// scope, resolved from logind rather than from `own_cgroup_id`.
    ///
    /// The comparison is the same one [`Self::restricted`] makes and rests on
    /// the same measured facts (issue #157): a `session-<n>.scope` is
    /// root-owned, an activity cannot write itself into it, and it cannot ask
    /// logind or the user manager to put a process there either. What differs
    /// is only where the number came from.
    ///
    /// Callers are responsible for resolving a cgroup that means something. A
    /// trusted id that names a cgroup nothing runs in refuses every peer, which
    /// is the safe direction but still wants saying out loud at startup.
    pub fn for_cgroup(trusted: u64) -> Self {
        Self {
            trusted_cgroup: Some(trusted),
            own_uid: nix::unistd::getuid().as_raw(),
        }
    }

    /// Whether the allow-list is armed.
    pub fn is_restricted(&self) -> bool {
        self.trusted_cgroup.is_some()
    }

    /// The cgroup id every non-root peer must be in, when armed.
    ///
    /// For startup reporting: a daemon that says which cgroup it is trusting
    /// turns "everything is refused" from a mystery into one line of log.
    pub fn trusted_cgroup(&self) -> Option<u64> {
        self.trusted_cgroup
    }

    /// Decide what a connected peer may do, or why it may do nothing.
    ///
    /// Called once per connection at accept, not per call: one decision instead
    /// of many, it cannot be forgotten when a method is added, and a peer that
    /// should not be reading state at all never gets the event stream.
    pub fn classify(
        &self,
        fd: BorrowedFd<'_>,
        peer_uid: Option<u32>,
    ) -> Result<ClientRole, Rejection> {
        // Root is unforgeable in `SO_PEERCRED` and is already able to do
        // everything lunchboxd could do for it. A local operator's `sudo`
        // reaches the daemon from any cgroup, which is what makes the check
        // survivable for administration.
        if peer_uid == Some(0) {
            return Ok(ClientRole::Admin);
        }

        let Some(trusted) = self.trusted_cgroup else {
            // Disarmed: exactly the classification lunchboxd shipped before.
            return Ok(match peer_uid {
                Some(u) if u == self.own_uid => ClientRole::Admin,
                _ => ClientRole::Shell,
            });
        };

        match peer_cgroup_id(fd) {
            Ok(peer) if peer == trusted => Ok(ClientRole::Admin),
            Ok(peer) => Err(self.reject(
                fd,
                format!(
                    "a process at this uid connected from cgroup id {peer}, which is not \
                     the trusted one ({trusted}) — activities live in their own \
                     cgroups and nothing there may drive the daemon"
                ),
            )),
            Err(e) => Err(self.reject(fd, format!("{e}"))),
        }
    }

    /// Dress a refusal up with the best-effort detail a human needs. Nothing
    /// gathered here influences the decision — it has already been made.
    fn reject(&self, fd: BorrowedFd<'_>, reason: String) -> Rejection {
        let peer_pid = peer_pid(fd);
        let peer_cgroup = peer_pid
            .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok())
            .and_then(|raw| {
                raw.lines()
                    .find_map(|l| l.strip_prefix("0::"))
                    .map(|p| p.trim().to_string())
            });
        Rejection {
            reason,
            peer_cgroup,
            peer_pid,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The uid these tests claim the peer had, deliberately not root.
    ///
    /// `classify` accepts root from any cgroup by design, and it checks that
    /// *before* it looks at the cgroup — so a test that passes `getuid()` skips
    /// the thing it means to exercise the moment it runs as root. The CI image
    /// has no `USER` directive, so that is exactly what happens there: one test
    /// failed outright and two others passed without ever comparing a cgroup.
    ///
    /// This is the value the accept loop would have read from `SO_PEERCRED`.
    /// Nothing here depends on it matching the process's real uid; what matters
    /// is only that it is not 0.
    const PEER_UID: u32 = 1000;

    #[test]
    fn the_ioctl_request_number_matches_the_kernel_header() {
        // _IOWR(0xFF, 11, struct pidfd_info) with the 80-byte struct. Pinned
        // because an off-by-one here is not a compile error and not a runtime
        // panic — it is an ENOTTY that reads as "unsupported kernel", which
        // would silently turn the check into a permanent refusal.
        assert_eq!(
            iowr(PIDFS_IOCTL_MAGIC, 11, PIDFD_INFO_SIZE_VER2),
            0xC050FF0B,
            "PIDFD_GET_INFO request number changed"
        );
    }

    #[test]
    fn the_info_struct_is_the_size_the_request_claims() {
        // The size is baked into the request number, so a layout change that
        // altered it would make the kernel reject the call.
        assert_eq!(
            std::mem::size_of::<PidfdInfo>(),
            PIDFD_INFO_SIZE_VER2 as usize
        );
    }

    #[test]
    fn our_own_cgroup_reads_back() {
        if skip_without_peer_cgroup("our_own_cgroup_reads_back") {
            return;
        }
        // Both readers have to work on whatever runs the tests; this is the
        // one assertion that would catch a kernel too old for the design.
        let id = own_cgroup_id().expect("own cgroup id");
        assert_ne!(id, 0);
        let path = own_cgroup_path().expect("own cgroup path");
        assert!(path.starts_with('/'), "unexpected cgroup path {path:?}");
    }

    #[test]
    fn an_armed_policy_accepts_a_peer_in_our_own_cgroup() {
        if skip_without_peer_cgroup("an_armed_policy_accepts_a_peer_in_our_own_cgroup") {
            return;
        }
        // A socketpair's peer is this very process, so it is in our cgroup by
        // construction — the launcher/HUD/one-shot case, all of which are
        // descendants of the session lunchboxd is in.
        let policy = PeerPolicy::restricted().expect("read own cgroup");
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let role = policy
            .classify(a.as_fd(), Some(PEER_UID))
            .expect("a peer in our own cgroup is accepted");
        assert_eq!(role, ClientRole::Admin);
    }

    #[test]
    fn an_armed_policy_refuses_a_peer_from_another_cgroup() {
        if skip_without_peer_cgroup("an_armed_policy_refuses_a_peer_from_another_cgroup") {
            return;
        }
        // Stand in for an activity by claiming a cgroup id that cannot be ours.
        // The real separation is exercised end-to-end in the e2e suite, which
        // can put a peer in a scope of its own; here the point is only that a
        // mismatch is refused rather than logged and waved through.
        let policy = PeerPolicy {
            trusted_cgroup: Some(own_cgroup_id().expect("own cgroup id").wrapping_add(1)),
            own_uid: PEER_UID,
        };
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let err = policy
            .classify(a.as_fd(), Some(PEER_UID))
            .expect_err("a peer outside Lunchbox's cgroup must be refused");
        assert!(
            err.reason.contains("not the trusted one"),
            "unhelpful refusal: {}",
            err.reason
        );
    }

    #[test]
    fn for_cgroup_trusts_a_cgroup_that_is_not_our_own() {
        if skip_without_peer_cgroup("for_cgroup_trusts_a_cgroup_that_is_not_our_own") {
            return;
        }
        // `lunchbox-stated` is deliberately outside the session it serves, so
        // the cgroup it trusts is never its own. A socketpair peer *is* this
        // process, which is what makes both directions checkable here: name our
        // cgroup and the peer is accepted, name any other and it is refused.
        let ours = own_cgroup_id().expect("own cgroup id");
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");

        let trusting_us = PeerPolicy::for_cgroup(ours);
        assert!(trusting_us.is_restricted());
        assert_eq!(trusting_us.trusted_cgroup(), Some(ours));
        assert_eq!(
            trusting_us
                .classify(a.as_fd(), Some(PEER_UID))
                .expect("a peer in the trusted cgroup is accepted"),
            ClientRole::Admin
        );

        let trusting_elsewhere = PeerPolicy::for_cgroup(ours.wrapping_add(1));
        let err = trusting_elsewhere
            .classify(a.as_fd(), Some(PEER_UID))
            .expect_err("a peer outside the trusted cgroup must be refused");
        assert!(
            err.reason.contains("not the trusted one"),
            "unhelpful refusal: {}",
            err.reason
        );
    }

    #[test]
    fn root_is_accepted_from_any_cgroup() {
        if skip_without_peer_cgroup("root_is_accepted_from_any_cgroup") {
            return;
        }
        // `sudo lunchbox …` comes from the operator's own login session, which
        // is never lunchboxd's cgroup. Without this the check would lock an
        // administrator out of their own device.
        let policy = PeerPolicy {
            trusted_cgroup: Some(own_cgroup_id().expect("own cgroup id").wrapping_add(1)),
            own_uid: nix::unistd::getuid().as_raw(),
        };
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        assert_eq!(
            policy.classify(a.as_fd(), Some(0)).expect("root accepted"),
            ClientRole::Admin
        );
    }

    #[test]
    fn a_disarmed_policy_classifies_as_before() {
        // The dev opt-out has to be exactly the old behaviour, or a developer's
        // session starts failing in ways a device never would.
        assert!(!PeerPolicy::unrestricted().is_restricted());

        // Built by hand rather than via `unrestricted()`, which takes `own_uid`
        // from `getuid()`: as root that is 0, and the root rule would answer
        // before the uid comparison this is here to check.
        let policy = PeerPolicy {
            trusted_cgroup: None,
            own_uid: PEER_UID,
        };
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        assert_eq!(
            policy
                .classify(a.as_fd(), Some(PEER_UID))
                .expect("accepted"),
            ClientRole::Admin
        );
        assert_eq!(
            policy
                .classify(a.as_fd(), Some(PEER_UID + 1))
                .expect("accepted"),
            ClientRole::Shell
        );
    }

    #[test]
    fn delegated_subtrees_are_recognised() {
        assert!(is_delegated_user_cgroup(
            "/user.slice/user-1000.slice/user@1000.service/app.slice/foo.scope"
        ));
        assert!(is_delegated_user_cgroup("/user.slice/user@0.service"));
        // A logind session scope: same uid, root-owned, not delegated. This is
        // where a device's session actually lives.
        assert!(!is_delegated_user_cgroup(
            "/user.slice/user-1000.slice/session-2.scope"
        ));
        // A transient scope in the system manager, where activities go.
        assert!(!is_delegated_user_cgroup(
            "/system.slice/lunchbox-abc.scope"
        ));
        // Not the user manager, just something that looks like it.
        assert!(!is_delegated_user_cgroup(
            "/user.slice/user@notauid.service"
        ));
    }
}
