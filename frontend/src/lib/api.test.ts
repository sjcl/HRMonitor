import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createPulsoidConnect } from "./api";
import { ApiError, TransientError, __resetRefreshStateForTests } from "./http";

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

function session(remainingSecs: number): Response {
  return res(200, {
    authenticated: true,
    expires_at: Math.floor(Date.now() / 1000) + remainingSecs,
  });
}

let assign: ReturnType<typeof vi.fn>;

beforeEach(() => {
  __resetRefreshStateForTests();
  assign = vi.fn();
  Object.defineProperty(window, "location", {
    configurable: true,
    value: {
      assign,
      pathname: "/settings",
      search: "",
      href: "http://localhost:3000/settings",
    },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

/** URLs of every fetch made, in order. */
function calls(fetchMock: ReturnType<typeof vi.fn>): string[] {
  return fetchMock.mock.calls.map((c) => c[0] as string);
}

describe("createPulsoidConnect", () => {
  it("skips the refresh when the token comfortably outlives the ticket", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(session(20 * 60))
      .mockResolvedValueOnce(res(200, { request_id: "r1" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(createPulsoidConnect("/settings")).resolves.toEqual({
      request_id: "r1",
    });
    expect(calls(fetchMock)).toEqual([
      "/api/auth/session",
      "/api/oauth/pulsoid/connect",
    ]);
  });

  it("refreshes first when the token would expire during the hand-off", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(session(2 * 60))
      .mockResolvedValueOnce(res(200))
      .mockResolvedValueOnce(res(200, { request_id: "r2" }));
    vi.stubGlobal("fetch", fetchMock);

    await expect(createPulsoidConnect("/settings")).resolves.toEqual({
      request_id: "r2",
    });
    expect(calls(fetchMock)).toEqual([
      "/api/auth/session",
      "/api/auth/refresh",
      "/api/oauth/pulsoid/connect",
    ]);
  });

  it("sends the user to login without minting a ticket when the session is gone", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(session(2 * 60))
      .mockResolvedValueOnce(res(401));
    vi.stubGlobal("fetch", fetchMock);

    await expect(createPulsoidConnect("/settings")).rejects.toBeInstanceOf(
      ApiError,
    );
    expect(calls(fetchMock)).toEqual([
      "/api/auth/session",
      "/api/auth/refresh",
    ]);
    expect(assign).toHaveBeenCalledWith(
      "/login?return_to=%2Fsettings",
    );
  });

  it("stays put on a transient refresh failure", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(session(2 * 60))
      .mockResolvedValueOnce(res(503));
    vi.stubGlobal("fetch", fetchMock);

    await expect(createPulsoidConnect("/settings")).rejects.toBeInstanceOf(
      TransientError,
    );
    expect(calls(fetchMock)).toEqual([
      "/api/auth/session",
      "/api/auth/refresh",
    ]);
    expect(assign).not.toHaveBeenCalled();
  });
});
