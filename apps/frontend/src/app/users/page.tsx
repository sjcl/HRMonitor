"use client";

import { useQuery } from "@tanstack/react-query";
import { getUsers, type UserListItem } from "@/lib/api";
import { useMemo, memo } from "react";
import Link from "next/link";
import { AllUsersHeartRateChart } from "@/components/all-users-heart-rate-chart";
import { useHeartRateWs, type LatestHeartRate } from "@/lib/ws";

export default function UsersPage() {
  const { data: users, isLoading } = useQuery({
    queryKey: ["users"],
    queryFn: getUsers,
    staleTime: Infinity,
  });

  const userIds = useMemo(() => (users ?? []).map((u) => u.id), [users]);
  const { data: liveHr, reconnectCount } = useHeartRateWs(userIds);

  return (
    <div>
      <div className="flex items-center justify-between mb-6">
        <h1 className="text-2xl font-bold">Users</h1>
      </div>

      <AllUsersHeartRateChart liveHr={liveHr} wsReconnectCount={reconnectCount} />

      {isLoading ? (
        <p className="text-gray-400">Loading...</p>
      ) : (
        <div className="border border-gray-800 rounded-lg overflow-hidden">
          <table className="w-full">
            <thead className="bg-gray-900">
              <tr>
                <th className="text-left px-4 py-3 text-sm text-gray-400">Name</th>
                <th className="text-right px-4 py-3 text-sm text-gray-400">Latest BPM</th>
                <th className="text-right px-4 py-3 text-sm text-gray-400">Time</th>
                <th className="text-right px-4 py-3 text-sm text-gray-400">Pulsoid</th>
              </tr>
            </thead>
            <tbody>
              {users?.map((user) => (
                <UserRow
                  key={user.id}
                  user={user}
                  liveHr={liveHr.get(user.id) ?? null}
                />
              ))}
              {users?.length === 0 && (
                <tr>
                  <td colSpan={4} className="px-4 py-8 text-center text-gray-500">
                    No users yet
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

const UserRow = memo(function UserRow({
  user,
  liveHr,
}: {
  user: UserListItem;
  liveHr: LatestHeartRate | null;
}) {
  return (
    <tr className="border-t border-gray-800 hover:bg-gray-900/50">
      <td className="px-4 py-3">
        <Link
          href={`/users/${user.id}`}
          className="text-blue-400 hover:underline"
        >
          {user.display_name}
        </Link>
      </td>
      <td className="px-4 py-3 text-right">
        <BpmBadge bpm={liveHr?.bpm ?? user.latest_bpm} />
      </td>
      <td className="px-4 py-3 text-right text-sm text-gray-400">
        <RecordedAtLabel epochSecs={liveHr?.recorded_at ?? null} />
      </td>
      <td className="px-4 py-3 text-right">
        {user.has_pulsoid_token ? (
          <span className="inline-block w-2 h-2 rounded-full bg-green-400" title="Connected" />
        ) : (
          <span className="text-gray-600">--</span>
        )}
      </td>
    </tr>
  );
});

function RecordedAtLabel({ epochSecs }: { epochSecs: number | null }) {
  if (epochSecs === null) return <span className="text-gray-600">--</span>;
  const d = new Date(epochSecs * 1000);
  return (
    <span className="font-mono">
      {d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
    </span>
  );
}

function BpmBadge({ bpm }: { bpm: number | null }) {
  if (bpm === null) return <span className="text-gray-600">--</span>;

  const color =
    bpm >= 60 && bpm <= 100
      ? "text-green-400"
      : bpm > 100
        ? "text-yellow-400"
        : "text-yellow-400";

  return <span className={`font-mono font-bold ${color}`}>{bpm}</span>;
}
