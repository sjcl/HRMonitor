"use client";

import { useQuery } from "@tanstack/react-query";
import { getUser } from "@/lib/api";
import { HeartRateChart } from "@/components/heart-rate-chart";
import { DailyStats } from "@/components/daily-stats";
import { useHeartRateWs } from "@/lib/ws";
import { use, useMemo } from "react";
import Link from "next/link";
import { UserAvatar } from "@/components/user-avatar";

export default function UserDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);

  const { data: user } = useQuery({
    queryKey: ["user", id],
    queryFn: () => getUser(id),
  });

  const userIds = useMemo(() => [id], [id]);
  const { data: liveHrData, reconnectCount } = useHeartRateWs(userIds);
  const latestHr = liveHrData.get(id) ?? null;

  return (
    <div>
      <Link href="/users" className="text-sm text-gray-400 hover:underline">
        &larr; Back to users
      </Link>

      <div className="flex items-center gap-4 mt-4 mb-6">
        <UserAvatar src={user?.avatar_url} name={user?.display_name ?? ""} size="lg" />
        <h1 className="text-2xl font-bold">{user?.display_name ?? "Loading..."}</h1>
        {latestHr && (
          <>
            <span className="text-3xl font-mono font-bold text-red-400">
              {latestHr.bpm} BPM
            </span>
            <span className="text-sm text-gray-400">
              {new Date(latestHr.recorded_at * 1000).toLocaleTimeString("ja-JP", {
                timeZone: user?.timezone,
                hour: "2-digit",
                minute: "2-digit",
                second: "2-digit",
              })}
            </span>
          </>
        )}
      </div>

      <section className="mb-8">
        <h2 className="text-lg font-semibold mb-3">Heart Rate</h2>
        <HeartRateChart userId={id} latestHr={latestHr} wsReconnectCount={reconnectCount} />
      </section>

      {user && (
        <section className="mb-8">
          <h2 className="text-lg font-semibold mb-3">Daily Statistics</h2>
          <DailyStats userId={id} timezone={user.timezone} key={user.timezone} />
        </section>
      )}
    </div>
  );
}
