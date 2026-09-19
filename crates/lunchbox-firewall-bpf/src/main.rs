//! cgroup_skb BPF program implementing the per-entry firewall.
//!
//! Loaded by `lunchbox-firewall-helper apply-cgroup` and attached to the
//! Snap/Flatpak runtime's scope cgroup. Inspects every egress packet, looks
//! up the destination address against an LPM trie, and returns 1 (pass) or
//! 0 (drop) based on the matching rule. If no rule matches, the per-program
//! default verdict applies.
//!
//! Maps:
//!   - ALLOW_V4 / DENY_V4: LPM tries keyed by /prefix + IPv4 address (BE)
//!   - ALLOW_V6 / DENY_V6: LPM tries keyed by /prefix + IPv6 address (BE)
//!   - DEFAULT:           single-entry array; value 1 = allow on miss, 0 = drop
//!
//! Look-up order: deny first (an explicit deny wins), then allow, then
//! default. This matches what systemd's IPAddressDeny/Allow does.

#![no_std]
#![no_main]

use aya_ebpf::{
    bindings::sk_action,
    macros::{cgroup_skb, map},
    maps::{Array, LpmTrie, lpm_trie::Key},
    programs::SkBuffContext,
};
use network_types::ip::{Ipv4Hdr, Ipv6Hdr};

// The BPF verifier rejects programs without a GPL-compatible license
// section, since IP filtering helpers are GPL-only kernel symbols. aya's
// loader also keys off this section to identify a valid BPF object.
#[unsafe(no_mangle)]
#[unsafe(link_section = "license")]
pub static LICENSE: [u8; 4] = *b"GPL\0";

const ETH_P_IP: u16 = 0x0800;
const ETH_P_IPV6: u16 = 0x86dd;

#[map(name = "ALLOW_V4")]
static ALLOW_V4: LpmTrie<u32, u8> = LpmTrie::with_max_entries(64, 0);

#[map(name = "DENY_V4")]
static DENY_V4: LpmTrie<u32, u8> = LpmTrie::with_max_entries(64, 0);

#[map(name = "ALLOW_V6")]
static ALLOW_V6: LpmTrie<[u8; 16], u8> = LpmTrie::with_max_entries(64, 0);

#[map(name = "DENY_V6")]
static DENY_V6: LpmTrie<[u8; 16], u8> = LpmTrie::with_max_entries(64, 0);

/// Single-element array. Value: 1 = allow on miss, 0 = drop on miss.
#[map(name = "DEFAULT")]
static DEFAULT: Array<u8> = Array::with_max_entries(1, 0);

#[cgroup_skb]
pub fn shepherd_firewall(ctx: SkBuffContext) -> i32 {
    match try_filter(&ctx) {
        Ok(verdict) => verdict,
        // On parse error we fall back to the default verdict; never panic
        // (the verifier rejects panics anyway).
        Err(()) => default_verdict(),
    }
}

fn try_filter(ctx: &SkBuffContext) -> Result<i32, ()> {
    // skb->protocol carries the EtherType in network byte order even for
    // socket-level cgroup_skb. The cgroup_skb program type runs at the
    // network-layer boundary, so the L3 header starts at offset 0 of the
    // packet view.
    let proto = u16::from_be(unsafe { (*ctx.skb.skb).protocol as u16 });

    match proto {
        ETH_P_IP => {
            let ip4: Ipv4Hdr = ctx.load(0).map_err(|_| ())?;
            // dst_addr is in network byte order; LpmTrie keys for IPv4 are
            // u32 in network byte order (the standard LPM trie convention).
            let dst_be = ip4.dst_addr;
            Ok(verdict_v4(dst_be))
        }
        ETH_P_IPV6 => {
            let ip6: Ipv6Hdr = ctx.load(0).map_err(|_| ())?;
            // network-types exposes the IPv6 address as an in6_addr union;
            // u6_addr8 is the 16-byte view in network byte order.
            let dst = unsafe { ip6.dst_addr.in6_u.u6_addr8 };
            Ok(verdict_v6(dst))
        }
        _ => {
            // Non-IP traffic (e.g. ARP) falls through to the default. Egress
            // ARP doesn't usually traverse cgroup_skb anyway.
            Ok(default_verdict())
        }
    }
}

#[inline(always)]
fn verdict_v4(dst_be: u32) -> i32 {
    // Deny first, then allow, then default.
    if LpmTrie::get(&DENY_V4, &Key::new(32, dst_be)).is_some() {
        return sk_action::SK_DROP as i32;
    }
    if LpmTrie::get(&ALLOW_V4, &Key::new(32, dst_be)).is_some() {
        return sk_action::SK_PASS as i32;
    }
    default_verdict()
}

#[inline(always)]
fn verdict_v6(dst: [u8; 16]) -> i32 {
    if LpmTrie::get(&DENY_V6, &Key::new(128, dst)).is_some() {
        return sk_action::SK_DROP as i32;
    }
    if LpmTrie::get(&ALLOW_V6, &Key::new(128, dst)).is_some() {
        return sk_action::SK_PASS as i32;
    }
    default_verdict()
}

#[inline(always)]
fn default_verdict() -> i32 {
    let v = DEFAULT.get(0).copied().unwrap_or(0);
    if v == 1 {
        sk_action::SK_PASS as i32
    } else {
        sk_action::SK_DROP as i32
    }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // BPF can't panic; loop forever. The verifier will reject any path that
    // can reach this in practice.
    loop {}
}

