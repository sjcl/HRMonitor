// --- Types ---

export interface UserListItem {
  id: string;
  name: string;
  latest_bpm: number | null;
  has_pulsoid_token: boolean;
  created_at: number;
}

export interface User {
  id: string;
  name: string;
  timezone: string;
  created_at: number;
  updated_at: number;
}

export interface PulsoidTokenStatus {
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

export function createUser(name: string, timezone: string) {
  return fetchJson<User>("/api/users", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, timezone }),
  });
}

export function updateUser(id: string, data: { name?: string; timezone?: string }) {
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

export function setPulsoidToken(userId: string, accessToken: string) {
  return fetchJson<PulsoidTokenStatus>(
    `/api/users/${userId}/pulsoid-token`,
    {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ access_token: accessToken }),
    }
  );
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

export function getDailyStats(userId: string, from: string, to: string) {
  return fetchJson<DailyStats[]>(
    `/api/users/${userId}/heart-rates/daily-stats?from=${from}&to=${to}`
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
