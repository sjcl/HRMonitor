import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router";
import type { ReactNode } from "react";
import { __resetRefreshStateForTests } from "./http";
import { logout, LogoutUnavailableError, useCurrentUser } from "./auth";

function res(status: number, body: unknown = {}, ok = status >= 200 && status < 300) {
  return { ok, status, json: async () => body } as Response;
}

let assign: ReturnType<typeof vi.fn>;

beforeEach(() => {
  __resetRefreshStateForTests();
  assign = vi.fn();
  Object.defineProperty(window, "location", {
    configurable: true,
    value: { assign, pathname: "/me", search: "", href: "http://localhost/me" },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function wrapper(children: ReactNode) {
  // `useCurrentUser` sets its own `retry` predicate, which overrides any
  // default here — so the retries are real. Only the backoff is removed, to
  // keep the transient-failure case from waiting seconds.
  const client = new QueryClient({
    defaultOptions: { queries: { retryDelay: 0 } },
  });
  return (
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={["/me"]}>{children}</MemoryRouter>
    </QueryClientProvider>
  );
}

/** Stand-in for the real ProtectedRoute, with the same decision logic. */
function Protected() {
  const { data: user, isPending, isError } = useCurrentUser();
  if (isPending) return <p>読み込み中...</p>;
  if (isError) return <p>サーバーに接続できません</p>;
  if (user === null) return <p data-testid="redirected">redirecting</p>;
  return <p>secret content for {user.display_name}</p>;
}

describe("route protection", () => {
  it("renders the page for an authenticated user", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        res(200, { id: "u1", display_name: "Alice", avatar_url: null, timezone: "UTC", heart_rate_visibility: "group_default" }),
      ),
    );

    render(
      wrapper(
        <Routes>
          <Route path="/me" element={<Protected />} />
        </Routes>,
      ),
    );

    expect(await screen.findByText(/secret content for Alice/)).toBeInTheDocument();
  });

  it("resolves to logged-out when the session cannot be refreshed", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(401)));

    render(
      wrapper(
        <Routes>
          <Route path="/me" element={<Protected />} />
        </Routes>,
      ),
    );

    expect(await screen.findByTestId("redirected")).toBeInTheDocument();
  });

  it("shows a retry state rather than logging out during an outage", async () => {
    // The important negative: a 503 must not render as "logged out".
    vi.stubGlobal(
      "fetch",
      vi.fn().mockImplementation((url: string) =>
        Promise.resolve(url === "/api/auth/refresh" ? res(503, {}, false) : res(401)),
      ),
    );

    render(
      wrapper(
        <Routes>
          <Route path="/me" element={<Protected />} />
        </Routes>,
      ),
    );

    expect(await screen.findByText(/接続できません/)).toBeInTheDocument();
    expect(screen.queryByTestId("redirected")).not.toBeInTheDocument();
    expect(assign).not.toHaveBeenCalled();
  });
});

describe("logout", () => {
  it("resolves when the session is revoked", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(204)));
    await expect(logout()).resolves.toBeUndefined();
  });

  it("treats 401 as already logged out", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(401)));
    await expect(logout()).resolves.toBeUndefined();
  });

  it("fails loudly when the server could not revoke the session", async () => {
    // A 503 means the refresh token is still live. Reporting success would
    // leave the user believing they had logged out when they had not.
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(503, {}, false)));
    await expect(logout()).rejects.toBeInstanceOf(LogoutUnavailableError);
  });

  it("fails loudly on a network error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("offline")));
    await expect(logout()).rejects.toBeInstanceOf(LogoutUnavailableError);
  });

  it("can be retried after a failure", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(res(503, {}, false))
      .mockResolvedValueOnce(res(204));
    vi.stubGlobal("fetch", fetchMock);

    await expect(logout()).rejects.toBeInstanceOf(LogoutUnavailableError);
    await expect(logout()).resolves.toBeUndefined();
  });
});

describe("no browser-side token storage", () => {
  it("keeps localStorage and sessionStorage empty", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        res(200, { id: "u1", display_name: "Alice", avatar_url: null, timezone: "UTC", heart_rate_visibility: "group_default" }),
      ),
    );

    render(
      wrapper(
        <Routes>
          <Route path="/me" element={<Protected />} />
        </Routes>,
      ),
    );
    await waitFor(() => expect(screen.getByText(/secret content/)).toBeInTheDocument());

    expect(window.localStorage.length).toBe(0);
    expect(window.sessionStorage.length).toBe(0);
  });
});
