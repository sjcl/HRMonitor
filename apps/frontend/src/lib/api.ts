// --- Types ---

export interface UserListItem {
  id: string;
  name: string;
  latest_bpm: number | null;
  token_count: number;
  created_at: number;
}

export interface User {
  id: string;
  name: string;
  created_at: number;
  updated_at: number;
}

export interface Token {
  id: string;
  user_id: string;
  label: string | null;
  is_active: boolean;
  last_connected_at: number | null;
  last_error: string | null;
  created_at: number;
  updated_at: number;
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

export function createUser(name: string) {
  return fetchJson<User>("/api/users", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
}

export function updateUser(id: string, name: string) {
  return fetchJson<User>(`/api/users/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name }),
  });
}

export function getTokens(userId: string) {
  return fetchJson<Token[]>(`/api/users/${userId}/pulsoid-tokens`);
}

export function createToken(
  userId: string,
  data: { label?: string; access_token: string }
) {
  return fetchJson<Token>(`/api/users/${userId}/pulsoid-tokens`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export function updateToken(
  tokenId: string,
  data: { label?: string; is_active?: boolean }
) {
  return fetchJson<Token>(`/api/pulsoid-tokens/${tokenId}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export function deleteToken(tokenId: string) {
  return fetchJson<void>(`/api/pulsoid-tokens/${tokenId}`, {
    method: "DELETE",
  });
}

export interface DailyStats {
  day: number;
  avg_bpm: number;
  min_bpm: number;
  max_bpm: number;
  count: number;
}

export function getDailyStats(userId: string, from: number, to: number) {
  return fetchJson<DailyStats[]>(
    `/api/users/${userId}/heart-rates/daily-stats?from=${from}&to=${to}`
  );
}

export function getHeartRates(
  userId: string,
  params?: { from?: number; to?: number; limit?: number }
) {
  const query = new URLSearchParams();
  if (params?.from) query.set("from", String(params.from));
  if (params?.to) query.set("to", String(params.to));
  if (params?.limit) query.set("limit", String(params.limit));
  const qs = query.toString();
  return fetchJson<HeartRateRecord[]>(
    `/api/users/${userId}/heart-rates${qs ? `?${qs}` : ""}`
  );
}

export function getLatestHeartRate(userId: string) {
  return fetchJson<HeartRateRecord>(`/api/users/${userId}/latest-heart-rate`);
}
