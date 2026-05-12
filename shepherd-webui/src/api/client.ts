import axios from "axios";
import type {
  DailyOverride,
  EntryView,
  LaunchResponse,
  SessionInfo,
  UsageStat,
  VolumeInfo,
  WindowsResponse,
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
      const message = err.response.data?.message ?? err.response.statusText;
      throw new ApiError(err.response.status, message);
    }
    throw err;
  },
);

async function req<T>(url: string, config: import("axios").AxiosRequestConfig = {}): Promise<T> {
  const res = await axiosInstance.request<T>({ url, ...config });
  return res.data;
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
    data: { entry_id },
  });

export const stopSession = () =>
  req<void>("/sessions/current", { method: "DELETE" });

export const extendSession = (seconds: number) =>
  req<{ new_deadline: string | null }>("/sessions/current/extend", {
    method: "POST",
    data: { seconds },
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
    data: { date, availability, quota_delta_seconds },
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

// Volume
export const getVolume = () => req<VolumeInfo>("/volume");
export const setVolumePercent = (percent: number) =>
  req<VolumeInfo>("/volume", {
    method: "PUT",
    data: { percent },
  });
export const setVolumeMuted = (muted: boolean) =>
  req<VolumeInfo>("/volume", {
    method: "PUT",
    data: { muted },
  });

// Config
export const reloadConfig = () =>
  req<{ entry_count: number }>("/config/reload", { method: "POST" });

// User
export const logoutUser = () =>
  req<void>("/user/logout", { method: "POST" });

// Debug
export const listWindows = () => req<WindowsResponse>("/debug/windows");
export const closeWindow = (id: number) =>
  req<void>(`/debug/windows/${id}/close`, { method: "POST" });
export const hideWindow = (id: number) =>
  req<void>(`/debug/windows/${id}/hide`, { method: "POST" });
export const showWindow = (id: number) =>
  req<void>(`/debug/windows/${id}/show`, { method: "POST" });

// Build SSE URL with auth token header workaround (use query param)
export function sseUrl(): string {
  const base = getBase();
  const token = getToken();
  const url = `${base}/api/v1/events`;
  return token ? `${url}?token=${encodeURIComponent(token)}` : url;
}
