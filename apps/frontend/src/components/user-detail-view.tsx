"use client";

import { useQuery } from "@tanstack/react-query";
import { getUser } from "@/lib/api";
import { HeartRateChart } from "@/components/heart-rate-chart";
import { DailyStats } from "@/components/daily-stats";
import { useHeartRateWs } from "@/lib/ws";
import { useMemo } from "react";
import { UserAvatar } from "@/components/user-avatar";

export function UserDetailView({ userId }: { userId: string }) {
  const { data: user } = useQuery({
    queryKey: ["user", userId],
    queryFn: () => getUser(userId),
  });

  const userIds = useMemo(() => [userId], [userId]);
  const { data: liveHrData, reconnectCount } = useHeartRateWs(userIds);
  const latestHr = liveHrData.get(userId) ?? null;

  return (
    <div>
      <div className="flex items-center gap-4 mb-6">
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
        <HeartRateChart userId={userId} latestHr={latestHr} wsReconnectCount={reconnectCount} />
      </section>

      {user && (
        <section className="mb-8">
          <h2 className="text-lg font-semibold mb-3">Daily Statistics</h2>
          <DailyStats userId={userId} timezone={user.timezone} key={user.timezone} />
        </section>
      )}
    </div>
  );
}
