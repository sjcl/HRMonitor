import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { MemoryRouter, Route, Routes } from "react-router";
import { __resetRefreshStateForTests } from "../lib/http";
import { LoginPage } from "./login";

function res(status: number, body: unknown = {}, ok = status >= 200 && status < 300) {
  return { ok, status, json: async () => body } as Response;
}

let assign: ReturnType<typeof vi.fn>;

beforeEach(() => {
  __resetRefreshStateForTests();
  assign = vi.fn();
  Object.defineProperty(window, "location", {
    configurable: true,
    value: {
      assign,
      pathname: "/login",
      search: "",
      href: "http://localhost/login",
    },
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function renderAt(entry: string) {
  const client = new QueryClient({
    defaultOptions: { queries: { retryDelay: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <Routes>
          <Route path="/login" element={<LoginPage />} />
          <Route path="/me" element={<p>me page</p>} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

describe("LoginPage", () => {
  it("shows the sign-in button to a logged-out visitor without navigating", async () => {
    // The page's own session query 401s by design. If that turned into a
    // navigation, /login would reload forever and the button would be
    // unreachable.
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(401)));

    renderAt("/login");

    expect(await screen.findByRole("button", { name: /Sign in with Discord/ })).toBeInTheDocument();
    expect(assign).not.toHaveBeenCalled();
  });

  it("keeps the error query intact instead of folding it into return_to", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(res(401)));

    renderAt("/login?error=denied");

    expect(await screen.findByRole("alert")).toHaveTextContent(/キャンセル/);
    expect(assign).not.toHaveBeenCalled();
  });

  it("sends an already signed-in visitor to their return_to", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(
        res(200, {
          id: "u1",
          display_name: "Alice",
          avatar_url: null,
          timezone: "UTC",
          heart_rate_visibility: "group_default",
        }),
      ),
    );

    renderAt("/login?return_to=%2Fme");

    expect(await screen.findByText("me page")).toBeInTheDocument();
  });
});
