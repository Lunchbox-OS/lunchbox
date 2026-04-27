import type {
  DailyOverride,
  EntryView,
  LaunchResponse,
  MaintenanceState,
  SessionInfo,
  UsageStat,
  VolumeInfo,
} from "./types";

export class ApiError extends Error {
  constructor(
    public status: number,
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

async function req<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = getToken();
  const headers: Record<string, string> = {
    ...(init.body ? { "Content-Type": "application/json" } : {}),
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  };

  const res = await fetch(`${getBase()}/api/v1${path}`, {
    ...init,
    headers: { ...headers, ...(init.headers as Record<string, string> ?? {}) },
  });

  if (res.status === 204) return undefined as T;

  const body = await res.json().catch(() => ({ message: res.statusText }));
  if (!res.ok) {
    throw new ApiError(res.status, body?.message ?? res.statusText);
  }
  return body as T;
}

// Health
export const getHealth = () => req<{ live: boolean; ready: boolean }>("/health");

// Entries
export const listEntries = (at?: Date) =>
  req<EntryView[]>(at ? `/entries?at=${at.toISOString()}` : "/entries");

export const getEntry = (id: string) => req<EntryView>(`/entries/${id}`);

// Sessions
export const getCurrentSession = () => req<SessionInfo | null>("/sessions/current");

export const launchSession = (entry_id: string) =>
  req<LaunchResponse>("/sessions", {
    method: "POST",
    body: JSON.stringify({ entry_id }),
  });

export const stopSession = () =>
  req<void>("/sessions/current", { method: "DELETE" });

export const extendSession = (seconds: number) =>
  req<{ new_deadline: string | null }>("/sessions/current/extend", {
    method: "POST",
    body: JSON.stringify({ seconds }),
  });

// Daily overrides
export const listOverrides = (date?: string) =>
  req<DailyOverride[]>(date ? `/overrides?date=${date}` : "/overrides");

export const getOverride = (entry_id: string, date?: string) =>
  req<DailyOverride | null>(
    date ? `/overrides/${entry_id}?date=${date}` : `/overrides/${entry_id}`,
  );

export const upsertOverride = (
  entry_id: string,
  availability: boolean | null,
  quota_delta_seconds: number | null,
  date?: string,
) =>
  req<DailyOverride>(`/overrides/${entry_id}`, {
    method: "PUT",
    body: JSON.stringify({ date, availability, quota_delta_seconds }),
  });

export const deleteOverride = (entry_id: string, date?: string) =>
  req<void>(
    date ? `/overrides/${entry_id}?date=${date}` : `/overrides/${entry_id}`,
    { method: "DELETE" },
  );

// Usage
export const getUsage = (from?: string, to?: string) => {
  const params = new URLSearchParams();
  if (from) params.set("from", from);
  if (to) params.set("to", to);
  const qs = params.toString();
  return req<UsageStat[]>(qs ? `/usage?${qs}` : "/usage");
};

export const getEntryUsage = (entry_id: string, from?: string, to?: string) => {
  const params = new URLSearchParams();
  if (from) params.set("from", from);
  if (to) params.set("to", to);
  const qs = params.toString();
  return req<UsageStat[]>(qs ? `/usage/${entry_id}?${qs}` : `/usage/${entry_id}`);
};

// Maintenance
export const getMaintenance = () => req<MaintenanceState>("/maintenance");
export const enterMaintenance = () =>
  req<MaintenanceState>("/maintenance", { method: "POST" });
export const exitMaintenance = () =>
  req<void>("/maintenance", { method: "DELETE" });

// Volume
export const getVolume = () => req<VolumeInfo>("/volume");
export const setVolumePercent = (percent: number) =>
  req<VolumeInfo>("/volume", {
    method: "PUT",
    body: JSON.stringify({ percent }),
  });
export const setVolumeMuted = (muted: boolean) =>
  req<VolumeInfo>("/volume", {
    method: "PUT",
    body: JSON.stringify({ muted }),
  });

// Config
export const reloadConfig = () =>
  req<{ entry_count: number }>("/config/reload", { method: "POST" });

// Build SSE URL with auth token header workaround (use query param)
export function sseUrl(): string {
  const base = getBase();
  const token = getToken();
  const url = `${base}/api/v1/events`;
  // EventSource doesn't support custom headers; if a token is required,
  // pass it as a query param and the server would need to accept it that way.
  // For now, rely on same-origin or no-auth setups for SSE.
  return token ? `${url}?token=${encodeURIComponent(token)}` : url;
}
