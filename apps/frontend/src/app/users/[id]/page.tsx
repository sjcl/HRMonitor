"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getUser, updateUser } from "@/lib/api";
import { HeartRateChart } from "@/components/heart-rate-chart";
import { DailyStats } from "@/components/daily-stats";
import { PulsoidToken } from "@/components/pulsoid-token";
import { TimezoneSelect } from "@/components/timezone-select";
import { useHeartRateWs } from "@/lib/ws";
import { use, useState, useEffect, useMemo } from "react";
import Link from "next/link";

export default function UserDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  const queryClient = useQueryClient();

  const { data: user } = useQuery({
    queryKey: ["user", id],
    queryFn: () => getUser(id),
  });

  const userIds = useMemo(() => [id], [id]);
  const { data: liveHrData, reconnectCount } = useHeartRateWs(userIds);
  const latestHr = liveHrData.get(id) ?? null;

  const [editName, setEditName] = useState("");
  const [editTimezone, setEditTimezone] = useState("");

  useEffect(() => {
    if (user) {
      setEditName(user.name);
      setEditTimezone(user.timezone);
    }
  }, [user]);

  const updateMutation = useMutation({
    mutationFn: (data: { name?: string; timezone?: string }) =>
      updateUser(id, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["user", id] });
      queryClient.invalidateQueries({ queryKey: ["users"] });
    },
  });

  const hasChanges =
    user && (editName !== user.name || editTimezone !== user.timezone);

  return (
    <div>
      <Link href="/users" className="text-sm text-gray-400 hover:underline">
        &larr; Back to users
      </Link>

      <div className="flex items-center gap-4 mt-4 mb-6">
        <h1 className="text-2xl font-bold">{user?.name ?? "Loading..."}</h1>
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

      <section className="mb-8">
        <h2 className="text-lg font-semibold mb-3">Pulsoid</h2>
        <PulsoidToken userId={id} />
      </section>

      <section>
        <h2 className="text-lg font-semibold mb-3">Settings</h2>
        <div className="flex flex-col gap-3 max-w-md">
          <div>
            <label className="block text-sm text-gray-400 mb-1">Name</label>
            <input
              type="text"
              value={editName}
              onChange={(e) => setEditName(e.target.value)}
              className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full"
            />
          </div>
          <div>
            <label className="block text-sm text-gray-400 mb-1">Timezone</label>
            <TimezoneSelect
              value={editTimezone}
              onChange={setEditTimezone}
              className="w-full"
            />
          </div>
          {hasChanges && (
            <button
              onClick={() =>
                updateMutation.mutate({
                  name: editName.trim(),
                  timezone: editTimezone,
                })
              }
              disabled={updateMutation.isPending || !editName.trim()}
              className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm disabled:opacity-50 self-start"
            >
              {updateMutation.isPending ? "Saving..." : "Save"}
            </button>
          )}
        </div>
      </section>
    </div>
  );
}
