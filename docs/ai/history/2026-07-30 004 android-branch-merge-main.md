# Second origin/main merge into the Android branch (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> merge origin/main forward and re-run rpc-codegen

Pre-review housekeeping. The first forward merge is
[2026-07-29 002](./2026-07-29%20002%20android-branch-merge-main.md); this one is
the boring counterpart, recorded because "the merge was clean" is itself the
thing a reviewer wants to know.

## What came across

Two commits: `c3c49a9` (`fix(hud): scale the close-confirmation buttons under the
XWayland DPI hack`) and its merge `c3204f0`. A HUD CSS fix, no wire types, no
RPC methods.

**No conflicts.** `crates/shepherd-hud/src/app.rs` auto-merged: main added a
`font-size` on the `.confirm-close-popover button` node, the branch's HUD work
(the Android back button + statusbar occlusion) is elsewhere in the file. Both
are present afterwards — checked by reading the merged CSS block, not just by
trusting the merge driver.

## The codegen re-run

The previous merge's lesson was that generated wire outputs must be regenerated
after any forward merge, because the drift test is the only thing that catches
staleness and generated files are never in the conflict set.

Re-ran `cargo run -p shepherd-wire-codegen --bin rpc-codegen`: **no diff**. That
is the expected result here (main touched no wire type, and the branch's own
`startup_busy` regen was already committed), but the point is that it was run —
a merge that *did* carry a wire change would have shown up here rather than in
CI.

## Verification on the merged tree

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, `cargo test --workspace --all-targets` (46 test binaries, 0
failures), `./scripts/shepherd version check`, `./scripts/shepherd config
validate config.example.toml`, `./scripts/ci/check-arch-neutral.sh`, CI's
shellcheck invocation, `npx tsc --noEmit` in `shepherd-webui`, and
`:app:testDebugUnitTest` in `companion-android`.

Two environment notes on top of the previous doc's (JDK 21, and rebuilding
`validate-config` so the example config isn't validated against a stale binary):

- The companion Gradle build also needs `ANDROID_HOME=/opt/android-sdk` on this
  box — without it, `:app:testDebugUnitTest` fails at configuration time with
  "SDK location not found", which reads like a Gradle problem rather than a
  missing env var.
