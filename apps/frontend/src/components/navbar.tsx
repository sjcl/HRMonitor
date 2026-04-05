"use client";

import { UserMenu } from "@/components/user-menu";

export function Navbar() {
  return (
    <nav className="border-b border-gray-800 px-6 py-4 flex items-center justify-between">
      <a href="/dashboard" className="text-xl font-bold">
        HR Monitor
      </a>
      <UserMenu />
    </nav>
  );
}
