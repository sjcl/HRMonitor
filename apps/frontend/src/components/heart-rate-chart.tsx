"use client";

import { useQuery } from "@tanstack/react-query";
import { getHeartRates } from "@/lib/api";
import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  ResponsiveContainer,
} from "recharts";

export function HeartRateChart({ userId }: { userId: string }) {
  const { data: records } = useQuery({
    queryKey: ["heart-rates", userId],
    queryFn: () => {
      const from = Math.floor(Date.now() / 1000) - 3600; // last 1 hour
      return getHeartRates(userId, { from, limit: 500 });
    },
    refetchInterval: 5000,
  });

  const chartData = (records ?? [])
    .slice()
    .sort((a, b) => a.timestamp - b.timestamp)
    .map((r) => ({
      time: new Date(r.timestamp * 1000).toLocaleTimeString([], {
        hour: "2-digit",
        minute: "2-digit",
        second: "2-digit",
      }),
      bpm: r.bpm,
    }));

  if (chartData.length === 0) {
    return (
      <div className="border border-gray-800 rounded-lg p-8 text-center text-gray-500">
        No heart rate data in the last hour
      </div>
    );
  }

  return (
    <div className="border border-gray-800 rounded-lg p-4">
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
    </div>
  );
}
