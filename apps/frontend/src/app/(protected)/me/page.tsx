"use client";

import { useSession } from "next-auth/react";
import { UserDetailView } from "@/components/user-detail-view";
import { SubNav } from "@/components/sub-nav";

export default function MePage() {
  const { data: session } = useSession();
  const selfUserId = session?.user?.id ?? null;

  return (
    <div>
      <SubNav />
      {selfUserId ? <UserDetailView userId={selfUserId} /> : null}
    </div>
  );
}
