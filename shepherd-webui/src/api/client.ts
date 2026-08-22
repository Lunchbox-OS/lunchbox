// JSON-RPC client for shepherdd's management HTTP API.
//
// Every operation dispatches through a single `POST /api/v1/rpc`
// endpoint with body `{ method, params }`; the server routes into
// `ManagementService::dispatch_json` and hands back either the
// method's return value (2xx) or `{ error, message }` (4xx/5xx).
// See `crates/shepherd-http/src/handlers/rpc.rs`.

import axios from "axios";
import type { RpcMethod } from "./rpc-methods.generated";
import type {
  BrightnessInfo,
  DailyOverride,
  GroupView,
  TokenStatus,
  EntryView,
  ReasonCode,
  SessionInfo,
  UsageStat,
  VolumeInfo,
  WindowInfo,
} from "./types";

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

function getToken(): string | null {
  return localStorage.getItem("apiToken");
}

const axiosInstance = axios.create();

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
 * Dispatch a single RPC. On success returns the trait method's
 * return value decoded as `T`; on server-side errors throws
 * `ApiError` with the status code + machine-readable code.
 */
async function call<T>(method: RpcMethod, params: unknown = {}): Promise<T> {
  const res = await axiosInstance.post<T>("/rpc", { method, params });
  return res.data;
}

// ---------------------------------------------------------------------------
// Typed helpers — one per RPC used by the UI.
// ---------------------------------------------------------------------------

// Health
export const getHealth = () => call<{ live: boolean; ready: boolean }>("health");

// Entries
export const listEntries = (at?: Date) =>
  call<EntryView[]>("list_entries", at ? { at: at.toISOString() } : {});

export const getEntry = (id: string) => call<EntryView>("get_entry", { id });

// Sessions
export const getCurrentSession = () =>
  call<SessionInfo | null>("current_session");

/**
 * `LaunchOutcome` on the wire is serde-externally-tagged:
 * `{"Approved": {...}}` or `{"Denied": {...}}`. The UI code was
 * written against a friendlier `{ result: "approved" | "denied", ...}`
 * shape from the old REST endpoint, so we normalise here.
 */
type LaunchWire =
  | { Approved: { session_id: string; deadline: string | null } }
  | { Denied: { reasons: ReasonCode[] } };

export type LaunchResponse =
  | { result: "approved"; session_id: string; deadline: string | null }
  | { result: "denied"; reasons: ReasonCode[] };

export const launchSession = async (
  entry_id: string,
): Promise<LaunchResponse> => {
  const wire = await call<LaunchWire>("launch", { id: entry_id });
  if ("Approved" in wire) {
    return {
      result: "approved",
      session_id: wire.Approved.session_id,
      deadline: wire.Approved.deadline,
    };
  }
  return { result: "denied", reasons: wire.Denied.reasons };
};

export const stopSession = () => call<null>("stop_current");

export const extendSession = (seconds: number) =>
  call<{ new_deadline: string | null }>("extend_current", { seconds });

export const listGroups = () => call<GroupView[]>("list_groups", {});

/**
 * Grant (positive) or revoke (negative) banked time on a token gate.
 * `subject` is an entry ID, or `group:<id>` for a whole category.
 */
export const adjustTokens = (subject: string, delta_seconds: number) =>
  call<TokenStatus>("adjust_tokens", { id: subject, delta_seconds });

// Daily overrides
export const listOverrides = (date?: string) =>
  call<DailyOverride[]>("list_overrides", date ? { date } : {});

export const getOverride = (entry_id: string, date?: string) =>
  call<DailyOverride | null>(
    "get_override",
    date ? { id: entry_id, date } : { id: entry_id },
  );

export const upsertOverride = (
  entry_id: string,
  availability: boolean | null,
  quota_delta_seconds: number | null,
  date?: string,
) =>
  call<DailyOverride>("upsert_override", {
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
  await call<{ deleted: boolean }>(
    "delete_override",
    date ? { id: entry_id, date } : { id: entry_id },
  );
};

// Usage
export const getUsage = (from?: string, to?: string) => {
  const params: Record<string, string> = {};
  if (from) params.from = from;
  if (to) params.to = to;
  return call<UsageStat[]>("usage_all", params);
};

export const getEntryUsage = (entry_id: string, from?: string, to?: string) => {
  const params: Record<string, string> = { id: entry_id };
  if (from) params.from = from;
  if (to) params.to = to;
  return call<UsageStat[]>("usage_entry", params);
};

// Volume
export const getVolume = () => call<VolumeInfo>("get_volume");
export const setVolumePercent = (percent: number) =>
  call<VolumeInfo>("set_volume", { percent });
export const setVolumeMuted = (muted: boolean) =>
  call<VolumeInfo>("set_mute", { muted });

// Brightness
export const getBrightness = () => call<BrightnessInfo>("get_brightness");
export const setBrightnessPercent = (percent: number) =>
  call<BrightnessInfo>("set_brightness", { percent });
export const setAutoBrightness = (enabled: boolean) =>
  call<BrightnessInfo>("set_auto_brightness", { enabled });

// Config — `reload_config` returns `{entry_count: number}` (wrap_result).
export const reloadConfig = () =>
  call<{ entry_count: number }>("reload_config");

// User
export const logoutUser = () => call<null>("logout");

// Debug
// `list_windows` has no `wrap_result`, so the daemon answers with a bare
// array rather than `{windows: [...]}`.
export const listWindows = () => call<WindowInfo[]>("list_windows");
export const closeWindow = (id: number) =>
  call<null>("act_on_window", { id, action: "close" });
export const hideWindow = (id: number) =>
  call<null>("act_on_window", { id, action: "hide" });
export const showWindow = (id: number) =>
  call<null>("act_on_window", { id, action: "show" });

// Build SSE URL with auth token (query param — EventSource can't set
// headers). The server-side handler still lives at GET /api/v1/events.
export function sseUrl(): string {
  const base = getBase();
  const token = getToken();
  const url = `${base}/api/v1/events`;
  return token ? `${url}?token=${encodeURIComponent(token)}` : url;
}
