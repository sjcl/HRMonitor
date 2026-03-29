"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getPulsoidToken, setPulsoidToken, deletePulsoidToken } from "@/lib/api";
import { useState } from "react";

export function PulsoidToken({ userId }: { userId: string }) {
  const queryClient = useQueryClient();
  const [accessToken, setAccessToken] = useState("");

  const { data: token, isLoading } = useQuery({
    queryKey: ["pulsoid-token", userId],
    queryFn: () => getPulsoidToken(userId),
    refetchInterval: 5000,
  });

  const connectMutation = useMutation({
    mutationFn: () => setPulsoidToken(userId, accessToken.trim()),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["pulsoid-token", userId] });
      queryClient.invalidateQueries({ queryKey: ["users"] });
      setAccessToken("");
    },
  });

  const disconnectMutation = useMutation({
    mutationFn: () => deletePulsoidToken(userId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["pulsoid-token", userId] });
      queryClient.invalidateQueries({ queryKey: ["users"] });
    },
  });

  if (isLoading) {
    return <p className="text-gray-500">Loading...</p>;
  }

  // Token not configured — show connect form
  if (token === null || token === undefined) {
    return (
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (accessToken.trim()) connectMutation.mutate();
        }}
        className="border border-gray-800 rounded-lg p-4 space-y-3"
      >
        <p className="text-gray-500 text-sm">No Pulsoid token configured</p>
        <input
          type="password"
          value={accessToken}
          onChange={(e) => setAccessToken(e.target.value)}
          placeholder="Pulsoid Access Token"
          className="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm"
          required
        />
        {connectMutation.error && (
          <p className="text-red-400 text-sm">
            {connectMutation.error.message}
          </p>
        )}
        <button
          type="submit"
          disabled={connectMutation.isPending}
          className="bg-green-600 hover:bg-green-700 px-4 py-2 rounded text-sm disabled:opacity-50"
        >
          Connect
        </button>
      </form>
    );
  }

  // Token configured — show status
  return (
    <div className="border border-gray-800 rounded-lg p-4">
      <div className="flex items-center justify-between">
        <div>
          <div className="text-sm text-gray-400">
            <span className="text-green-400">Connected</span>
            {token.last_connected_at && (
              <span className="ml-3">
                Last connected:{" "}
                {new Date(token.last_connected_at * 1000).toLocaleString()}
              </span>
            )}
          </div>
          {token.last_error && (
            <div className="text-sm text-red-400 mt-1">
              Error: {token.last_error}
            </div>
          )}
        </div>
        <button
          onClick={() => {
            if (confirm("Disconnect Pulsoid?")) {
              disconnectMutation.mutate();
            }
          }}
          disabled={disconnectMutation.isPending}
          className="px-3 py-1 rounded text-sm border border-red-800 text-red-400 hover:bg-red-900/30 disabled:opacity-50"
        >
          Disconnect
        </button>
      </div>
    </div>
  );
}
