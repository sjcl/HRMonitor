"use client";

import { useState, useMemo, memo } from "react";
import { useQuery } from "@tanstack/react-query";
import { getDailyStats, getMinuteStatsByDate } from "@/lib/api";
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

function formatDateIso(date: Date, tz: string): string {
  const parts = new Intl.DateTimeFormat("en", {
    timeZone: tz,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
  }).formatToParts(date);
  const y = parts.find((p) => p.type === "year")!.value;
  const m = parts.find((p) => p.type === "month")!.value;
  const d = parts.find((p) => p.type === "day")!.value;
  return `${y}-${m}-${d}`;
}

function todayInTz(tz: string): string {
  return formatDateIso(new Date(), tz);
}

function addDays(dateStr: string, days: number): string {
  const [y, m, d] = dateStr.split("-").map(Number);
  const date = new Date(y, m - 1, d + days);
  const yy = date.getFullYear();
  const mm = String(date.getMonth() + 1).padStart(2, "0");
  const dd = String(date.getDate()).padStart(2, "0");
  return `${yy}-${mm}-${dd}`;
}

function getDateBoundsMs(dateStr: string, tz: string): [number, number] {
  const [y, m, d] = dateStr.split("-").map(Number);
  const formatter = new Intl.DateTimeFormat("en-US", {
    timeZone: tz,
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });

  // Binary search for midnight of the given date in the target timezone
  const findMidnight = (targetY: number, targetM: number, targetD: number) => {
    // Start with a rough estimate using UTC
    let guess = new Date(targetY, targetM - 1, targetD).getTime();
    // Adjust: check what date/time this is in the target timezone
    for (let i = 0; i < 3; i++) {
      const parts = formatter.formatToParts(new Date(guess));
      const h = Number(parts.find((p) => p.type === "hour")!.value);
      const min = Number(parts.find((p) => p.type === "minute")!.value);
      const s = Number(parts.find((p) => p.type === "second")!.value);
      // Adjust to midnight
      const offsetMs = (h * 3600 + min * 60 + s) * 1000;
      guess -= offsetMs;
      // Check if we landed on the right day
      const check = formatter.formatToParts(new Date(guess));
      const checkD = Number(check.find((p) => p.type === "day")!.value);
      const checkM = Number(check.find((p) => p.type === "month")!.value);
      if (checkD === targetD && checkM === targetM) break;
      // If we overshot, add a day
      if (checkD < targetD || checkM < targetM) guess += 86400000;
    }
    return guess;
  };

  const dayStart = findMidnight(y, m, d);
  const nextDay = new Date(y, m - 1, d + 1);
  const dayEnd = findMidnight(
    nextDay.getFullYear(),
    nextDay.getMonth() + 1,
    nextDay.getDate(),
  );
  return [dayStart, dayEnd];
}

function formatTimeMs(ms: number, tz: string): string {
  return new Date(ms).toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    timeZone: tz,
  });
}

