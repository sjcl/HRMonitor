/**
 * The single HTTP client for the whole app.
 *
 * Every call to the Rust API goes through {@link fetchJson}, which is what
 * makes the access-token refresh transparent: there is exactly one place that
 * knows what a 401 means, and exactly one place that can trigger a refresh.
 *
 * The distinction that matters most here is between *authentication failure*
 * and *infrastructure failure*. A 401 from `/api/auth/refresh` means the
 * session is genuinely gone and the user must log in again. A 5xx or a network
 * error means Redis is down or restarting — the session is probably fine, and
 * bouncing everyone to the login page would turn a brief outage into a mass
 * logout. Those two cases are handled differently throughout this file.
 */

export class ApiError extends Error {
  constructor(
    public status: number,
    message: string,
  ) {
    super(message);
    this.name = "ApiError";
  }
}

/** Raised when the session store is unreachable — retryable, not a logout. */
export class TransientError extends Error {
  constructor(message = "サーバーに一時的に接続できません") {
    super(message);
    this.name = "TransientError";
  }
}

export type RefreshResult =
  | { status: "refreshed" }
  /** The session is gone. The caller should send the user to /login. */
  | { status: "unauthenticated" }
  /** Session store unavailable. Retry later; do not log out. */
  | { status: "unavailable" };

/**
 * In-flight refresh, shared by every caller (single-flight).
 *
 * Without this, a page that fires several queries at once would answer a burst
 * of 401s with a burst of `POST /api/auth/refresh` calls. Because refresh
 * *rotates* the token, the second and later calls would present an already
 * superseded token — surviving only because of the server's grace window, and
 * needlessly so. One request per burst is both correct and cheaper.
 */
let inflight: Promise<RefreshResult> | null = null;

async function doRefresh(): Promise<RefreshResult> {
  let res: Response;
  try {
    res = await fetch("/api/auth/refresh", {
      method: "POST",
      headers: { "X-Requested-With": "hrmonitor" },
    });
  } catch {
    // Network-level failure: offline, DNS, connection reset.
    return { status: "unavailable" };
  }
  if (res.ok) return { status: "refreshed" };
  if (res.status === 401) return { status: "unauthenticated" };
  // 403 (origin), 5xx, anything else: not a proven auth failure.
  return { status: "unavailable" };
}

export function refreshSession(): Promise<RefreshResult> {
  inflight ??= doRefresh().finally(() => {
    inflight = null;
  });
  return inflight;
}

let redirecting = false;

/** Test hook: clears the module-level refresh and redirect guards. */
export function __resetRefreshStateForTests(): void {
  inflight = null;
  redirecting = false;
}

/**
 * Send the user to the login page, preserving where they were.
 *
 * Guarded so that several simultaneous failures cannot each trigger a
 * navigation.
 */
export function redirectToLogin(): void {
  if (redirecting) return;
  redirecting = true;
  const here = window.location.pathname + window.location.search;
  const target =
    here === "/login" ? "/login" : `/login?return_to=${encodeURIComponent(here)}`;
  window.location.assign(target);
}

async function parseError(res: Response): Promise<ApiError> {
  const body = await res.json().catch(() => ({}) as { error?: string });
  return new ApiError(res.status, body.error || `HTTP ${res.status}`);
}

/**
 * Fetch JSON from the API, refreshing the access token once if it has expired.
 *
 * Retries at most once. A second 401 after a successful refresh means
 * something other than expiry is wrong, and looping would just hammer the
 * server.
 */
export async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  let res = await fetch(url, init);

  if (res.status === 401) {
    const result = await refreshSession();
    if (result.status === "unauthenticated") {
      redirectToLogin();
      throw new ApiError(401, "Unauthorized");
    }
    if (result.status === "unavailable") {
      // Stay put: this is very likely a transient backend problem, and the
      // user's session is probably still valid.
      throw new TransientError();
    }
    res = await fetch(url, init);
    if (res.status === 401) {
      redirectToLogin();
      throw new ApiError(401, "Unauthorized");
    }
  }

  if (!res.ok) throw await parseError(res);
  if (res.status === 204) return undefined as T;
  return res.json() as Promise<T>;
}

/** JSON body helper, so no call site hand-writes the content type. */
export function jsonBody(method: string, data: unknown): RequestInit {
  return {
    method,
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  };
}

/**
 * Whether the access token is valid, and until when.
 *
 * Local verification on the server: no database or Redis access. Used before
 * opening a WebSocket, because a browser cannot read the status or body of a
 * failed upgrade — the socket just closes, indistinguishably from any other
 * network problem.
 */
export async function getSessionInfo(): Promise<{
  authenticated: boolean;
  expires_at: number;
} | null> {
  let res: Response;
  try {
    res = await fetch("/api/auth/session", { cache: "no-store" });
  } catch {
    return null;
  }
  if (res.status === 401) return { authenticated: false, expires_at: 0 };
  if (!res.ok) return null;
  return res.json();
}

/**
 * Guarantee a usable access token, refreshing if it is missing or close to
 * expiry.
 *
 * `minRemainingSecs` exists for flows that leave the site and come back — the
 * Pulsoid OAuth hand-off in particular. A token with two minutes left is fine
 * for a normal request but not for a round trip through another provider's
 * consent screen.
 */
export async function ensureFreshToken(
  minRemainingSecs = 0,
): Promise<RefreshResult> {
  const info = await getSessionInfo();
  if (info === null) return { status: "unavailable" };

  if (info.authenticated) {
    const remaining = info.expires_at - Math.floor(Date.now() / 1000);
    if (remaining > minRemainingSecs) return { status: "refreshed" };
  }
  return refreshSession();
}
