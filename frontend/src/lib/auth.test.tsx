import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router";
import type { ReactNode } from "react";
import { __resetRefreshStateForTests } from "./http";
import { logout, LogoutUnavailableError } from "./auth";
import { ProtectedRoute } from "../App";

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

const ALICE = {
  id: "u1",
  display_name: "Alice",
  avatar_url: null,
  timezone: "UTC",
  heart_rate_visibility: "group_default",
};

/** The real gate, wrapped around a page that only signed-in users may see. */
function protectedRoutes() {
  return (
    <Routes>
      <Route element={<ProtectedRoute />}>
        <Route path="/me" element={<p>secret content</p>} />
      </Route>
    </Routes>
  );
}

/** Every response an unauthenticated caller gets: 401, refresh included. */
function loggedOutFetch() {
  return vi.fn().mockResolvedValue(res(401));
}

/** 401 on the API, 503 on refresh: the session store is down, not the session. */
function outageFetch() {
  return vi
    .fn()
    .mockImplementation((url: string) =>
      Promise.resolve(url === "/api/auth/refresh" ? res(503, {}, false) : res(401)),
    );
}

describe("route protection", () => {
  it("renders the page for an authenticated user", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(200, ALICE)));

    render(wrapper(protectedRoutes()));

    expect(await screen.findByText("secret content")).toBeInTheDocument();
    expect(screen.getByText("Alice")).toBeInTheDocument();
  });

  it("sends a genuinely logged-out user to the login page", async () => {
    vi.stubGlobal("fetch", loggedOutFetch());

    render(wrapper(protectedRoutes()));

    await waitFor(() =>
      expect(assign).toHaveBeenCalledWith("/login?return_to=%2Fme"),
    );
    expect(screen.queryByText("secret content")).not.toBeInTheDocument();
  });

  it("shows a retry state rather than logging out during an outage", async () => {
    // The important negative: a 503 must not render as "logged out".
    vi.stubGlobal("fetch", outageFetch());

    render(wrapper(protectedRoutes()));

    expect(await screen.findByText(/接続できません/)).toBeInTheDocument();
    expect(screen.queryByText("secret content")).not.toBeInTheDocument();
    expect(assign).not.toHaveBeenCalled();
  });
});

describe("recovery from a transient outage", () => {
  // Testing Library's async helpers wait on real time, which never passes once
  // the clock is faked — so these tests advance the clock by hand and then
  // assert synchronously.
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  async function tick(ms: number) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ms);
    });
  }

  it("polls itself back to life once the backend returns", async () => {
    // The regression: the retries run out, and without a poll the screen sits
    // on "retrying..." forever while the backend is already healthy again.
    const fetchMock = outageFetch();
    vi.stubGlobal("fetch", fetchMock);

    render(wrapper(protectedRoutes()));

    // Long enough for the initial attempt and all three retries to settle.
    await tick(1_000);
    expect(screen.getByText(/接続できません/)).toBeInTheDocument();

    fetchMock.mockResolvedValue(res(200, ALICE));
    await tick(30_000);

    expect(screen.getByText("secret content")).toBeInTheDocument();
    expect(assign).not.toHaveBeenCalled();
  });

  it("does not poll while healthy", async () => {
    const fetchMock = vi.fn().mockResolvedValue(res(200, ALICE));
    vi.stubGlobal("fetch", fetchMock);

    render(wrapper(protectedRoutes()));
    await tick(1_000);
    expect(screen.getByText("secret content")).toBeInTheDocument();

    const settled = fetchMock.mock.calls.length;
    await tick(120_000);

    expect(fetchMock.mock.calls.length).toBe(settled);
  });
});

describe("manual retry", () => {
  it("refetches when the user presses the retry button", async () => {
    const fetchMock = outageFetch();
    vi.stubGlobal("fetch", fetchMock);

    render(wrapper(protectedRoutes()));
    const button = await screen.findByRole("button", { name: "再試行" });

    fetchMock.mockResolvedValue(res(200, ALICE));
    fireEvent.click(button);

    expect(await screen.findByText("secret content")).toBeInTheDocument();
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
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(200, ALICE)));

    render(wrapper(protectedRoutes()));
    await waitFor(() => expect(screen.getByText("secret content")).toBeInTheDocument());

    expect(window.localStorage.length).toBe(0);
    expect(window.sessionStorage.length).toBe(0);
  });
});
