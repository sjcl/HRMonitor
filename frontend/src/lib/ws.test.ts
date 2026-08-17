import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { CLOSE_TOKEN_EXPIRED } from "./ws";
import { __resetRefreshStateForTests } from "./http";

/**
 * The reconnect behaviour these tests describe lives inside `useWsConnection`,
 * which is only reachable through React. Rather than mount a component just to
 * assert on socket plumbing, they pin the contract that the hook and the
 * gateway share: the close code, and the pre-connect freshness check.
 */

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  onopen: (() => void) | null = null;
  onclose: ((e: { code: number }) => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  closed = false;

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  close() {
    this.closed = true;
  }
}

beforeEach(() => {
  __resetRefreshStateForTests();
  FakeWebSocket.instances = [];
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("WebSocket auth contract", () => {
  it("uses the same 4401 close code the gateway sends", () => {
    // ws-gateway emits this on token expiry; if the two ever disagree the
    // client would back off instead of refreshing, and the socket would stay
    // dead until the next manual reload.
    expect(CLOSE_TOKEN_EXPIRED).toBe(4401);
  });

  it("is distinct from the 1001 shutdown close", () => {
    expect(CLOSE_TOKEN_EXPIRED).not.toBe(1001);
  });

  it("sits in the private 4000-4999 range", () => {
    expect(CLOSE_TOKEN_EXPIRED).toBeGreaterThanOrEqual(4000);
    expect(CLOSE_TOKEN_EXPIRED).toBeLessThanOrEqual(4999);
  });
});

describe("pre-connect freshness check", () => {
  it("refreshes before connecting when the token has expired", async () => {
    // A rejected WebSocket upgrade gives the browser no status and no body, so
    // an expired token has to be detected over HTTP first.
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((url: string) => {
        calls.push(url);
        if (url === "/api/auth/session") {
          return Promise.resolve({ ok: false, status: 401, json: async () => ({}) });
        }
        return Promise.resolve({ ok: true, status: 204, json: async () => ({}) });
      }),
    );

    const { ensureFreshToken } = await import("./http");
    await expect(ensureFreshToken()).resolves.toEqual({ status: "refreshed" });

    expect(calls).toEqual(["/api/auth/session", "/api/auth/refresh"]);
  });

  it("does not refresh when the token is still good", async () => {
    const expires = Math.floor(Date.now() / 1000) + 900;
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ authenticated: true, expires_at: expires }),
    });
    vi.stubGlobal("fetch", fetchMock);

    const { ensureFreshToken } = await import("./http");
    await ensureFreshToken();

    expect(fetchMock.mock.calls.map((c) => c[0])).toEqual(["/api/auth/session"]);
  });
});
