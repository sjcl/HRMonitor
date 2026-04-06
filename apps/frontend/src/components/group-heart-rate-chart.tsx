"use client";

import { useState, useEffect, useRef, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  getGroupHeartRates,
  getGroupMinuteStats,
  type GroupMemberInfo,
} from "@/lib/groups-api";
import { type LatestHeartRate } from "@/lib/ws";
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

const BUCKET_SIZE = 5;

function computeTicks(data: Array<{ timestamp: number }>, count = 6): number[] {
  if (data.length === 0) return [];
  const min = data[0].timestamp;
  const max = data[data.length - 1].timestamp;
  if (min === max) return [min];
  const step = (max - min) / (count - 1);
  return Array.from({ length: count }, (_, i) => min + step * i);
}

export function GroupHeartRateChart({
  groupId,
  members,
  currentUserId,
  liveHrData,
  wsReconnectCount,
}: {
  groupId: string;
  members: GroupMemberInfo[];
  currentUserId: string;
  liveHrData: Map<string, LatestHeartRate>;
  wsReconnectCount: number;
}) {
  const [range, setRange] = useState<(typeof PRESETS)[number]>(PRESETS[2]);
  const isRealtime = range.seconds <= 3600;
  const useMinuteStats = range.seconds >= 10800;
  const queryClient = useQueryClient();

  // Members visible in chart: sharing OR self
  const visibleMembers = useMemo(
    () => members.filter((m) => m.sharing || m.user_id === currentUserId),
    [members, currentUserId],
  );

  const { data: rawRecords, isPending: isPendingRaw } = useQuery({
    queryKey: ["group-heart-rates", groupId, range.label],
    queryFn: () => getGroupHeartRates(groupId, range.label),
    enabled: !useMinuteStats,
    refetchInterval: isRealtime ? false : 60_000,
    refetchOnMount: isRealtime ? "always" : true,
    refetchOnWindowFocus: isRealtime ? "always" : true,
    refetchOnReconnect: isRealtime ? "always" : true,
    staleTime: isRealtime ? Infinity : undefined,
  });

  const { data: minuteRecords, isPending: isPendingMinute } = useQuery({
    queryKey: ["group-minute-stats", groupId, range.label],
    queryFn: () => getGroupMinuteStats(groupId, range.label),
    enabled: useMinuteStats,
    refetchInterval: 60_000,
    refetchOnMount: true,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
  });

  const isPending = useMinuteStats ? isPendingMinute : isPendingRaw;

  // Refetch on WS reconnect
  const prevReconnectCount = useRef(wsReconnectCount);
  useEffect(() => {
    if (wsReconnectCount !== prevReconnectCount.current) {
      prevReconnectCount.current = wsReconnectCount;
      if (isRealtime) {
        queryClient.invalidateQueries({
          queryKey: ["group-heart-rates", groupId, range.label],
        });
      }
    }
  }, [wsReconnectCount, isRealtime, queryClient, groupId, range.label]);

  // Per-user WS buffer for realtime mode
  const [wsBuffer, setWsBuffer] = useState<
    Map<string, Array<{ timestamp: number; bpm: number }>>
  >(new Map());

  useEffect(() => {
    setWsBuffer(new Map());
  }, [range.label, groupId]);

  useEffect(() => {
    if (!isRealtime) return;
    const cutoff = Date.now() / 1000 - range.seconds;

    setWsBuffer((prev) => {
      let changed = false;
      const next = new Map(prev);

      for (const [userId, hr] of liveHrData) {
        const existing = next.get(userId) ?? [];
        if (existing.length > 0 && existing[existing.length - 1].timestamp === hr.recorded_at) {
          continue;
        }
        changed = true;
        const updated = [...existing, { timestamp: hr.recorded_at, bpm: hr.bpm }];
        const firstValid = updated.findIndex((p) => p.timestamp >= cutoff);
        next.set(userId, firstValid > 0 ? updated.slice(firstValid) : updated);
      }

      return changed ? next : prev;
    });
  }, [liveHrData, isRealtime, range.seconds]);

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

  // Build chart data: bucket-based for raw, per-user lines for minute-stats
  const chartData = useMemo(() => {
    if (useMinuteStats) {
      // Minute-stats mode: bucket by timestamp, one key per user
      const bucketMap = new Map<number, Record<string, unknown>>();
      for (const r of minuteRecords ?? []) {
        const ts = r.timestamp * 1000;
        if (!bucketMap.has(ts)) {
          bucketMap.set(ts, { timestamp: ts });
        }
        bucketMap.get(ts)![r.user_id] = Math.round(r.avg_bpm * 10) / 10;
      }
      return [...bucketMap.values()].sort(
        (a, b) => (a.timestamp as number) - (b.timestamp as number),
      );
    }

    // Raw mode: 5-second buckets, merge API + WS
    const now = Date.now() / 1000;
    const cutoff = isRealtime ? now - range.seconds : 0;

    const bucketMap = new Map<number, Record<string, unknown>>();

    // API data
    const apiTimestamps = new Set<string>();
    for (const r of rawRecords ?? []) {
      if (r.timestamp < cutoff) continue;
      const bucket = Math.round(r.timestamp / BUCKET_SIZE) * BUCKET_SIZE;
      apiTimestamps.add(`${r.user_id}:${r.timestamp}`);
      if (!bucketMap.has(bucket)) {
        bucketMap.set(bucket, { _ts: bucket });
      }
      bucketMap.get(bucket)![r.user_id] = r.bpm;
    }

    // WS buffer (only add points not in API)
    for (const [userId, points] of wsBuffer) {
      for (const p of points) {
        if (p.timestamp < cutoff) continue;
        if (apiTimestamps.has(`${userId}:${p.timestamp}`)) continue;
        const bucket = Math.round(p.timestamp / BUCKET_SIZE) * BUCKET_SIZE;
        if (!bucketMap.has(bucket)) {
          bucketMap.set(bucket, { _ts: bucket });
        }
        bucketMap.get(bucket)![userId] = p.bpm;
      }
    }

    return [...bucketMap.values()]
      .sort((a, b) => (a._ts as number) - (b._ts as number))
      .map((row) => ({
        ...row,
        timestamp: (row._ts as number) * 1000,
      }));
  }, [rawRecords, wsBuffer, minuteRecords, isRealtime, range.seconds, useMinuteStats]);

  const xTicks = useMemo(
    () => computeTicks(chartData as Array<{ timestamp: number }>),
    [chartData],
  );

  // Stable color assignment based on visible members order
  const memberColors = useMemo(() => {
    const map = new Map<string, { color: string; name: string }>();
    visibleMembers.forEach((m, i) => {
      map.set(m.user_id, {
        color: USER_COLORS[i % USER_COLORS.length],
        name: m.display_name,
      });
    });
    return map;
  }, [visibleMembers]);

  return (
    <div className="border border-gray-800 rounded-lg p-4 mb-6">
      <div className="flex flex-wrap items-center justify-end gap-2 mb-3">
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

      {isPending ? (
        <div className="p-8 text-center text-gray-500">Loading...</div>
      ) : chartData.length === 0 ? (
        <div className="p-8 text-center text-gray-500">
          No heart rate data in the last {range.label}
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={300}>
          <LineChart data={chartData as Record<string, unknown>[]}>
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
            {visibleMembers.map((m) => {
              const info = memberColors.get(m.user_id);
              return (
                <Line
                  key={m.user_id}
                  type="monotone"
                  dataKey={m.user_id}
                  name={info?.name ?? m.user_id}
                  stroke={info?.color ?? "#9CA3AF"}
                  strokeWidth={2}
                  dot={false}
                  isAnimationActive={false}
                  connectNulls={false}
                />
              );
            })}
          </LineChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}
