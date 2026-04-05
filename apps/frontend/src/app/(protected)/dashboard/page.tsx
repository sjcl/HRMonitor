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
  const { data: session } = useSession();
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
      <div className="mb-6">
        <h1 className="text-2xl font-bold text-gray-100">Dashboard</h1>
        <p className="text-sm text-gray-400 mt-1">心拍データのモニタリングと共有</p>
      </div>

      <div className="flex gap-6 border-b border-gray-800 mb-6">
        <TabButton
          active={tab === "self"}
          onClick={() => setTab("self")}
          icon={<HeartIcon />}
        >
          自分
        </TabButton>
        <TabButton
          active={tab === "shared"}
          onClick={() => setTab("shared")}
          icon={<UsersIcon />}
        >
          共有
        </TabButton>
      </div>

      {tab === "self" ? (
        selfUserId ? <UserDetailView userId={selfUserId} /> : null
      ) : (
        <SharedUsersView />
      )}
    </div>
  );
}

function TabButton({
  active,
  onClick,
  icon,
  children,
}: {
  active: boolean;
  onClick: () => void;
  icon?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      className={`inline-flex items-center gap-2 px-1 pb-3 pt-1 text-sm font-medium border-b-2 -mb-px transition-colors ${
        active
          ? "border-red-500 text-gray-100"
          : "border-transparent text-gray-400 hover:text-gray-200 hover:border-gray-700"
      }`}
    >
      {icon}
      {children}
    </button>
  );
}

function HeartIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="w-4 h-4"
    >
      <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
    </svg>
  );
}

function UsersIcon() {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      width="16"
      height="16"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="w-4 h-4"
    >
      <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" />
      <circle cx="9" cy="7" r="4" />
      <path d="M22 21v-2a4 4 0 0 0-3-3.87" />
      <path d="M16 3.13a4 4 0 0 1 0 7.75" />
    </svg>
  );
}
