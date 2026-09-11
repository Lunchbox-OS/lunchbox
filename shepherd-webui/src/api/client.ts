// JSON-RPC client for shepherdd's management HTTP API.
//
// Every operation dispatches through a single `POST /api/v1/rpc`
// endpoint with body `{ method, params }`; the server routes into
// `ManagementService::dispatch_json` and hands back either the
// method's return value (2xx) or `{ error, message }` (4xx/5xx).
// See `crates/shepherd-http/src/handlers/rpc.rs`.

import axios from "axios";
import type { RpcMethod, RpcParams, RpcResult } from "./rpc-methods.generated";
import type { ReasonCode } from "./types";
import { UNAUTHENTICATED_EVENT } from "../auth/AuthGate";

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

function getBase(): string {
  return localStorage.getItem("apiBase") ?? "";
}

/**
 * The *machine* token, since issue #156.
 *
 * A browser signed in normally has no token here at all: it authenticates with
 * an `HttpOnly` session cookie it cannot read. This survives for the one case a
 * cookie cannot serve — an `apiBase` pointing at a different origin, where the
 * browser will not send this origin's cookie — and for `npm run dev` against a
 * device. See `ConnectionSettings`.
 */
function getToken(): string | null {
  return localStorage.getItem("apiToken");
}

const axiosInstance = axios.create({
  // Attach the session cookie. Redundant same-origin, load-bearing when
  // `apiBase` points elsewhere.
  withCredentials: true,
});

axiosInstance.interceptors.request.use((config) => {
  config.baseURL = `${getBase()}/api/v1`;
  const token = getToken();
  if (token) config.headers.Authorization = `Bearer ${token}`;
  return config;
});

axiosInstance.interceptors.response.use(
  (res) => res,
  (err) => {
    if (axios.isAxiosError(err) && err.response) {
      // A session can expire, or be revoked from another device, while this
      // tab is open. Tell the gate rather than letting every query on the page
      // fail with its own unexplained error.
      if (err.response.status === 401) {
        window.dispatchEvent(new Event(UNAUTHENTICATED_EVENT));
      }
      const body = err.response.data as
        | { error?: string; message?: string }
        | undefined;
      throw new ApiError(
        err.response.status,
        body?.error ?? "unknown",
        body?.message ?? err.response.statusText,
      );
    }
    throw err;
  },
);

/**
 * Dispatch a single RPC. On success returns the trait method's return value;
 * on server-side errors throws `ApiError` with the status code and the
 * machine-readable code.
 *
 * Both halves are generated from the trait: `RpcParams` is what the method
 * takes and `RpcResult` is what it answers with, so a renamed parameter or a
 * changed return type is a build failure here rather than a runtime surprise.
 * Methods listed in `RPC_WRAP_FIELDS` answer with a single-key object, and
 * `RpcResult` says so — the unwrap stays visible at the call site below.
 */
async function call<M extends RpcMethod>(
  method: M,
  params: RpcParams<M>,
): Promise<RpcResult<M>> {
  const res = await axiosInstance.post<RpcResult<M>>("/rpc", { method, params });
  return res.data;
}

// ---------------------------------------------------------------------------
// Typed helpers — one per RPC used by the UI.
// ---------------------------------------------------------------------------

// Health
export const getHealth = () => call("health", {});

// Administrator mode (issue #154). The snapshot that carries `admin_mode` is
// `getServiceState`, declared with the other whole-snapshot readers below.
export const enterAdminMode = () => call("enter_admin_mode", {});
export const exitAdminMode = () => call("exit_admin_mode", {});
export const lockDevice = () => call("lock_device", {});
export const unlockDevice = () => call("unlock_device", {});

// Entries
export const listEntries = (at?: Date) =>
  call("list_entries", { at: at?.toISOString() });

export const getEntry = (id: string) => call("get_entry", { id });

// Sessions
export const getCurrentSession = () => call("current_session", {});

