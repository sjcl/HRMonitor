"use client";

import { useQuery } from "@tanstack/react-query";
import { ApiError, getHeartRateProfile } from "@/lib/api";
import { HeartRateChart } from "@/components/heart-rate-chart";
import { DailyStats } from "@/components/daily-stats";
import { useUserHeartRateWs } from "@/lib/ws";
import { UserAvatar } from "@/components/user-avatar";
import { useInView } from "@/hooks/use-in-view";

export function UserDetailView({ userId }: { userId: string }) {
  const { data: user, status, error } = useQuery({
    queryKey: ["heart-rate-profile", userId],
    queryFn: () => getHeartRateProfile(userId),
    retry: false,
  });

  const { ref: dailyStatsRef, inView: dailyStatsInView } = useInView();

  // Only subscribe to WS once the user fetch has succeeded — otherwise a
  // forbidden user detail page would still trigger WS subscription.
  const authorized = status === "success";
  const { data: latestHr, reconnectCount } = useUserHeartRateWs(authorized ? userId : null);

  if (status === "error") {
    const forbidden = error instanceof ApiError && error.status === 403;
    return (
      <div>
        <h1 className="text-2xl font-bold mb-4">
          {forbidden ? "非公開" : "エラー"}
        </h1>
        <p className="text-gray-400">
          {forbidden
            ? "このユーザーの心拍データは公開されていません。"
            : error instanceof Error
              ? error.message
              : "ユーザー情報を取得できませんでした。"}
        </p>
      </div>
    );
  }

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

      {authorized && (
        <section className="mb-8">
          <h2 className="text-lg font-semibold mb-3">Heart Rate</h2>
          <HeartRateChart userId={userId} latestHr={latestHr} wsReconnectCount={reconnectCount} />
        </section>
      )}

      {user && (
        <section className="mb-8" ref={dailyStatsRef}>
          <h2 className="text-lg font-semibold mb-3">Daily Statistics</h2>
          {dailyStatsInView && (
            <DailyStats userId={userId} timezone={user.timezone} key={user.timezone} />
          )}
        </section>
      )}
    </div>
  );
}
