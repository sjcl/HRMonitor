"use client";

import { Suspense } from "react";
import { useSession } from "next-auth/react";
import { useSearchParams, useRouter } from "next/navigation";
import { UserDetailView } from "@/components/user-detail-view";
import { SharedUsersView } from "@/components/shared-users-view";

type Tab = "self" | "shared";

export default function DashboardPage() {
  return (
    <Suspense fallback={<p className="text-gray-400">Loading...</p>}>
      <DashboardContent />
    </Suspense>
  );
}

function DashboardContent() {
  const { data: session, status } = useSession();
  const searchParams = useSearchParams();
  const router = useRouter();

  const tabParam = searchParams.get("tab");
  const tab: Tab = tabParam === "shared" ? "shared" : "self";

  const setTab = (next: Tab) => {
    const params = new URLSearchParams(searchParams.toString());
    if (next === "self") {
      params.delete("tab");
    } else {
      params.set("tab", next);
    }
    const qs = params.toString();
    router.replace(qs ? `/dashboard?${qs}` : "/dashboard");
  };

  const selfUserId = session?.user?.id ?? null;

  return (
    <div>
      <div className="flex gap-2 mb-6">
        <TabButton active={tab === "self"} onClick={() => setTab("self")}>
          自分
        </TabButton>
        <TabButton active={tab === "shared"} onClick={() => setTab("shared")}>
          共有
        </TabButton>
      </div>

      {tab === "self" ? (
        status === "loading" ? (
          <p className="text-gray-400">Loading...</p>
        ) : selfUserId ? (
          <UserDetailView userId={selfUserId} />
        ) : (
          <p className="text-gray-400">Not signed in.</p>
        )
      ) : (
        <SharedUsersView />
      )}
    </div>
  );
}

function TabButton({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`px-4 py-2 rounded-md text-sm font-medium transition-colors ${
        active
          ? "bg-gray-800 text-gray-100"
          : "text-gray-400 hover:text-gray-200 hover:bg-gray-900"
      }`}
    >
      {children}
    </button>
  );
}
