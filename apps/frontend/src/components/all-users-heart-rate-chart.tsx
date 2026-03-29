"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { getUsers, getHeartRates } from "@/lib/api";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from "recharts";

const PRESETS = [
  { seconds: 600, label: "10m" },
  { seconds: 1800, label: "30m" },
  { seconds: 3600, label: "1h" },
  { seconds: 10800, label: "3h" },
  { seconds: 21600, label: "6h" },
  { seconds: 43200, label: "12h" },
  { seconds: 86400, label: "24h" },
] as const;

const USER_COLORS = [
  "#EF4444", "#3B82F6", "#10B981", "#F59E0B", "#8B5CF6",
  "#EC4899", "#06B6D4", "#F97316", "#84CC16", "#6366F1",
];

export function AllUsersHeartRateChart() {
  const [range, setRange] = useState<(typeof PRESETS)[number]>(PRESETS[2]);

  const { data: users } = useQuery({
    queryKey: ["users"],
    queryFn: getUsers,
    refetchInterval: 5000,
  });

  const userIds = (users ?? []).map((u) => u.id).sort().join(",");

  const { data: allRecords } = useQuery({
    queryKey: ["all-heart-rates", userIds, range.label],
    queryFn: async () => {
      if (!users?.length) return [];
      const results = await Promise.all(
        users.map(async (u) => ({
          userId: u.id,
          name: u.name,
          records: await getHeartRates(u.id, range.label),
        }))
      );
      return results;
    },
    enabled: !!users?.length,
    refetchInterval: 5000,
  });

  const useShortFormat = range.seconds <= 10800;

  const formatTimestamp = (tsMs: number): string => {
    const d = new Date(tsMs);
    return useShortFormat
      ? d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })
      : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  };

  const BUCKET_SIZE = 5;
  const bucketMap = new Map<number, Record<string, unknown>>();
  const userMeta: { id: string; name: string }[] = [];

  if (allRecords?.length) {
    for (const { userId, name, records } of allRecords) {
      userMeta.push({ id: userId, name });
      for (const r of records) {
        const bucket = Math.round(r.timestamp / BUCKET_SIZE) * BUCKET_SIZE;
        if (!bucketMap.has(bucket)) {
          bucketMap.set(bucket, { _ts: bucket });
        }
        bucketMap.get(bucket)![userId] = r.bpm;
      }
    }
  }

  const chartData = [...bucketMap.values()]
    .sort((a, b) => (a._ts as number) - (b._ts as number))
    .map((row) => ({
      ...row,
      timestamp: (row._ts as number) * 1000,
    }));

  return (
    <div className="border border-gray-800 rounded-lg p-4 mb-6">
      <div className="flex items-center justify-between mb-3">
        <h2 className="text-lg font-semibold">All Users Heart Rate</h2>
        <div className="flex flex-wrap items-center gap-2">
          {PRESETS.map((p) => (
            <button
              key={p.label}
              onClick={() => setRange(p)}
              className={`px-3 py-1 rounded text-sm ${
                range.label === p.label
                  ? "bg-gray-600 text-white"
                  : "bg-gray-800 text-gray-400 hover:bg-gray-700"
              }`}
            >
              {p.label}
            </button>
          ))}
        </div>
      </div>

      {chartData.length === 0 ? (
        <div className="p-8 text-center text-gray-500">
          No heart rate data in the last {range.label}
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={300}>
          <LineChart data={chartData}>
            <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
            <XAxis
              dataKey="timestamp"
              type="number"
              scale="time"
              domain={['dataMin', 'dataMax']}
              tickFormatter={formatTimestamp}
              stroke="#9CA3AF"
              fontSize={12}
            />
            <YAxis domain={[40, 200]} stroke="#9CA3AF" fontSize={12} />
            <Tooltip
              labelFormatter={(value) => formatTimestamp(value as number)}
              contentStyle={{
                backgroundColor: "#1F2937",
                border: "1px solid #374151",
                borderRadius: "8px",
              }}
            />
            <Legend />
            {userMeta.map((u, i) => (
              <Line
                key={u.id}
                type="monotone"
                dataKey={u.id}
                name={u.name}
                stroke={USER_COLORS[i % USER_COLORS.length]}
                strokeWidth={2}
                dot={false}
              />
            ))}
          </LineChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}
