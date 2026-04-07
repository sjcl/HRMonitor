"use client";

import { use, Suspense } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { UserDetailView } from "@/components/user-detail-view";

const UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export default function UserDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);

  return (
    <Suspense>
      <UserDetailContent userId={id} />
    </Suspense>
  );
}

function UserDetailContent({ userId }: { userId: string }) {
  const searchParams = useSearchParams();
  const fromGroup = searchParams.get("from");
  const isValidGroup = fromGroup !== null && UUID_RE.test(fromGroup);
  const backHref = isValidGroup ? `/groups/${fromGroup}` : "/groups";
  const backLabel = isValidGroup ? "Back to group" : "Back to groups";

  return (
    <div>
      <Link href={backHref} className="text-sm text-gray-400 hover:underline">
        &larr; {backLabel}
      </Link>
      <div className="mt-4">
        <UserDetailView userId={userId} />
      </div>
    </div>
  );
}
