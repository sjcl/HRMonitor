"use client";

import { useSession, signOut } from "next-auth/react";
import { UserAvatar } from "@/components/user-avatar";

export function Navbar() {
  const { data: session } = useSession();

  return (
    <nav className="border-b border-gray-800 px-6 py-4 flex items-center justify-between">
      <a href="/users" className="text-xl font-bold">
        HR Monitor
      </a>
      {session?.user && (
        <div className="flex items-center gap-4">
          <div className="flex items-center gap-2">
            <UserAvatar src={session.user.image} name={session.user.name ?? ""} size="sm" />
            <span className="text-sm text-gray-400">
              {session.user.name}
            </span>
          </div>
          <button
            onClick={() => signOut({ redirectTo: "/login" })}
            className="text-sm text-gray-400 hover:text-gray-200"
          >
            Sign out
          </button>
        </div>
      )}
    </nav>
  );
}
