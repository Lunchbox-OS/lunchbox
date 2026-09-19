//! BPF loading + cgroup attach for the `apply-cgroup` subcommand.
//!
//! Loads the embedded `lunchbox-firewall-bpf` ELF, populates the LPM-trie
//! and `DEFAULT` maps from the rule list, then attaches the program to the
//! caller-supplied cgroup via the legacy `BPF_PROG_ATTACH` syscall.
//!
//! Why legacy attach: aya's high-level `CgroupSkb::attach()` uses
//! `BPF_LINK_CREATE` on kernel ≥ 5.7, which ties the attachment to a file
//! descriptor — when the helper exits the fd is closed and the program is
//! detached. We need the attach to outlive the helper. Legacy
//! `BPF_PROG_ATTACH` keeps a kernel-level reference from the cgroup to the
//! program, so the program stays attached until the cgroup is destroyed.
//!
//! Map population uses aya's `LpmTrie` and `Array` wrappers, which is the
//! one place we benefit from aya — aya gives us correct map fd lifetime
//! and key/value layout.

use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsFd, AsRawFd};
use std::path::Path;

use aya::{
    Ebpf, include_bytes_aligned,
    maps::{Array, LpmTrie, lpm_trie::Key},
    programs::CgroupSkb,
};

/// The BPF object built by `build.rs`, embedded at 32-byte alignment.
///
/// `include_bytes_aligned!` rather than plain `include_bytes!`: the latter
/// yields alignment-1 data, and aya hands the slice straight to the `object`
/// crate, whose ELF header parse is a zero-copy cast that rejects anything
/// not aligned to `align_of::<elf::FileHeader64>()` (8). A plain
/// `include_bytes!` therefore loads only when the linker happens to place the
/// blob on an 8-byte boundary — a per-build coin flip that any unrelated
/// change to this crate's rodata can lose. See issue #151, where the flatpak
/// firewall silently stopped enforcing with
/// `ParseError(ElfError(Error("Invalid ELF header size or alignment")))`.
static BPF_OBJ: &[u8] = include_bytes_aligned!(env!("SHEPHERD_FIREWALL_BPF_OBJ"));

/// Build, populate, and attach the firewall program to `cgroup_path`.
/// On success the program is left attached to the cgroup; the kernel
/// detaches it automatically when the cgroup is destroyed.
pub fn apply_cgroup(
    cgroup_path: &str,
    default_deny: bool,
    allow_rules: &[String],
    deny_rules: &[String],
) -> Result<(), Error> {
    // The `{e:?}` (Debug) is deliberate: aya's `ParseError::ElfError(_)`
    // doesn't expose its inner error via `source()`, so Debug is the only
    // way to see why a load actually failed. (An earlier comment here
    // claimed that removing the Debug formatting *caused* load failures,
    // and blamed monomorphization. It was really the alignment bug fixed
    // in #151: adding or removing the formatting shifted this crate's
    // rodata, moving the embedded object on and off an 8-byte boundary.)
    let mut bpf =
        Ebpf::load(BPF_OBJ).map_err(|e| Error::other("aya load", format!("{e}: {e:?}")))?;

    populate_default(&mut bpf, default_deny)?;
    populate_rules(&mut bpf, allow_rules, "ALLOW_V4", "ALLOW_V6")?;
    populate_rules(&mut bpf, deny_rules, "DENY_V4", "DENY_V6")?;

    // Load the program (kernel verifier runs here).
    let prog: &mut CgroupSkb = bpf
        .program_mut("shepherd_firewall")
        .ok_or_else(|| Error::msg("BPF program 'shepherd_firewall' missing in object"))?
        .try_into()
        .map_err(|e: aya::programs::ProgramError| Error::other("program try_into", e))?;
    prog.load()
        .map_err(|e| Error::other("CgroupSkb::load (verifier?)", e))?;

    let prog_fd = prog
        .fd()
        .map_err(|e| Error::other("CgroupSkb::fd", e))?
        .as_fd()
        .as_raw_fd();

    let cgroup = File::open(Path::new(cgroup_path))
        .map_err(|e| Error::other(format!("open cgroup {}", cgroup_path), e))?;

    bpf_prog_attach_legacy(prog_fd, cgroup.as_raw_fd(), BPF_CGROUP_INET_EGRESS)?;

    // Don't drop `bpf`. Drop would close program / map fds, decrementing the
    // kernel refcounts. The legacy attach we just did adds its own refcount,
    // so the program survives `exit(0)` — but only if we don't *actively*
    // detach from the userspace side, which dropping would otherwise do.
    // (Map fds close on process exit, but the program holds its own
    // reference to the maps, so they outlive us.)
    std::mem::forget(bpf);

    Ok(())
}

