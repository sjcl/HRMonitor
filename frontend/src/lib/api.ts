import {
  ApiError,
  TransientError,
  ensureFreshToken,
  fetchJson,
  jsonBody,
  redirectToLogin,
} from "./http";

// Re-exported: components catch ApiError to render 403/404 states.
export { ApiError, TransientError } from "./http";

// --- Types ---

export type HeartRateVisibility = "group_default" | "private";

export interface SelfUser {
  id: string;
  display_name: string;
  avatar_url: string | null;
  timezone: string;
  heart_rate_visibility: HeartRateVisibility;
}

export interface HeartRateProfile {
  id: string;
  display_name: string;
  avatar_url: string | null;
  timezone: string;
}

export interface PulsoidTokenStatus {
  source: "oauth" | "manual";
  connection_state: "pending" | "connected" | "error";
  state_updated_at: number;
  last_connected_at: number | null;
  last_error: string | null;
}

export interface TokenMutationResult {
  status: "syncing";
}

export interface HeartRateRecord {
  bpm: number;
  timestamp: number;
}

// --- API functions ---

/**
 * Also the "am I signed in?" probe, so a 401 must come back as an error for
 * the caller to interpret — never as a navigation. `/login` renders this very
 * query, and redirecting from here would reload that page forever.
 */
export function getSelfUser() {
  return fetchJson<SelfUser>(`/api/users/me`, undefined, {
    redirectOn401: false,
  });
}

export function getHeartRateProfile(userId: string) {
  return fetchJson<HeartRateProfile>(`/api/users/${userId}/heart-rate-profile`);
}

export function updateUser(data: {
  display_name?: string;
  timezone?: string;
  heart_rate_visibility?: HeartRateVisibility;
}) {
  return fetchJson<SelfUser>(`/api/users/me`, jsonBody("PATCH", data));
}

/**
 * `null` when the user has no Pulsoid connection.
 *
 * Previously a bare `fetch`, which meant this one call skipped the 401/refresh
 * path entirely; it now goes through the shared client like everything else.
 */
export async function getPulsoidToken(): Promise<PulsoidTokenStatus | null> {
  try {
    return await fetchJson<PulsoidTokenStatus>(`/api/users/me/pulsoid-token`);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
}

/**
 * The Pulsoid connect ticket's lifetime (`connect_requests.expires_at`, set to
 * `now() + INTERVAL '5 minutes'` in the backend) plus 30 seconds of clock skew.
 */
const OAUTH_HANDOFF_MIN_TOKEN_SECS = 5 * 60 + 30;

/**
 * Mint the ticket that hands the browser off to Pulsoid's consent screen.
 *
 * The redirect and the callback the user comes back to are both behind
 * `require_auth`, and while the user is away the SPA is not running — so the
 * usual 401-then-refresh recovery in {@link fetchJson} cannot happen. Starting
 * the trip with a nearly expired access token therefore ends in a bare 401 on
 * return. Guarantee the token outlives the ticket before leaving.
 */
export async function createPulsoidConnect(returnTo?: string) {
  const fresh = await ensureFreshToken(OAUTH_HANDOFF_MIN_TOKEN_SECS);
  if (fresh.status === "unauthenticated") {
    redirectToLogin();
    throw new ApiError(401, "Unauthorized");
  }
  if (fresh.status === "unavailable") throw new TransientError();

  return fetchJson<{ request_id: string }>(
    "/api/oauth/pulsoid/connect",
    jsonBody("POST", { return_to: returnTo ?? "/settings" }),
  );
}

export function setManualPulsoidToken(accessToken: string) {
  return fetchJson<TokenMutationResult>(
    `/api/users/me/pulsoid-token`,
    jsonBody("PUT", { access_token: accessToken }),
  );
}

export function deletePulsoidToken() {
  return fetchJson<TokenMutationResult>(`/api/users/me/pulsoid-token`, {
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

