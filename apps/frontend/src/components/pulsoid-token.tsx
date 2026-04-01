"use client";

import { useState, useEffect } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getPulsoidToken, createPulsoidConnect, setManualPulsoidToken, deletePulsoidToken } from "@/lib/api";

const RESULT_MESSAGES: Record<string, { text: string; color: string }> = {
  authorized: { text: "Pulsoid authorized. Connecting...", color: "text-blue-400" },
  denied: { text: "Pulsoid authorization was denied.", color: "text-yellow-400" },
  exchange_failed: { text: "Connection failed. Please try again.", color: "text-red-400" },
  invalid_state: { text: "Security verification failed. Please try again.", color: "text-red-400" },
};

export function PulsoidToken({
  userId,
  oauthResult,
}: {
  userId: string;
  oauthResult?: string | null;
}) {
  const queryClient = useQueryClient();
  const [manualToken, setManualToken] = useState("");

  // Clear the query param from URL after displaying
  useEffect(() => {
    if (oauthResult) {
      window.history.replaceState({}, "", "/settings");
    }
  }, [oauthResult]);

  const { data: token, isLoading } = useQuery({
    queryKey: ["pulsoid-token", userId],
    queryFn: () => getPulsoidToken(userId),
    refetchOnWindowFocus: false,
  });

  const connectMutation = useMutation({
    mutationFn: () => createPulsoidConnect("/settings"),
    onSuccess: (data) => {
      window.location.href = `/api/oauth/pulsoid/connect/${data.request_id}`;
    },
  });

  const manualMutation = useMutation({
    mutationFn: (accessToken: string) => setManualPulsoidToken(userId, accessToken),
    onSuccess: () => {
      setManualToken("");
      queryClient.invalidateQueries({ queryKey: ["pulsoid-token", userId] });
      queryClient.invalidateQueries({ queryKey: ["users"] });
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

  const resultMessage = oauthResult ? RESULT_MESSAGES[oauthResult] : null;

  // Token not configured — show connect options
  if (token === null || token === undefined) {
    return (
      <div className="border border-gray-800 rounded-lg p-4 space-y-4">
        {resultMessage && (
          <p className={`text-sm ${resultMessage.color}`}>{resultMessage.text}</p>
        )}
        <p className="text-gray-500 text-sm">No Pulsoid connection configured</p>

        {/* OAuth connect */}
        <div>
          {connectMutation.error && (
            <p className="text-red-400 text-sm mb-2">{connectMutation.error.message}</p>
          )}
          <button
            onClick={() => connectMutation.mutate()}
            disabled={connectMutation.isPending}
            className="bg-green-600 hover:bg-green-700 px-4 py-2 rounded text-sm disabled:opacity-50"
          >
            {connectMutation.isPending ? "Connecting..." : "Connect Pulsoid (OAuth)"}
          </button>
        </div>

        <div className="flex items-center gap-3">
          <div className="flex-1 border-t border-gray-800" />
          <span className="text-gray-600 text-xs">or</span>
          <div className="flex-1 border-t border-gray-800" />
        </div>

        {/* Manual token input */}
        <div className="space-y-2">
          <label className="text-gray-400 text-sm">Manual access token</label>
          {manualMutation.error && (
            <p className="text-red-400 text-sm">{manualMutation.error.message}</p>
          )}
          <div className="flex gap-2">
            <input
              type="password"
              value={manualToken}
              onChange={(e) => setManualToken(e.target.value)}
              placeholder="Paste your Pulsoid access token"
              className="flex-1 bg-gray-900 border border-gray-700 rounded px-3 py-2 text-sm focus:outline-none focus:border-gray-500"
            />
            <button
              onClick={() => manualMutation.mutate(manualToken)}
              disabled={manualMutation.isPending || !manualToken.trim()}
              className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm disabled:opacity-50"
            >
              {manualMutation.isPending ? "Saving..." : "Save"}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // Token configured — show status
  const sourceLabel = token.source === "oauth" ? "OAuth" : "Manual token";

  return (
    <div className="border border-gray-800 rounded-lg p-4 space-y-3">
      {resultMessage && (
        <p className={`text-sm ${resultMessage.color}`}>{resultMessage.text}</p>
      )}
      <div className="flex items-center justify-between">
        <div>
          <span className="text-xs text-gray-500 bg-gray-800 px-2 py-0.5 rounded mr-2">
            {sourceLabel}
          </span>
          <span className="text-sm text-gray-400">
            {token.last_error ? (
              <span className="text-red-400">Error: {token.last_error}</span>
            ) : token.last_connected_at ? (
              <>
                <span className="text-green-400">Connected</span>
                <span className="ml-3">
                  Last connected:{" "}
                  {new Date(token.last_connected_at * 1000).toLocaleString()}
                </span>
              </>
            ) : (
              <span className="text-yellow-400">Connecting...</span>
            )}
          </span>
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