function formatDateDisplay(dateStr: string): string {
  const [y, m, d] = dateStr.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    weekday: "long",
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

export const DailyStats = memo(function DailyStats({
  userId,
  timezone,
}: {
  userId: string;
  timezone: string;
}) {
  const [selectedDate, setSelectedDate] = useState(() => todayInTz(timezone));

  const today = todayInTz(timezone);
  const isToday = selectedDate === today;

  const {
    data: dailyStats,
    refetch: refetchStats,
    isFetching: isFetchingStats,
  } = useQuery({
    queryKey: ["daily-stats", userId, timezone, selectedDate],
    queryFn: () => getDailyStats(userId, selectedDate),
    refetchOnWindowFocus: false,
  });

  const {
    data: records,
    isLoading,
    isError,
    error,
    refetch: refetchRecords,
    isFetching: isFetchingRecords,
  } = useQuery({
    queryKey: ["daily-minute-stats", userId, timezone, selectedDate],
    queryFn: () => getMinuteStatsByDate(userId, selectedDate),
    refetchOnWindowFocus: false,
  });

  const isRefreshing = isFetchingStats || isFetchingRecords;
  const handleRefresh = () => {
    refetchStats();
    refetchRecords();
  };

  const todayStats = dailyStats ?? undefined;

  const [dayStartMs, dayEndMs] = useMemo(
    () => getDateBoundsMs(selectedDate, timezone),
    [selectedDate, timezone],
  );

  const dayTicks = useMemo(() => {
    const ticks: number[] = [];
    for (let t = dayStartMs; t <= dayEndMs; t += 3 * 3600 * 1000) {
      ticks.push(t);
    }
    return ticks;
  }, [dayStartMs, dayEndMs]);

  const chartData = useMemo(
    () =>
      (records ?? [])
        .slice()
        .sort((a, b) => a.timestamp - b.timestamp)
        .map((r) => ({
          time: r.timestamp * 1000,
          avg_bpm: Math.round(r.avg_bpm * 10) / 10,
          min_bpm: r.min_bpm,
          max_bpm: r.max_bpm,
          band: r.max_bpm - r.min_bpm,
        })),
    [records],
  );

  const goToPrevDay = () => {
    setSelectedDate((d) => addDays(d, -1));
  };

  const goToNextDay = () => {
    if (!isToday) {
      setSelectedDate((d) => addDays(d, 1));
    }
  };

  return (
    <div className="border border-gray-800 rounded-lg p-4">
      {/* Day navigator */}
      <div className="flex items-center justify-between mb-4">
        <button
          onClick={goToPrevDay}
          className="px-3 py-1.5 rounded bg-gray-800 text-gray-400 hover:bg-gray-700"
        >
          &larr;
        </button>
        <div className="flex items-center gap-3">
          <span className="text-sm font-medium">
            {formatDateDisplay(selectedDate)}
          </span>
          <input
            type="date"
            value={selectedDate}
            max={today}
            onChange={(e) => {
              if (e.target.value) {
                setSelectedDate(e.target.value);
              }
            }}
            className="bg-gray-800 border border-gray-700 rounded px-2 py-1 text-sm text-white [color-scheme:dark]"
          />
        </div>
        <div className="flex items-center gap-2">
          {isToday && (
            <button
              onClick={handleRefresh}
              disabled={isRefreshing}
              className="px-3 py-1.5 rounded bg-gray-800 text-gray-400 hover:bg-gray-700 disabled:opacity-50"
              title="Reload"
            >
              <svg
                className={`w-4 h-4 ${isRefreshing ? "animate-spin" : ""}`}
                fill="none"
                stroke="currentColor"
                viewBox="0 0 24 24"
              >
                <path
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  strokeWidth={2}
                  d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15"
                />
              </svg>
            </button>
          )}
          <button
            onClick={goToNextDay}
            disabled={isToday}
            className={`px-3 py-1.5 rounded ${
              isToday
                ? "bg-gray-900 text-gray-600 cursor-not-allowed"
                : "bg-gray-800 text-gray-400 hover:bg-gray-700"
            }`}
          >
            &rarr;
          </button>
        </div>
      </div>

      {/* Stats cards */}
      <div className="grid grid-cols-3 gap-3 mb-4">
        <div className="bg-gray-800 rounded-lg p-3 text-center">
          <div className="text-xs text-gray-400 mb-1">Average</div>
          <div className="text-2xl font-mono font-bold text-red-400">
            {todayStats ? todayStats.avg_bpm.toFixed(1) : "--"}
          </div>
          <div className="text-xs text-gray-500">BPM</div>
        </div>
        <div className="bg-gray-800 rounded-lg p-3 text-center">
          <div className="text-xs text-gray-400 mb-1">Min</div>
          <div className="text-2xl font-mono font-bold text-blue-400">
            {todayStats ? todayStats.min_bpm : "--"}
          </div>
          <div className="text-xs text-gray-500">BPM</div>
        </div>
        <div className="bg-gray-800 rounded-lg p-3 text-center">
          <div className="text-xs text-gray-400 mb-1">Max</div>
          <div className="text-2xl font-mono font-bold text-orange-400">
            {todayStats ? todayStats.max_bpm : "--"}
          </div>
          <div className="text-xs text-gray-500">BPM</div>
        </div>
      </div>
      {todayStats && (
        <div className="text-xs text-gray-500 text-center mb-4">
          {todayStats.count.toLocaleString()} records
        </div>
      )}

      {/* 24h chart */}
      {isLoading ? (
        <div className="p-8 text-center text-gray-500">Loading...</div>
      ) : isError ? (
        <div className="p-8 text-center text-red-400">
          Failed to load chart data: {(error as Error)?.message}
        </div>
      ) : chartData.length === 0 ? (
        <div className="p-8 text-center text-gray-500">
          No heart rate data for this day
        </div>
      ) : (
        <ResponsiveContainer width="100%" height={300}>
          <ComposedChart data={chartData}>
            <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
            <XAxis
              dataKey="time"
              type="number"
              domain={[dayStartMs, dayEndMs]}
              ticks={dayTicks}
              tickFormatter={(ms: number) => formatTimeMs(ms, timezone)}
              stroke="#9CA3AF"
              fontSize={12}
            />
            <YAxis domain={[40, 200]} stroke="#9CA3AF" fontSize={12} />
            <Tooltip
              labelFormatter={(ms) => formatTimeMs(Number(ms), timezone)}
              contentStyle={{
                backgroundColor: "#1F2937",
                border: "1px solid #374151",
                borderRadius: "8px",
              }}
              formatter={(value, name) => {
                const labels: Record<string, string> = {
                  avg_bpm: "Avg",
                  min_bpm: "Min",
                  max_bpm: "Max",
                };
                if (labels[name as string]) return [value, labels[name as string]];
                return [undefined, undefined];
              }}
            />
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
          </ComposedChart>
        </ResponsiveContainer>
      )}
    </div>
  );
});
