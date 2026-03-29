"use client";

import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { getDailyStats, getHeartRates } from "@/lib/api";
import {
  LineChart,
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

function formatDateDisplay(dateStr: string): string {
  const [y, m, d] = dateStr.split("-").map(Number);
  return new Date(y, m - 1, d).toLocaleDateString(undefined, {
    weekday: "long",
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

export function DailyStats({
  userId,
  timezone,
}: {
  userId: string;
  timezone: string;
}) {
  const [selectedDate, setSelectedDate] = useState(() => todayInTz(timezone));

  const statsFrom = addDays(selectedDate, -15);
  const statsTo = addDays(selectedDate, 16);

  const today = todayInTz(timezone);
  const isToday = selectedDate === today;

  const { data: dailyStats } = useQuery({
    queryKey: ["daily-stats", userId, timezone, statsFrom, statsTo],
    queryFn: () => getDailyStats(userId, statsFrom, statsTo),
  });

  const { data: records, isLoading, isError, error } = useQuery({
    queryKey: ["daily-heart-rates", userId, timezone, selectedDate],
    queryFn: () => getHeartRates(userId, { date: selectedDate, limit: 2880 }),
  });

  const todayStats = dailyStats?.find((s) => s.day === selectedDate);

  const chartData = (records ?? [])
    .slice()
    .sort((a, b) => a.timestamp - b.timestamp)
    .map((r) => ({
      time: new Date(r.timestamp * 1000).toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
        timeZone: timezone,
      }),
      bpm: r.bpm,
    }));

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
          <LineChart data={chartData}>
            <CartesianGrid strokeDasharray="3 3" stroke="#374151" />
            <XAxis dataKey="time" stroke="#9CA3AF" fontSize={12} />
            <YAxis domain={[40, 200]} stroke="#9CA3AF" fontSize={12} />
            <Tooltip
              contentStyle={{
                backgroundColor: "#1F2937",
                border: "1px solid #374151",
                borderRadius: "8px",
              }}
            />
            <Line
              type="monotone"
              dataKey="bpm"
              stroke="#EF4444"
              strokeWidth={2}
              dot={false}
            />
          </LineChart>
        </ResponsiveContainer>
      )}
    </div>
  );
}
