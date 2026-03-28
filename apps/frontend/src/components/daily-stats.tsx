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

function getDayBounds(date: Date): { from: number; to: number } {
  const start = new Date(date.getFullYear(), date.getMonth(), date.getDate());
  const end = new Date(
    date.getFullYear(),
    date.getMonth(),
    date.getDate() + 1
  );
  return {
    from: Math.floor(start.getTime() / 1000),
    to: Math.floor(end.getTime() / 1000),
  };
}

function formatDate(date: Date): string {
  return date.toLocaleDateString(undefined, {
    weekday: "long",
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}

function toInputDate(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, "0");
  const d = String(date.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

function isToday(date: Date): boolean {
  const now = new Date();
  return (
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()
  );
}

export function DailyStats({ userId }: { userId: string }) {
  const [selectedDate, setSelectedDate] = useState(
    () => new Date(new Date().getFullYear(), new Date().getMonth(), new Date().getDate())
  );

  const { from, to } = getDayBounds(selectedDate);

  // Fetch stats for 31-day window around selected date
  const statsFrom = from - 15 * 86400;
  const statsTo = to + 15 * 86400;

  const { data: dailyStats } = useQuery({
    queryKey: ["daily-stats", userId, statsFrom, statsTo],
    queryFn: () => getDailyStats(userId, statsFrom, statsTo),
  });

  const { data: records } = useQuery({
    queryKey: ["daily-heart-rates", userId, from, to],
    queryFn: () => getHeartRates(userId, { from, to, limit: 2880 }),
  });

  const todayStats = dailyStats?.find((s) => s.day === from);

  const chartData = (records ?? [])
    .slice()
    .sort((a, b) => a.timestamp - b.timestamp)
    .map((r) => ({
      time: new Date(r.timestamp * 1000).toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
      }),
      bpm: r.bpm,
    }));

  const goToPrevDay = () => {
    setSelectedDate(
      (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate() - 1)
    );
  };

  const goToNextDay = () => {
    if (!isToday(selectedDate)) {
      setSelectedDate(
        (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1)
      );
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
            {formatDate(selectedDate)}
          </span>
          <input
            type="date"
            value={toInputDate(selectedDate)}
            max={toInputDate(new Date())}
            onChange={(e) => {
              if (e.target.value) {
                const [y, m, d] = e.target.value.split("-").map(Number);
                setSelectedDate(new Date(y, m - 1, d));
              }
            }}
            className="bg-gray-800 border border-gray-700 rounded px-2 py-1 text-sm text-white [color-scheme:dark]"
          />
        </div>
        <button
          onClick={goToNextDay}
          disabled={isToday(selectedDate)}
          className={`px-3 py-1.5 rounded ${
            isToday(selectedDate)
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
      {chartData.length === 0 ? (
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
