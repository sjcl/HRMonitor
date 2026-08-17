import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  ApiError,
  TransientError,
  __resetRefreshStateForTests,
  ensureFreshToken,
  fetchJson,
  refreshSession,
} from "./http";

/** Minimal `Response` stand-in — only what the client actually reads. */
function res(
  status: number,
  body: unknown = {},
  ok = status >= 200 && status < 300,
): Response {
  return {
    ok,
    status,
    json: async () => body,
  } as Response;
}

let assign: ReturnType<typeof vi.fn>;

beforeEach(() => {
  __resetRefreshStateForTests();
  assign = vi.fn();
  Object.defineProperty(window, "location", {
    configurable: true,
    value: {
      assign,
      pathname: "/me",
      search: "",
      href: "http://localhost:3000/me",
    },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("fetchJson", () => {
  it("returns the parsed body on success", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(200, { id: "u1" })));
    await expect(fetchJson<{ id: string }>("/api/users/me")).resolves.toEqual({
      id: "u1",
    });
  });

  it("returns undefined for 204", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(204)));
    await expect(fetchJson("/api/groups/g1", { method: "DELETE" })).resolves.toBeUndefined();
  });

  it("refreshes once on 401 and retries the original request", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(res(401))
      .mockResolvedValueOnce(res(204)) // /api/auth/refresh
      .mockResolvedValueOnce(res(200, { id: "u1" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchJson("/api/users/me")).resolves.toEqual({ id: "u1" });

    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(fetchMock.mock.calls[1][0]).toBe("/api/auth/refresh");
    expect(fetchMock.mock.calls[1][1]).toMatchObject({ method: "POST" });
    expect(assign).not.toHaveBeenCalled();
  });

  it("redirects to login when the refresh itself is rejected", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(res(401))
      .mockResolvedValueOnce(res(401)); // refresh says the session is gone
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchJson("/api/users/me")).rejects.toBeInstanceOf(ApiError);
    expect(assign).toHaveBeenCalledWith(
      "/login?return_to=%2Fme",
    );
  });

  it("does NOT log the user out when the session store is unavailable", async () => {
    // The whole point of separating 401 from 5xx: a Redis restart must not
    // turn into a mass logout.
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(res(401))
      .mockResolvedValueOnce(res(503, {}, false));
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchJson("/api/users/me")).rejects.toBeInstanceOf(TransientError);
    expect(assign).not.toHaveBeenCalled();
  });

  it("treats a network failure during refresh as transient", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(res(401))
      .mockRejectedValueOnce(new TypeError("Failed to fetch"));
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchJson("/api/users/me")).rejects.toBeInstanceOf(TransientError);
    expect(assign).not.toHaveBeenCalled();
  });

  it("retries at most once, then gives up", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(res(401))
      .mockResolvedValueOnce(res(204)) // refresh succeeds
      .mockResolvedValueOnce(res(401)); // still 401 afterwards
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchJson("/api/users/me")).rejects.toBeInstanceOf(ApiError);
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(assign).toHaveBeenCalled();
  });

  it("surfaces non-401 errors without attempting a refresh", async () => {
    const fetchMock = vi.fn().mockResolvedValue(res(403, { error: "Not allowed" }, false));
    vi.stubGlobal("fetch", fetchMock);

    await expect(fetchJson("/api/users/other")).rejects.toMatchObject({
      status: 403,
      message: "Not allowed",
    });
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });
});

describe("refreshSession single-flight", () => {
  it("issues one refresh request for concurrent callers", async () => {
    // Without single-flight each 401 would rotate the refresh token again,
    // relying on the server's grace window for no reason.
    let resolveRefresh: (r: Response) => void = () => {};
    const pending = new Promise<Response>((r) => {
      resolveRefresh = r;
    });
    const fetchMock = vi.fn().mockReturnValue(pending);
    vi.stubGlobal("fetch", fetchMock);

    const all = Promise.all([refreshSession(), refreshSession(), refreshSession()]);
    resolveRefresh(res(204));
    const results = await all;

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(results).toEqual([
      { status: "refreshed" },
      { status: "refreshed" },
      { status: "refreshed" },
    ]);
  });

  it("collapses concurrent 401s from separate requests into one refresh", async () => {
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url === "/api/auth/refresh") return Promise.resolve(res(204));
      return Promise.resolve(res(200, { ok: true }));
    });
    // First call for each endpoint 401s, subsequent ones succeed.
    let first = 3;
    fetchMock.mockImplementation((url: string) => {
      if (url === "/api/auth/refresh") return Promise.resolve(res(204));
      if (first-- > 0) return Promise.resolve(res(401));
      return Promise.resolve(res(200, { ok: true }));
    });
    vi.stubGlobal("fetch", fetchMock);

    await Promise.all([fetchJson("/a"), fetchJson("/b"), fetchJson("/c")]);

    const refreshCalls = fetchMock.mock.calls.filter(
      (c) => c[0] === "/api/auth/refresh",
    );
    expect(refreshCalls).toHaveLength(1);
  });

  it("allows a new refresh after the previous one settles", async () => {
    const fetchMock = vi.fn().mockResolvedValue(res(204));
    vi.stubGlobal("fetch", fetchMock);

    await refreshSession();
    await refreshSession();

    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("reports 401 as unauthenticated and 5xx as unavailable", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(401)));
    await expect(refreshSession()).resolves.toEqual({ status: "unauthenticated" });

    __resetRefreshStateForTests();
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(500, {}, false)));
    await expect(refreshSession()).resolves.toEqual({ status: "unavailable" });
  });
});

describe("ensureFreshToken", () => {
  it("skips the refresh when the token has plenty of life left", async () => {
    const expires = Math.floor(Date.now() / 1000) + 1800;
    const fetchMock = vi
      .fn()
      .mockResolvedValue(res(200, { authenticated: true, expires_at: expires }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(ensureFreshToken()).resolves.toEqual({ status: "refreshed" });
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/auth/session");
  });

  it("refreshes when the token is close to expiry", async () => {
    // Guards the Pulsoid hand-off: a token with 2 minutes left is not enough
    // to survive a round trip through an external consent screen.
    const expires = Math.floor(Date.now() / 1000) + 120;
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url === "/api/auth/session") {
        return Promise.resolve(res(200, { authenticated: true, expires_at: expires }));
      }
      return Promise.resolve(res(204));
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(ensureFreshToken(300)).resolves.toEqual({ status: "refreshed" });
    expect(
      fetchMock.mock.calls.some((c) => c[0] === "/api/auth/refresh"),
    ).toBe(true);
  });

  it("refreshes when the session endpoint says unauthenticated", async () => {
    const fetchMock = vi.fn().mockImplementation((url: string) => {
      if (url === "/api/auth/session") return Promise.resolve(res(401));
      return Promise.resolve(res(204));
    });
    vi.stubGlobal("fetch", fetchMock);

    await expect(ensureFreshToken()).resolves.toEqual({ status: "refreshed" });
  });

  it("reports unavailable when the session endpoint cannot be reached", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    await expect(ensureFreshToken()).resolves.toEqual({ status: "unavailable" });
  });
});