fn populate_default(bpf: &mut Ebpf, default_deny: bool) -> Result<(), Error> {
    let map = bpf
        .map_mut("DEFAULT")
        .ok_or_else(|| Error::msg("map DEFAULT missing"))?;
    let mut arr: Array<_, u8> =
        Array::try_from(map).map_err(|e| Error::other("DEFAULT Array::try_from", e))?;
    let v: u8 = if default_deny { 0 } else { 1 };
    arr.set(0, v, 0)
        .map_err(|e| Error::other("DEFAULT set", e))?;
    Ok(())
}

/// Insert each `RULE` into the v4 or v6 LPM trie (chosen per-rule by the
/// parsed address family). systemd address tokens (`any`, `localhost`,
/// `link-local`, `multicast`) are expanded into one or two CIDRs each.
fn populate_rules(
    bpf: &mut Ebpf,
    rules: &[String],
    v4_map: &str,
    v6_map: &str,
) -> Result<(), Error> {
    // Build the cidr lists first; we want zero map writes if everything is
    // invalid. (The helper has already validated each rule, but defensive
    // coding here lets the BPF crate evolve independently.)
    let mut v4_cidrs: Vec<(Ipv4Addr, u32)> = Vec::new();
    let mut v6_cidrs: Vec<(Ipv6Addr, u32)> = Vec::new();
    for r in rules {
        match parse_rule(r) {
            Ok(ParsedRule::V4(cs)) => v4_cidrs.extend(cs),
            Ok(ParsedRule::V6(cs)) => v6_cidrs.extend(cs),
            Ok(ParsedRule::Mixed { v4, v6 }) => {
                v4_cidrs.extend(v4);
                v6_cidrs.extend(v6);
            }
            Err(e) => return Err(Error::msg(format!("rule {r}: {e}"))),
        }
    }

    if !v4_cidrs.is_empty() {
        let map = bpf
            .map_mut(v4_map)
            .ok_or_else(|| Error::msg(format!("map {v4_map} missing")))?;
        let mut trie: LpmTrie<_, u32, u8> =
            LpmTrie::try_from(map).map_err(|e| Error::other(format!("{v4_map} try_from"), e))?;
        for (addr, prefix) in v4_cidrs {
            // BPF program reads the destination IPv4 address from skb in
            // network byte order (big-endian); LPM trie keys for IPv4 must
            // match the byte order the program will compare.
            let key = Key::new(prefix, u32::from(addr).to_be());
            trie.insert(&key, 1u8, 0)
                .map_err(|e| Error::other(format!("{v4_map} insert"), e))?;
        }
    }

    if !v6_cidrs.is_empty() {
        let map = bpf
            .map_mut(v6_map)
            .ok_or_else(|| Error::msg(format!("map {v6_map} missing")))?;
        let mut trie: LpmTrie<_, [u8; 16], u8> =
            LpmTrie::try_from(map).map_err(|e| Error::other(format!("{v6_map} try_from"), e))?;
        for (addr, prefix) in v6_cidrs {
            let key = Key::new(prefix, addr.octets());
            trie.insert(&key, 1u8, 0)
                .map_err(|e| Error::other(format!("{v6_map} insert"), e))?;
        }
    }

    Ok(())
}

