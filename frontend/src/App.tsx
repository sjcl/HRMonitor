import { Navigate, Outlet, Route, Routes } from "react-router";
import { Navbar } from "./components/navbar";
import { useCurrentUser } from "./lib/auth";
import { redirectToLogin } from "./lib/http";
import { LoginPage } from "./pages/login";
import { InvitePage } from "./pages/invite";
import { MePage } from "./pages/me";
import { UserDetailPage } from "./pages/user-detail";
import { GroupsPage } from "./pages/groups";
import { GroupDetailPage } from "./pages/group-detail";
import { SettingsPage } from "./pages/settings";
import { NotFoundPage } from "./pages/not-found";
import { useEffect } from "react";

/**
 * Gate for the authenticated area.
 *
 * UI control only. Every route it wraps is also enforced by the API, which
 * checks the access token on each request and re-reads group membership and
 * visibility from the database — nothing here is a security boundary, and it
 * must not be mistaken for one.
 */
function ProtectedRoute() {
  const { data: user, isPending, isError } = useCurrentUser();

  useEffect(() => {
    if (!isPending && !isError && user === null) redirectToLogin();
  }, [isPending, isError, user]);

  if (isPending) {
    return (
      <div className="p-8 text-center text-gray-500" role="status">
        読み込み中...
      </div>
    );
  }

  if (isError) {
    // Distinct from "logged out": the session store is unreachable, and the
    // retry is already in flight. Sending the user to /login here would turn a
    // brief outage into a forced re-login.
    return (
      <div className="p-8 text-center text-gray-500" role="alert">
        サーバーに接続できません。再試行しています...
      </div>
    );
  }

  if (user === null) return null;

  return (
    <>
      <Navbar user={user} />
      <main className="mx-auto max-w-5xl px-4 py-6">
        <Outlet />
      </main>
    </>
  );
}

/** Shell for routes rendered without the authenticated chrome. */
function PublicLayout() {
  return (
    <main className="mx-auto max-w-5xl px-4 py-6">
      <Outlet />
    </main>
  );
}

export function App() {
  return (
    <Routes>
      <Route element={<PublicLayout />}>
        <Route path="/login" element={<LoginPage />} />
        <Route path="/invite/:token" element={<InvitePage />} />
      </Route>

      <Route element={<ProtectedRoute />}>
        <Route index element={<Navigate to="/me" replace />} />
        <Route path="/me" element={<MePage />} />
        {/* Kept as a redirect: the old list page linked here. */}
        <Route path="/users" element={<Navigate to="/me" replace />} />
        <Route path="/users/:id" element={<UserDetailPage />} />
        <Route path="/groups" element={<GroupsPage />} />
        <Route path="/groups/:id" element={<GroupDetailPage />} />
        <Route path="/settings" element={<SettingsPage />} />
      </Route>

      <Route path="*" element={<NotFoundPage />} />
    </Routes>
  );
}
