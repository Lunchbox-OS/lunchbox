//! Who is on the other end of a connection to shepherdd's socket (issue #144).
//!
//! Every activity shepherdd launches runs as shepherdd's own uid, so the uid
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
//! * **Not the peer's binary.** `/usr/local/bin/shepherd-launcher` is
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

use nix::libc;
use shepherd_api::ClientRole;
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

/// What a *client* found on the other end of its connection (issue #144).
///
/// The mirror of [`PeerPolicy`], and needed for the same reason read backwards.
/// The socket lives in a directory owned by the uid every activity runs as, so
/// an activity can `unlink()` it and `bind()` its own listener at the same
/// path: existing connections survive, but every new one — a relaunched
/// launcher or HUD, a keybinding one-shot, `swayidle`'s screen blank — arrives
/// at the impostor instead. No file mode prevents this. A root-owned directory
/// stops `shepherdd` binding at all, and the sticky bit restricts deletion to
/// the file's *owner*, which an activity is: it shares the daemon's uid. Both
/// were measured, not assumed.
///
/// So the client checks who answered, the same way the daemon checks who
/// called. Shepherd's own clients — the launcher, the HUD, the one-shots sway
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
/// to start a process for it in a cgroup that is in no shepherd scope at all.
/// That process is refused here because it is not *in shepherdd's own cgroup*,
/// which is a different question from whether it is in a scope we made.
#[derive(Debug, Clone)]
pub struct PeerPolicy {
    /// `Some` when the allow-list is armed, carrying the cgroup id every
    /// accepted peer must match. `None` disarms it (the dev opt-out), and the
    /// uid-only classification from before this check applies.
    own_cgroup: Option<u64>,
    own_uid: u32,
}

impl PeerPolicy {
    /// Accept anything at this uid, as shepherdd did before #144.
    ///
    /// For development, where the whole stack runs inside one shell's cgroup
    /// and any client started from another terminal would otherwise be refused.
    pub fn unrestricted() -> Self {
        Self {
            own_cgroup: None,
            own_uid: nix::unistd::getuid().as_raw(),
        }
    }

    /// Accept only root, and peers in shepherdd's own cgroup.
    ///
    /// Fails if our own cgroup cannot be read, because a policy that cannot
    /// name what it accepts would refuse every client — better to say so at
    /// startup than to bring up a session nothing can talk to.
    pub fn restricted() -> Result<Self, PeerError> {
        Ok(Self {
            own_cgroup: Some(own_cgroup_id()?),
            own_uid: nix::unistd::getuid().as_raw(),
        })
    }

    /// Whether the allow-list is armed.
    pub fn is_restricted(&self) -> bool {
        self.own_cgroup.is_some()
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
        // everything shepherdd could do for it. A local operator's `sudo`
        // reaches the daemon from any cgroup, which is what makes the check
        // survivable for administration.
        if peer_uid == Some(0) {
            return Ok(ClientRole::Admin);
        }

        let Some(own) = self.own_cgroup else {
            // Disarmed: exactly the classification shepherdd shipped before.
            return Ok(match peer_uid {
                Some(u) if u == self.own_uid => ClientRole::Admin,
                _ => ClientRole::Shell,
            });
        };

        match peer_cgroup_id(fd) {
            Ok(peer) if peer == own => Ok(ClientRole::Admin),
            Ok(peer) => Err(self.reject(
                fd,
                format!(
                    "a process at this uid connected from cgroup id {peer}, which is not \
                     shepherd's own ({own}) — activities live in their own \
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
        // Both readers have to work on whatever runs the tests; this is the
        // one assertion that would catch a kernel too old for the design.
        let id = own_cgroup_id().expect("own cgroup id");
        assert_ne!(id, 0);
        let path = own_cgroup_path().expect("own cgroup path");
        assert!(path.starts_with('/'), "unexpected cgroup path {path:?}");
    }

    #[test]
    fn an_armed_policy_accepts_a_peer_in_our_own_cgroup() {
        // A socketpair's peer is this very process, so it is in our cgroup by
        // construction — the launcher/HUD/one-shot case, all of which are
        // descendants of the session shepherdd is in.
        let policy = PeerPolicy::restricted().expect("read own cgroup");
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let role = policy
            .classify(a.as_fd(), Some(nix::unistd::getuid().as_raw()))
            .expect("a peer in our own cgroup is accepted");
        assert_eq!(role, ClientRole::Admin);
    }

    #[test]
    fn an_armed_policy_refuses_a_peer_from_another_cgroup() {
        // Stand in for an activity by claiming a cgroup id that cannot be ours.
        // The real separation is exercised end-to-end in the e2e suite, which
        // can put a peer in a scope of its own; here the point is only that a
        // mismatch is refused rather than logged and waved through.
        let policy = PeerPolicy {
            own_cgroup: Some(own_cgroup_id().expect("own cgroup id").wrapping_add(1)),
            own_uid: nix::unistd::getuid().as_raw(),
        };
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let err = policy
            .classify(a.as_fd(), Some(nix::unistd::getuid().as_raw()))
            .expect_err("a peer outside shepherd's cgroup must be refused");
        assert!(
            err.reason.contains("not shepherd's own"),
            "unhelpful refusal: {}",
            err.reason
        );
    }

    #[test]
    fn root_is_accepted_from_any_cgroup() {
        // `sudo shepherd …` comes from the operator's own login session, which
        // is never shepherdd's cgroup. Without this the check would lock an
        // administrator out of their own device.
        let policy = PeerPolicy {
            own_cgroup: Some(own_cgroup_id().expect("own cgroup id").wrapping_add(1)),
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
        let policy = PeerPolicy::unrestricted();
        assert!(!policy.is_restricted());
        let (a, _b) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        let uid = nix::unistd::getuid().as_raw();
        assert_eq!(
            policy.classify(a.as_fd(), Some(uid)).expect("accepted"),
            ClientRole::Admin
        );
        assert_eq!(
            policy.classify(a.as_fd(), Some(uid + 1)).expect("accepted"),
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
            "/system.slice/shepherd-abc.scope"
        ));
        // Not the user manager, just something that looks like it.
        assert!(!is_delegated_user_cgroup(
            "/user.slice/user@notauid.service"
        ));
    }
}
