/**
 * Client-side view of authentication.
 *
 * There is no session object in the browser and no token in storage: the
 * access and refresh tokens are HttpOnly cookies that JavaScript cannot read.
 * "Am I logged in?" is therefore answered by asking the API — which is also
 * the only answer worth trusting, since the server enforces authorisation
 * regardless of what the UI believes.
 */

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ApiError, TransientError, redirectToLogin } from "./http";
import { getSelfUser, type SelfUser } from "./api";

export const CURRENT_USER_KEY = ["current-user"] as const;

/**
 * Begin Discord login.
 *
 * A full navigation rather than a fetch: the endpoint answers with a redirect
 * to Discord, and the browser has to follow it. `tz` seeds the new user's
 * timezone — it replaces the short-lived `browser_tz` cookie the Auth.js
 * adapter used to read, and is validated server-side before it reaches the
 * database.
 */
export function startDiscordLogin(returnTo?: string): void {
  const params = new URLSearchParams();
  if (returnTo) params.set("return_to", returnTo);
  try {
    const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
    if (tz) params.set("tz", tz);
  } catch {
    // Timezone detection is a nicety; the server defaults to UTC.
  }
  const query = params.toString();
  window.location.assign(`/api/auth/login/discord${query ? `?${query}` : ""}`);
}

/** Thrown when logout could not revoke the session and should be retried. */
export class LogoutUnavailableError extends Error {
  constructor() {
    super("ログアウトできませんでした。時間をおいて再試行してください。");
    this.name = "LogoutUnavailableError";
  }
}

/**
 * End the session.
 *
 * A 503 means the server could not actually revoke the refresh session, so the
 * user is *not* logged out however the UI looks. Surfacing that as an error —
 * rather than clearing local state and navigating away — is the difference
 * between a failed logout the user can retry and one they never learn about
 * while their refresh token stays live for another 30 days.
 */
export async function logout(): Promise<void> {
  let res: Response;
  try {
    res = await fetch("/api/auth/logout", {
      method: "POST",
      headers: { "X-Requested-With": "hrmonitor" },
    });
  } catch {
    throw new LogoutUnavailableError();
  }
  // 401 means there was nothing to revoke, which is a successful logout.
  if (!res.ok && res.status !== 401) throw new LogoutUnavailableError();
}

/** The signed-in user, or `null` when not authenticated. */
export function useCurrentUser() {
  return useQuery<SelfUser | null>({
    queryKey: CURRENT_USER_KEY,
    queryFn: async () => {
      try {
        return await getSelfUser();
      } catch (e) {
        // `fetchJson` has already tried to refresh; a 401 here is final.
        if (e instanceof ApiError && e.status === 401) return null;
        throw e;
      }
    },
    // A transient backend problem should retry rather than resolve to
    // "logged out", which would bounce the user to /login.
    retry: (count, error) => error instanceof TransientError && count < 3,
    staleTime: 60_000,
    // Those three retries take about seven seconds; an outage lasts longer.
    // Once the query has settled into an error the UI would otherwise sit on
    // "retrying..." forever, so keep knocking — but only while it is broken,
    // which leaves the healthy path at zero extra requests.
    refetchInterval: (query) => (query.state.status === "error" ? 30_000 : false),
    // Same reasoning: coming back to the tab is a good moment to try again.
    refetchOnWindowFocus: (query) => query.state.status === "error",
  });
}

export function useLogout() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: logout,
    onSuccess: () => {
      queryClient.clear();
      redirectToLogin();
    },
  });
}
