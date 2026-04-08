"use client";

import { SubNav } from "@/components/sub-nav";

export default function GroupsLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <div>
      <SubNav />
      {children}
    </div>
  );
}
