/**
 * Cross-references between pages.
 *
 * The editor has no router — tabs are React state — so "show me that category"
 * is a request passed down rather than a URL. The nonce is what makes it a
 * *request* rather than a piece of state: asking for the same subject twice in
 * a row must re-open it, even though the subject has not changed, because the
 * user may have closed it in between.
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
