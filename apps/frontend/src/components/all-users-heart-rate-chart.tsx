"use client";

import { useState, useEffect, useRef, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getUsers, getHeartRates } from "@/lib/api";
import { LatestHeartRate } from "@/lib/ws";
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

function computeTicks(data: Array<{ timestamp: number }>, count = 6): number[] {
  if (data.length === 0) return [];
  const min = data[0].timestamp;
  const max = data[data.length - 1].timestamp;
  if (min === max) return [min];
  const step = (max - min) / (count - 1);
  return Array.from({ length: count }, (_, i) => min + step * i);
}

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
  "#EF4444",
  "#3B82F6",
  "#10B981",
  "#F59E0B",
  "#8B5CF6",
  "#EC4899",
  "#06B6D4",
  "#F97316",
  "#84CC16",
  "#6366F1",
];

export function AllUsersHeartRateChart({
  liveHr,
  wsReconnectCount,
}: {
  liveHr: Map<string, LatestHeartRate>;
  wsReconnectCount: number;
}) {
  const [range, setRange] = useState<(typeof PRESETS)[number]>(PRESETS[2]);
  const isRealtime = range.seconds <= 3600;
  const queryClient = useQueryClient();

  const { data: users } = useQuery({
    queryKey: ["users"],
    queryFn: getUsers,
    staleTime: Infinity,
  });

  const userIds = (users ?? [])
    .map((u) => u.id)
    .sort()
    .join(",");

  const { data: allRecords } = useQuery({
    queryKey: ["all-heart-rates", userIds, range.label],
    queryFn: async () => {
      if (!users?.length) return [];
      const results = await Promise.all(
        users.map(async (u) => ({
          userId: u.id,
          name: u.name,
          records: await getHeartRates(u.id, range.label),
        })),
      );
      return results;
    },
    enabled: !!users?.length,
    refetchInterval: isRealtime ? false : 60_000,
    refetchOnWindowFocus: !isRealtime,
    refetchOnReconnect: !isRealtime,
    staleTime: isRealtime ? Infinity : undefined,
  });

  // Refetch on WS reconnect
  const prevReconnectCount = useRef(wsReconnectCount);
  useEffect(() => {
    if (wsReconnectCount !== prevReconnectCount.current) {
      prevReconnectCount.current = wsReconnectCount;
      if (isRealtime) {
        queryClient.invalidateQueries({
          queryKey: ["all-heart-rates", userIds, range.label],
        });
      }
    }
  }, [wsReconnectCount, isRealtime, queryClient, userIds, range.label]);

  // Per-user WS buffers
  const [wsBuffers, setWsBuffers] = useState<
    Map<string, Array<{ timestamp: number; bpm: number }>>
  >(new Map());
  const lastProcessedRef = useRef<Map<string, number>>(new Map());

  useEffect(() => {
    setWsBuffers(new Map());
    lastProcessedRef.current = new Map();
  }, [range.label]);

  useEffect(() => {
    if (!isRealtime) return;
    const cutoff = Date.now() / 1000 - range.seconds;

    setWsBuffers((prev) => {
      const next = new Map(prev);
      let changed = false;

      for (const [uid, hr] of liveHr) {
        const lastTs = lastProcessedRef.current.get(uid);
        if (lastTs === hr.recorded_at) continue;
        lastProcessedRef.current.set(uid, hr.recorded_at);

        const buf = next.get(uid) ?? [];
        const appended = [
          ...buf,
          { timestamp: hr.recorded_at, bpm: hr.bpm },
        ];
        const firstValid = appended.findIndex((p) => p.timestamp >= cutoff);
        next.set(uid, firstValid > 0 ? appended.slice(firstValid) : appended);
        changed = true;
      }

      for (const uid of next.keys()) {
        if (!liveHr.has(uid)) {
          next.delete(uid);
          lastProcessedRef.current.delete(uid);
          changed = true;
        }
      }

      return changed ? next : prev;
    });
  }, [liveHr, isRealtime, range.seconds]);

  const useShortFormat = range.seconds <= 10800;

  const formatTimestamp = (tsMs: number): string => {
    const d = new Date(tsMs);
    return useShortFormat
      ? d.toLocaleTimeString([], {
          hour: "2-digit",
          minute: "2-digit",
          second: "2-digit",
        })
      : d.toLocaleString([], {
          month: "short",
          day: "numeric",
          hour: "2-digit",
          minute: "2-digit",
        });
  };

  const { chartData, userMeta } = useMemo(() => {
    const now = Date.now() / 1000;
    const cutoff = isRealtime ? now - range.seconds : 0;

    const BUCKET_SIZE = 5;
    const bucketMap = new Map<number, Record<string, unknown>>();
    const meta: { id: string; name: string }[] = [];

    // API data — cutoff before bucketing, latest-wins per bucket
    if (allRecords?.length) {
      for (const { userId, name, records } of allRecords) {
        meta.push({ id: userId, name });
        for (const r of records) {
          if (r.timestamp < cutoff) continue;
          const bucket = Math.round(r.timestamp / BUCKET_SIZE) * BUCKET_SIZE;
          if (!bucketMap.has(bucket))
            bucketMap.set(bucket, { _ts: bucket });
          const row = bucketMap.get(bucket)!;
          const existingTs =
            (row[`_ts_${userId}`] as number | undefined) ?? 0;
          if (r.timestamp >= existingTs) {
            row[userId] = r.bpm;
            row[`_ts_${userId}`] = r.timestamp;
          }
        }
      }
    }

    // WS buffer data — same logic
    for (const [uid, buf] of wsBuffers) {
      if (!meta.some((u) => u.id === uid)) continue;
      for (const p of buf) {
        if (p.timestamp < cutoff) continue;
        const bucket = Math.round(p.timestamp / BUCKET_SIZE) * BUCKET_SIZE;
        if (!bucketMap.has(bucket))
          bucketMap.set(bucket, { _ts: bucket });
        const row = bucketMap.get(bucket)!;
        const existingTs = (row[`_ts_${uid}`] as number | undefined) ?? 0;
        if (p.timestamp >= existingTs) {
          row[uid] = p.bpm;
          row[`_ts_${uid}`] = p.timestamp;
        }
      }
    }

    // Clean tracking keys before render
    const data = [...bucketMap.values()]
      .sort((a, b) => (a._ts as number) - (b._ts as number))
      .map((row) => {
        const clean: Record<string, unknown> = {
          timestamp: (row._ts as number) * 1000,
        };
        for (const [k, v] of Object.entries(row)) {
          if (!k.startsWith("_")) clean[k] = v;
        }
        return clean;
      });

    return { chartData: data, userMeta: meta };
  }, [allRecords, wsBuffers, isRealtime, range.seconds]);

  const xTicks = useMemo(
    () => computeTicks(chartData as Array<{ timestamp: number }>),
    [chartData],
  );

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
              domain={["dataMin", "dataMax"]}
              ticks={xTicks}
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
                isAnimationActive={false}
              />
            ))}
          </LineChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}
