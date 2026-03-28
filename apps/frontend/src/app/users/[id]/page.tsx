"use client";

import { useQuery } from "@tanstack/react-query";
import { getLatestHeartRate, getUsers } from "@/lib/api";
import { HeartRateChart } from "@/components/heart-rate-chart";
import { DailyStats } from "@/components/daily-stats";
import { TokenList } from "@/components/token-list";
import { AddTokenForm } from "@/components/add-token-form";
import { use } from "react";
import Link from "next/link";

export default function UserDetailPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);

  const { data: users } = useQuery({
    queryKey: ["users"],
    queryFn: getUsers,
  });

  const user = users?.find((u) => u.id === id);

  const { data: latestHr } = useQuery({
    queryKey: ["latest-hr", id],
    queryFn: () => getLatestHeartRate(id).catch(() => null),
    refetchInterval: 5000,
  });

  return (
    <div>
      <Link href="/users" className="text-sm text-gray-400 hover:underline">
        &larr; Back to users
      </Link>

      <div className="flex items-center gap-4 mt-4 mb-6">
        <h1 className="text-2xl font-bold">{user?.name ?? "Loading..."}</h1>
        {latestHr && (
          <span className="text-3xl font-mono font-bold text-red-400">
            {latestHr.bpm} BPM
          </span>
        )}
      </div>

      <section className="mb-8">
        <h2 className="text-lg font-semibold mb-3">Heart Rate</h2>
        <HeartRateChart userId={id} />
      </section>

      <section className="mb-8">
        <h2 className="text-lg font-semibold mb-3">Daily Statistics</h2>
        <DailyStats userId={id} />
      </section>

      <section>
        <h2 className="text-lg font-semibold mb-3">Pulsoid Tokens</h2>
        <TokenList userId={id} />
        <div className="mt-3">
          <AddTokenForm userId={id} />
        </div>
      </section>
    </div>
  );
}
