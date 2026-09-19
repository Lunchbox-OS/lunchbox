// Client for the login endpoints (issue #156).
//
// Separate from `client.ts` because these are the only calls that are *not*
// RPC: they are HTTP endpoints under `/api/v1/auth`, five of them reachable
// without a credential, and the thing they hand back is a cookie rather than a
// value. See `crates/lunchbox-http/src/handlers/auth.rs`.
//
// Nothing here ever sees the session token. It arrives as an `HttpOnly`
// cookie, which is the point: a script in this page cannot read it, so an XSS
// in the UI cannot walk off with a permanent administrator credential the way
// it could with the `localStorage` token this replaced.

import axios from "axios";
import type { LoginRequestInfo, WebAuthStatus, WebSessionInfo } from "./wire-types.generated";

export type { LoginRequestInfo, WebAuthStatus, WebSessionInfo };

/** Thrown for any non-2xx; `retryAfter` is set only on a lockout. */
export class AuthError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
    public retryAfter?: number,
  ) {
    super(message);
  }
}

function getBase(): string {
  return localStorage.getItem("apiBase") ?? "";
}

/** The machine token, for the cross-origin case. See `ConnectionSettings`. */
function getMachineToken(): string | null {
  return localStorage.getItem("apiToken");
}

const http = axios.create({
  // Same-origin is the normal case — the daemon serves this page — and
  // `withCredentials` is what makes the browser attach the session cookie when
  // `apiBase` points somewhere else.
  withCredentials: true,
});

http.interceptors.request.use((config) => {
  config.baseURL = `${getBase()}/api/v1`;
  const token = getMachineToken();
  if (token) config.headers.Authorization = `Bearer ${token}`;
  return config;
});

http.interceptors.response.use(
  (res) => res,
  (err) => {
    if (axios.isAxiosError(err) && err.response) {
      const body = err.response.data as { error?: string; message?: string } | undefined;
      const retryAfter = Number(err.response.headers["retry-after"]);
      throw new AuthError(
        err.response.status,
        body?.error ?? "unknown",
        body?.message ?? err.response.statusText,
        Number.isFinite(retryAfter) ? retryAfter : undefined,
      );
    }
    throw err;
  },
);

/** Whether this device has a password yet, and whether a phone can approve. */
export const getAuthStatus = async (): Promise<WebAuthStatus> =>
  (await http.get<WebAuthStatus>("/auth/status")).data;

/** First-run enrolment with the code shown on the device's own screen. */
export const completeSetup = async (
  code: string,
  password: string,
): Promise<WebSessionInfo> =>
  (await http.post<{ session: WebSessionInfo }>("/auth/setup", { code, password })).data.session;

export const login = async (password: string): Promise<WebSessionInfo> =>
  (await http.post<{ session: WebSessionInfo }>("/auth/login", { password })).data.session;

export interface WhoAmI {
  session: WebSessionInfo | null;
  /** True for a bearer token that authenticates requests but is not a login. */
  machine: boolean;
}

export const whoAmI = async (): Promise<WhoAmI> =>
  (await http.get<WhoAmI>("/auth/session")).data;

export const signOut = async (): Promise<void> => {
  await http.post("/auth/signout");
};

export const listSessions = async (): Promise<WebSessionInfo[]> =>
  (await http.get<WebSessionInfo[]>("/auth/sessions")).data;

export const revokeSession = async (id: string): Promise<void> => {
  await http.delete(`/auth/sessions/${encodeURIComponent(id)}`);
};

// --- companion approval ----------------------------------------------------

export interface RequestedLogin {
  /** Secret capability; kept in memory only and never shown to the user. */
  poll_token: string;
  /** The six digits to display, for comparison against the phone. */
  code: string;
  expires_at: string;
}

export const requestApproval = async (): Promise<RequestedLogin> =>
  (await http.post<RequestedLogin>("/auth/request")).data;

export type PollAnswer =
  | { state: "pending" }
  | { state: "approved"; session: WebSessionInfo }
  | { state: "denied" }
  | { state: "expired" };

export const pollApproval = async (pollToken: string): Promise<PollAnswer> =>
  (await http.post<PollAnswer>("/auth/poll", { poll_token: pollToken })).data;
