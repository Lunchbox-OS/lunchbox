/**
 * Cross-references between pages.
 *
 * The editor has no router — tabs are React state — so "show me that category"
 * is a request passed down rather than a URL.
 *
 * A request is **one-shot**: whoever acts on it must call `onHandled`, which
 * clears it. That is not tidiness. Pages are conditionally rendered, so leaving
 * a tab unmounts one and returning mounts it fresh — and a mount runs every
 * effect regardless of its deps. A request left lying around therefore fires
 * again every time you come back to that tab, which showed up as an activity
 * that would not stop re-opening itself.
 *
 * The nonce is a separate concern: it distinguishes *repeat* requests for the
 * same subject, which a bare id could not, since asking twice must re-open
 * something the user closed in between.
 */
import type { Subject } from "./doc/patches";

export interface FocusRequest {
  subject: Subject;
  /** Bumped on every request, so repeats are distinguishable. */
  nonce: number;
}

/** Narrow a request to one kind of subject, for a page that only handles that kind. */
export const focusFor = (
  focus: FocusRequest | null,
  kind: Subject["kind"],
): FocusRequest | null => (focus && focus.subject.kind === kind ? focus : null);
