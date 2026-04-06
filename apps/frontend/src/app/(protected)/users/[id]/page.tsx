"use client";

import { use } from "react";
import Link from "next/link";
import { UserDetailView } from "@/components/user-detail-view";

export default function UserDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);

  return (
    <div>
      <Link href="/groups" className="text-sm text-gray-400 hover:underline">
        &larr; Back to groups
      </Link>
      <div className="mt-4">
        <UserDetailView userId={id} />
      </div>
    </div>
  );
}