/**
 * `LaunchOutcome` on the wire is serde-externally-tagged:
 * `{"Approved": {...}}` or `{"Denied": {...}}`. The UI code was
 * written against a friendlier `{ result: "approved" | "denied", ...}`
 * shape from the old REST endpoint, so we normalise here.
 */
export type LaunchResponse =
  | { result: "approved"; session_id: string; deadline: string | null }
  | { result: "denied"; reasons: ReasonCode[] };

export const launchSession = async (
  entry_id: string,
): Promise<LaunchResponse> => {
  const wire = await call("launch", { id: entry_id });
  if ("Approved" in wire) {
    return {
      result: "approved",
      session_id: wire.Approved.session_id,
      deadline: wire.Approved.deadline ?? null,
    };
  }
  return { result: "denied", reasons: wire.Denied.reasons };
};

export const stopSession = () => call("stop_current", {});

export const extendSession = (seconds: number) =>
  call("extend_current", { seconds });

export const listGroups = () => call("list_groups", {});

/**
 * Grant (positive) or revoke (negative) banked time on a token gate.
 * `subject` is an entry ID, or `group:<id>` for a whole category.
 */
export const adjustTokens = (subject: string, delta_seconds: number) =>
  call("adjust_tokens", { id: subject, delta_seconds });

// Daily overrides
export const listOverrides = (date?: string) =>
  call("list_overrides", { date });

export const getOverride = (entry_id: string, date?: string) =>
  call("get_override", { id: entry_id, date });

export const upsertOverride = (
  entry_id: string,
  availability: boolean | null,
  quota_delta_seconds: number | null,
  date?: string,
) =>
  call("upsert_override", {
    id: entry_id,
    date,
    availability,
    quota_delta_seconds,
  });

// `delete_override` returns `{deleted: bool}` via `wrap_result`; the
// UI doesn't care about the boolean today, so match the previous void
// signature.
export const deleteOverride = async (
  entry_id: string,
  date?: string,
): Promise<void> => {
  await call("delete_override", { id: entry_id, date });
};

// Usage
export const getUsage = (from?: string, to?: string) =>
  call("usage_all", { from, to });

export const getEntryUsage = (entry_id: string, from?: string, to?: string) =>
  call("usage_entry", { id: entry_id, from, to });

// Volume
export const getVolume = () => call("get_volume", {});
export const setVolumePercent = (percent: number) =>
  call("set_volume", { percent });
export const setVolumeMuted = (muted: boolean) =>
  call("set_mute", { muted });

// Per-output volume limits
export const listAudioOutputs = () => call("list_audio_outputs", {});
export const setAudioOutputLimits = (
  output_key: string,
  max_volume: number | null,
) => call("set_audio_output_limits", { output_key, max_volume });
export const forgetAudioOutput = (output_key: string) =>
  call("forget_audio_output", { output_key });
export const selectAudioOutput = (output_key: string) =>
  call("select_audio_output", { output_key });

// Brightness
export const getBrightness = () => call("get_brightness", {});
export const setBrightnessPercent = (percent: number) =>
  call("set_brightness", { percent });
export const setAutoBrightness = (enabled: boolean) =>
  call("set_auto_brightness", { enabled });

// Config — `reload_config` returns `{entry_count: number}` (wrap_result).
export const reloadConfig = () => call("reload_config", {});

// Media — re-fetch playlists, sponsor segments and pending downloads now
// instead of waiting out their caches (issue #165). Returns as soon as the
// daemon accepts the request, not when the refresh finishes; what came of it
// shows up on the Device health page as a diagnostic.
export const refreshMedia = () => call("refresh_media", {});

// User
export const logoutUser = () => call("logout", {});

// Debug
// `list_windows` has no `wrap_result`, so the daemon answers with a bare
// array rather than `{windows: [...]}`.
/**
 * Administrator-facing conditions currently true of the device (issue #143).
 *
 * A call of its own rather than reading them off a state snapshot: this UI
 * queries per page and never fetches a whole snapshot, so the set would
 * otherwise be unreachable from the browser.
 */