type V4Cidr = (Ipv4Addr, u32);
type V6Cidr = (Ipv6Addr, u32);

enum ParsedRule {
    V4(Vec<V4Cidr>),
    V6(Vec<V6Cidr>),
    Mixed { v4: Vec<V4Cidr>, v6: Vec<V6Cidr> },
}

fn parse_rule(r: &str) -> Result<ParsedRule, String> {
    match r {
        // systemd's "any" expands to 0.0.0.0/0 + ::/0. We mirror that.
        "any" => Ok(ParsedRule::Mixed {
            v4: vec![(Ipv4Addr::UNSPECIFIED, 0)],
            v6: vec![(Ipv6Addr::UNSPECIFIED, 0)],
        }),
        // 127.0.0.0/8 + ::1/128
        "localhost" => Ok(ParsedRule::Mixed {
            v4: vec![(Ipv4Addr::new(127, 0, 0, 0), 8)],
            v6: vec![(Ipv6Addr::LOCALHOST, 128)],
        }),
        // 169.254.0.0/16 + fe80::/64
        "link-local" => Ok(ParsedRule::Mixed {
            v4: vec![(Ipv4Addr::new(169, 254, 0, 0), 16)],
            v6: vec![(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 64)],
        }),
        // 224.0.0.0/4 + ff00::/8
        "multicast" => Ok(ParsedRule::Mixed {
            v4: vec![(Ipv4Addr::new(224, 0, 0, 0), 4)],
            v6: vec![(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8)],
        }),
        other => parse_cidr(other),
    }
}

fn parse_cidr(s: &str) -> Result<ParsedRule, String> {
    let (addr_str, prefix_str) = s
        .split_once('/')
        .map(|(a, p)| (a, Some(p)))
        .unwrap_or((s, None));
    let ip: IpAddr = addr_str
        .parse()
        .map_err(|e: std::net::AddrParseError| format!("addr parse: {e}"))?;
    let max_prefix: u32 = if ip.is_ipv4() { 32 } else { 128 };
    let prefix: u32 = match prefix_str {
        Some(p) => p
            .parse()
            .map_err(|e: std::num::ParseIntError| format!("prefix parse: {e}"))?,
        None => max_prefix,
    };
    if prefix > max_prefix {
        return Err(format!("prefix {prefix} > {max_prefix}"));
    }
    match ip {
        IpAddr::V4(v4) => Ok(ParsedRule::V4(vec![(v4, prefix)])),
        IpAddr::V6(v6) => Ok(ParsedRule::V6(vec![(v6, prefix)])),
    }
}

// ---------------------------------------------------------------------------
// Raw bpf() syscall for legacy PROG_ATTACH
// ---------------------------------------------------------------------------

const BPF_PROG_ATTACH: u64 = 8;
// Linux UAPI `enum bpf_attach_type` from <linux/bpf.h>:
//   BPF_CGROUP_INET_INGRESS = 0
//   BPF_CGROUP_INET_EGRESS  = 1
// systemd's IPAddressDeny= attaches to both ingress and egress, but for
// outbound TCP (the dominant kid-kiosk concern, and all our tests cover)
// egress alone is sufficient.
const BPF_CGROUP_INET_EGRESS: u32 = 1;

#[repr(C)]
#[derive(Default)]
struct BpfProgAttachAttr {
    target_fd: u32,
    attach_bpf_fd: u32,
    attach_type: u32,
    attach_flags: u32,
    replace_bpf_fd: u32,
}

