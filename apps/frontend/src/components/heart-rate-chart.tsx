"use client";

import { useState, useEffect, useRef, useMemo } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { getHeartRates, getMinuteStats } from "@/lib/api";
import { LatestHeartRate } from "@/lib/ws";
import {
  ComposedChart,
  Area,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
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

export function HeartRateChart({
  userId,
  latestHr,
  wsReconnectCount,
}: {
  userId: string;
  latestHr?: LatestHeartRate | null;
  wsReconnectCount?: number;
}) {
  const [range, setRange] = useState<(typeof PRESETS)[number]>(PRESETS[2]);
  const isRealtime = range.seconds <= 3600;
  const useMinuteStats = range.seconds >= 10800;
  const queryClient = useQueryClient();

  const { data: records, isPending: isPendingRaw } = useQuery({
    queryKey: ["heart-rates", userId, range.label],
    queryFn: () => getHeartRates(userId, range.label),
    enabled: !useMinuteStats,
    refetchInterval: isRealtime ? false : 60_000,
    refetchOnMount: isRealtime ? "always" : true,
    refetchOnWindowFocus: isRealtime ? "always" : true,
    refetchOnReconnect: isRealtime ? "always" : true,
    staleTime: isRealtime ? Infinity : undefined,
  });

  const { data: minuteRecords, isPending: isPendingMinute } = useQuery({
    queryKey: ["minute-stats", userId, range.label],
    queryFn: () => getMinuteStats(userId, range.label),
    enabled: useMinuteStats,
    refetchInterval: 60_000,
    refetchOnMount: true,
    refetchOnWindowFocus: true,
    refetchOnReconnect: true,
  });

  const isPending = useMinuteStats ? isPendingMinute : isPendingRaw;

  // Refetch on WS reconnect to fill gaps
  const prevReconnectCount = useRef(wsReconnectCount);
  useEffect(() => {
    if (
      wsReconnectCount !== undefined &&
      wsReconnectCount !== prevReconnectCount.current
    ) {
      prevReconnectCount.current = wsReconnectCount;
      if (isRealtime) {
        queryClient.invalidateQueries({
          queryKey: ["heart-rates", userId, range.label],
        });
      }
    }
  }, [wsReconnectCount, isRealtime, queryClient, userId, range.label]);

  // WS buffer for realtime mode
  const [wsBuffer, setWsBuffer] = useState<
    Array<{ timestamp: number; bpm: number }>
  >([]);

  useEffect(() => {
    setWsBuffer([]);
  }, [range.label, userId]);

  useEffect(() => {
    if (!isRealtime || !latestHr) return;
    const cutoff = Date.now() / 1000 - range.seconds;
    setWsBuffer((prev) => {
      if (
        prev.length > 0 &&
        prev[prev.length - 1].timestamp === latestHr.recorded_at
      )
        return prev;
      const next = [
        ...prev,
        { timestamp: latestHr.recorded_at, bpm: latestHr.bpm },
      ];
      const firstValid = next.findIndex((p) => p.timestamp >= cutoff);
      return firstValid > 0 ? next.slice(firstValid) : next;
    });
  }, [latestHr, isRealtime, range.seconds]);

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

  // Merge API + WS data with moving window cutoff (raw mode)
  const rawChartData = useMemo(() => {
    if (useMinuteStats) return [];
    const now = Date.now() / 1000;
    const cutoff = isRealtime ? now - range.seconds : 0;

    const apiPoints = (records ?? []).map((r) => ({
      timestamp: r.timestamp,
      bpm: r.bpm,
    }));
    const apiTimestamps = new Set(apiPoints.map((p) => p.timestamp));
    const uniqueWsPoints = wsBuffer.filter(
      (p) => !apiTimestamps.has(p.timestamp),
    );

    return [...apiPoints, ...uniqueWsPoints]
      .filter((p) => p.timestamp >= cutoff)
      .sort((a, b) => a.timestamp - b.timestamp)
      .map((r) => ({
        timestamp: r.timestamp * 1000,
        bpm: r.bpm,
      }));
  }, [records, wsBuffer, isRealtime, range.seconds, useMinuteStats]);

  // Minute-stats chart data (aggregate mode)
  const minuteChartData = useMemo(() => {
    if (!useMinuteStats) return [];
    return (minuteRecords ?? []).map((r) => ({
      timestamp: r.timestamp * 1000,
      avg_bpm: Math.round(r.avg_bpm * 10) / 10,
      min_bpm: r.min_bpm,
      max_bpm: r.max_bpm,
      band: r.max_bpm - r.min_bpm,
    }));
  }, [minuteRecords, useMinuteStats]);

  const chartData = useMinuteStats ? minuteChartData : rawChartData;
  const xTicks = useMemo(
    () => computeTicks(chartData as Array<{ timestamp: number }>),
    [chartData],
  );

  return (
    <div className="border border-gray-800 rounded-lg p-4">
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
          <ComposedChart data={chartData as Record<string, unknown>[]}>
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
              formatter={(value, name) => {
                if (useMinuteStats) {
                  const labels: Record<string, string> = {
                    avg_bpm: "Avg",
                    min_bpm: "Min",
                    max_bpm: "Max",
                  };
                  if (labels[name as string]) return [value, labels[name as string]];
                  return [undefined, undefined];
                }
                return [value, "BPM"];
              }}
            />
            {useMinuteStats ? (
              <>
                <Area
                  type="monotone"
                  dataKey="min_bpm"
                  stackId="minmax"
                  fill="transparent"
                  stroke="none"
                  isAnimationActive={false}
                />
                <Area
                  type="monotone"
                  dataKey="band"
                  stackId="minmax"
                  fill="#EF4444"
                  fillOpacity={0.15}
                  stroke="none"
                  isAnimationActive={false}
                  tooltipType="none"
                />
                <Line
                  type="monotone"
                  dataKey="avg_bpm"
                  stroke="#EF4444"
                  strokeWidth={2}
                  dot={false}
                  isAnimationActive={false}
                />
                <Line
                  type="monotone"
                  dataKey="min_bpm"
                  stroke="transparent"
                  dot={false}
                  isAnimationActive={false}
                  legendType="none"
                />
                <Line
                  type="monotone"
                  dataKey="max_bpm"
                  stroke="transparent"
                  dot={false}
                  isAnimationActive={false}
                  legendType="none"
                />
              </>
            ) : (
              <Line
                type="monotone"
                dataKey="bpm"
                stroke="#EF4444"
                strokeWidth={2}
                dot={false}
                isAnimationActive={false}
              />
            )}
          </ComposedChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}
