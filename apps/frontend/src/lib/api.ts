// --- Types ---

export interface UserListItem {
  id: string;
  display_name: string;
  avatar_url: string | null;
  latest_bpm: number | null;
  has_pulsoid_token: boolean;
  created_at: number;
}

export interface User {
  id: string;
  display_name: string;
  avatar_url: string | null;
  timezone: string;
  created_at: number;
  updated_at: number;
}

export interface PulsoidTokenStatus {
  source: "oauth" | "manual";
  last_connected_at: number | null;
  last_error: string | null;
}

export interface HeartRateRecord {
  bpm: number;
  timestamp: number;
}

// --- API functions ---

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  if (res.status === 401) {
    window.location.href = "/login";
    throw new Error("Unauthorized");
  }
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export function getUsers() {
  return fetchJson<UserListItem[]>("/api/users");
}

export function getUser(id: string) {
  return fetchJson<User>(`/api/users/${id}`);
}

export function updateUser(id: string, data: { display_name?: string; timezone?: string }) {
  return fetchJson<User>(`/api/users/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export async function getPulsoidToken(
  userId: string
): Promise<PulsoidTokenStatus | null> {
  const res = await fetch(`/api/users/${userId}/pulsoid-token`);
  if (res.status === 404) return null;
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new Error(body.error || `HTTP ${res.status}`);
  }
  return res.json();
}

export function createPulsoidConnect(returnTo?: string) {
  return fetchJson<{ request_id: string }>("/api/oauth/pulsoid/connect", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ return_to: returnTo ?? "/settings" }),
  });
}

export function setManualPulsoidToken(userId: string, accessToken: string) {
  return fetchJson<void>(`/api/users/${userId}/pulsoid-token`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ access_token: accessToken }),
  });
}

export function deletePulsoidToken(userId: string) {
  return fetchJson<void>(`/api/users/${userId}/pulsoid-token`, {
    method: "DELETE",
  });
}

export interface DailyStats {
  day: string;
  avg_bpm: number;
  min_bpm: number;
  max_bpm: number;
  count: number;
}

export function getDailyStats(userId: string, date: string) {
  return fetchJson<DailyStats | null>(
    `/api/users/${userId}/heart-rates/daily-stats?date=${date}`
  );
}

export interface MinuteStats {
  timestamp: number;
  avg_bpm: number;
  min_bpm: number;
  max_bpm: number;
  sample_count: number;
}

export function getMinuteStats(userId: string, period: string) {
  return fetchJson<MinuteStats[]>(
    `/api/users/${userId}/heart-rates/minute-stats?period=${period}`
  );
}

export function getMinuteStatsByDate(userId: string, date: string) {
  return fetchJson<MinuteStats[]>(
    `/api/users/${userId}/heart-rates/minute-stats/by-date?date=${date}`
  );
}

export function getHeartRates(userId: string, period: string) {
  return fetchJson<HeartRateRecord[]>(
    `/api/users/${userId}/heart-rates?period=${period}`
  );
}

export function getHeartRatesByDate(userId: string, date: string) {
  return fetchJson<HeartRateRecord[]>(
    `/api/users/${userId}/heart-rates/by-date?date=${date}`
  );
}

export function getLatestHeartRate(userId: string) {
  return fetchJson<HeartRateRecord>(`/api/users/${userId}/latest-heart-rate`);
}