export const listDiagnostics = () => call("list_diagnostics", {});

/**
 * Where this device is on the network, and where the web interface it is
 * serving this page from is listening (issue #182).
 */
export const getNetworkStatus = () => call("network_status", {});

/**
 * The whole service snapshot.
 *
 * Every other page queries the one thing it needs, so this has only two
 * callers, both of which want facts that live nowhere else: the network page
 * needs the connectivity checks (duplicating them onto `network_status` would
 * give the UI two sources for one fact), and the administrator page needs
 * `admin_mode` and `locked` (issue #154).
 */
export const getServiceState = () => call("service_state", {});

export const listWindows = () => call("list_windows", {});
export const closeWindow = (id: number) =>
  call("act_on_window", { id, action: "close" });
export const hideWindow = (id: number) =>
  call("act_on_window", { id, action: "hide" });
export const showWindow = (id: number) =>
  call("act_on_window", { id, action: "show" });
export const focusWindow = (id: number) =>
  call("act_on_window", { id, action: "focus" });

/**
 * Open the SSE event stream at `GET /api/v1/events`.
 *
 * Deliberately `fetch` and not `EventSource`. The only thing that ever argued
 * for putting the token in the query string was the `EventSource` constructor's
 * inability to set headers — and `require_auth` reads nothing but
 * `Authorization`, so that stream authenticated with nobody and silently closed
 * on any claimed device. Streaming the response body keeps one auth mechanism
 * for every request and no credential in a URL.
 *
 * The caller owns the framing (see `useEvents`) and the `AbortSignal`.
 */
export function openEventStream(signal: AbortSignal): Promise<Response> {
  const token = getToken();
  return fetch(`${getBase()}/api/v1/events`, {
    headers: token ? { Authorization: `Bearer ${token}` } : {},
    // Same reason as the axios instance: the session cookie is the credential
    // for a browser that signed in normally, and `fetch` does not send cookies
    // cross-origin without being told to.
    credentials: "include",
    signal,
    cache: "no-store", // long-lived stream; never serve it from cache
  });
}

// ---------------------------------------------------------------------------
// The policy file (issue #185)
//
// Not RPCs. `GET`/`PUT /api/v1/config` carry the config as text with an
// `ETag`, because the thing being moved is a file — see
// `crates/shepherd-http/src/handlers/config.rs` for why it is off the RPC
// endpoint entirely.
// ---------------------------------------------------------------------------

/** The device's config as it is on disk, plus the tag needed to write it back. */
export interface DeviceConfig {
  text: string;
  /** The `ETag` verbatim, quotes included. Pass it straight back. */
  etag: string | null;
}

export async function getDeviceConfig(): Promise<DeviceConfig> {
  const res = await axiosInstance.get<string>("/config", {
    // Without both of these axios sees TOML that happens to start with a
    // number and hands back something that is not a string.
    responseType: "text",
    transformResponse: (data: string) => data,
  });
  return { text: res.data, etag: res.headers.etag ?? null };
}

/**
 * Replace the device's config.
 *
 * `etag` is what the last read (or write) returned; `null` means "overwrite
 * whatever is there", which the daemon spells `If-Match: *`. A 412 means
 * somebody else — `sudoedit`, `shepherd install policy`, another browser —
 * wrote the file in between, and the honest answer is to re-read rather than
 * to retry.
 */
export async function putDeviceConfig(
  text: string,
  etag: string | null,
): Promise<DeviceConfig> {
  const res = await axiosInstance.put<{ version: string }>("/config", text, {
    headers: {
      "Content-Type": "text/plain; charset=utf-8",
      "If-Match": etag ?? "*",
    },
  });
  return { text, etag: res.headers.etag ?? `"${res.data.version}"` };
}
