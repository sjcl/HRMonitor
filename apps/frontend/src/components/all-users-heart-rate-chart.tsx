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

type TimeRange =
  | { kind: "preset"; seconds: number; label: string }
  | { kind: "custom"; from: number; to: number };

export function AllUsersHeartRateChart() {
  const [range, setRange] = useState<TimeRange>({
    kind: "preset",
    seconds: 3600,
    label: "1h",
  });
  const [showCustom, setShowCustom] = useState(false);
  const [customFrom, setCustomFrom] = useState("");
  const [customTo, setCustomTo] = useState("");

  const { data: users } = useQuery({
    queryKey: ["users"],
    queryFn: getUsers,
    refetchInterval: 5000,
  });

  const userIds = (users ?? []).map((u) => u.id).sort().join(",");

  const { data: allRecords } = useQuery({
    queryKey: ["all-heart-rates", userIds, range],
    queryFn: async () => {
      if (!users?.length) return [];
      const now = Math.floor(Date.now() / 1000);
      const from = range.kind === "preset" ? now - range.seconds : range.from;
      const to = range.kind === "custom" ? range.to : undefined;
      const limit =
        range.kind === "preset"
          ? Math.min(2000, Math.max(500, Math.ceil(range.seconds / 3)))
          : 2000;
      const results = await Promise.all(
        users.map(async (u) => ({
          userId: u.id,
          name: u.name,
          records: await getHeartRates(u.id, { from, to, limit }),
        }))
      );
      return results;
    },
    enabled: !!users?.length,
    refetchInterval: 5000,
  });

  const useShortFormat = range.kind === "preset" && range.seconds <= 10800;

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

  const applyCustomRange = () => {
    if (!customFrom || !customTo) return;
    const from = Math.floor(new Date(customFrom).getTime() / 1000);
    const to = Math.floor(new Date(customTo).getTime() / 1000);
    if (from >= to) return;
    setRange({ kind: "custom", from, to });
  };

  const rangeLabel =
    range.kind === "preset" ? `the last ${range.label}` : "the selected range";

  return (
    <div className="border border-gray-800 rounded-lg p-4 mb-6">
      <div className="flex items-center justify-between mb-3">
        <h2 className="text-lg font-semibold">All Users Heart Rate</h2>
        <div className="flex flex-wrap items-center gap-2">
          {PRESETS.map((p) => (
            <button
              key={p.label}
              onClick={() => {
                setRange({ kind: "preset", seconds: p.seconds, label: p.label });
                setShowCustom(false);
              }}
              className={`px-3 py-1 rounded text-sm ${
                range.kind === "preset" && range.seconds === p.seconds
                  ? "bg-gray-600 text-white"
                  : "bg-gray-800 text-gray-400 hover:bg-gray-700"
              }`}
            >
              {p.label}
            </button>
          ))}
          <button
            onClick={() => setShowCustom((v) => !v)}
            className={`px-3 py-1 rounded text-sm ${
              range.kind === "custom"
                ? "bg-gray-600 text-white"
                : "bg-gray-800 text-gray-400 hover:bg-gray-700"
            }`}
          >
            Custom
          </button>
        </div>
      </div>

      {showCustom && (
        <div className="flex flex-wrap items-end justify-end gap-3 mb-3">
          <label className="text-sm text-gray-400">
            From
            <input
              type="datetime-local"
              value={customFrom}
              onChange={(e) => setCustomFrom(e.target.value)}
              className="block mt-1 bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-white [color-scheme:dark]"
            />
          </label>
          <label className="text-sm text-gray-400">
            To
            <input
              type="datetime-local"
              value={customTo}
              onChange={(e) => setCustomTo(e.target.value)}
              className="block mt-1 bg-gray-800 border border-gray-700 rounded px-3 py-1.5 text-sm text-white [color-scheme:dark]"
            />
          </label>
          <button
            onClick={applyCustomRange}
            className="bg-green-600 hover:bg-green-700 px-4 py-1.5 rounded text-sm text-white"
          >
            Apply
          </button>
        </div>
      )}

      {chartData.length === 0 ? (
        <div className="p-8 text-center text-gray-500">
          No heart rate data in {rangeLabel}
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
