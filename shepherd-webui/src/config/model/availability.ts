/**
 * The shape of a subject's availability, whose types are generated from
 * `crates/shepherd-config-wasm/src/windows.rs`.
 *
 * Spans are half-open minute ranges from local midnight, seven days Monday
 * first, matching the day bitmask's bit order.
 */
import type { Span } from "./wasm-types.generated";

export type { AvailabilityView, Span } from "./wasm-types.generated";

/**
 * Seven days of spans, Monday first.
 *
 * Hand-written because `Week` is a Rust type alias rather than a struct, so
 * `schemars` inlines it: every `Week` in the generated `AvailabilityView` reads
 * as `Span[][]`. This names it back for the grid, which passes weeks around.
 */
export type Week = Span[][];

export const MINUTES_PER_DAY = 1440;