fn bpf_prog_attach_legacy(prog_fd: i32, cgroup_fd: i32, attach_type: u32) -> Result<(), Error> {
    let mut attr = MaybeUninit::<BpfProgAttachAttr>::zeroed();
    // SAFETY: zeroed POD; we only touch fields below.
    let attr_ref = unsafe { attr.assume_init_mut() };
    attr_ref.target_fd = cgroup_fd as u32;
    attr_ref.attach_bpf_fd = prog_fd as u32;
    attr_ref.attach_type = attach_type;

    // SAFETY: SYS_bpf with BPF_PROG_ATTACH and a valid attr struct.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_bpf,
            BPF_PROG_ATTACH,
            attr.as_ptr(),
            std::mem::size_of::<BpfProgAttachAttr>(),
        )
    };
    if ret < 0 {
        return Err(Error::other(
            "bpf(BPF_PROG_ATTACH)",
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Error(String);

impl Error {
    fn msg(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    fn other(label: impl Into<String>, e: impl std::fmt::Display) -> Self {
        Self(format!("{}: {}", label.into(), e))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test for issue #151: the embedded BPF object was included
    /// with plain `include_bytes!`, which yields alignment-1 data. aya's ELF
    /// parse is a zero-copy cast and rejects a header that isn't 8-byte
    /// aligned, so `apply-cgroup` failed with "Invalid ELF header size or
    /// alignment" and every flatpak/snap entry ran unfiltered.
    #[test]
    fn embedded_bpf_object_is_aligned_and_parses() {
        assert_eq!(
            BPF_OBJ.as_ptr() as usize % 8,
            0,
            "embedded BPF object must be at least 8-byte aligned for aya's \
             zero-copy ELF parse; use include_bytes_aligned!, not include_bytes!"
        );

        // Parsing is the part this test cares about. Everything after it
        // (map creation, the verifier) needs CAP_BPF, which unprivileged
        // test runs don't have — so only a parse failure is fatal here.
        if let Err(aya::EbpfError::ParseError(e)) = Ebpf::load(BPF_OBJ) {
            panic!("embedded BPF object failed to parse: {e:?}");
        }
    }

    fn collect(rule: &str) -> (Vec<V4Cidr>, Vec<V6Cidr>) {
        match parse_rule(rule).expect("parse") {
            ParsedRule::V4(v) => (v, vec![]),
            ParsedRule::V6(v) => (vec![], v),
            ParsedRule::Mixed { v4, v6 } => (v4, v6),
        }
    }

    #[test]
    fn tokens_expand_to_systemd_equivalents() {
        let (v4, v6) = collect("any");
        assert_eq!(v4, vec![(Ipv4Addr::UNSPECIFIED, 0)]);
        assert_eq!(v6, vec![(Ipv6Addr::UNSPECIFIED, 0)]);

        let (v4, v6) = collect("localhost");
        assert_eq!(v4, vec![(Ipv4Addr::new(127, 0, 0, 0), 8)]);
        assert_eq!(v6, vec![(Ipv6Addr::LOCALHOST, 128)]);

        let (v4, v6) = collect("link-local");
        assert_eq!(v4, vec![(Ipv4Addr::new(169, 254, 0, 0), 16)]);
        assert_eq!(v6, vec![(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 64)]);

        let (v4, v6) = collect("multicast");
        assert_eq!(v4, vec![(Ipv4Addr::new(224, 0, 0, 0), 4)]);
        assert_eq!(v6, vec![(Ipv6Addr::new(0xff00, 0, 0, 0, 0, 0, 0, 0), 8)]);
    }

    #[test]
    fn cidrs_round_trip() {
        let (v4, _) = collect("10.0.0.0/8");
        assert_eq!(v4, vec![(Ipv4Addr::new(10, 0, 0, 0), 8)]);
        let (_, v6) = collect("2001:db8::/32");
        assert_eq!(v6.len(), 1);
        assert_eq!(v6[0].1, 32);
        let (v4, _) = collect("127.0.0.1");
        assert_eq!(v4, vec![(Ipv4Addr::new(127, 0, 0, 1), 32)]);
    }

    #[test]
    fn invalid_cidr_rejected() {
        assert!(parse_rule("10.0.0.0/40").is_err());
        assert!(parse_rule("not-an-ip").is_err());
    }
}
