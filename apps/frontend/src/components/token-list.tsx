"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getTokens, updateToken, deleteToken } from "@/lib/api";

export function TokenList({ userId }: { userId: string }) {
  const queryClient = useQueryClient();

  const { data: tokens } = useQuery({
    queryKey: ["tokens", userId],
    queryFn: () => getTokens(userId),
  });

  const toggleMutation = useMutation({
    mutationFn: ({ id, is_active }: { id: string; is_active: boolean }) =>
      updateToken(id, { is_active }),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["tokens", userId] }),
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => deleteToken(id),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["tokens", userId] }),
  });

  if (!tokens || tokens.length === 0) {
    return <p className="text-gray-500">No tokens configured</p>;
  }

  return (
    <div className="space-y-3">
      {tokens.map((token) => (
        <div
          key={token.id}
          className="border border-gray-800 rounded-lg p-4 flex items-center justify-between"
        >
          <div>
            <div className="font-medium">
              {token.label || "Unnamed token"}
            </div>
            <div className="text-sm text-gray-400 mt-1">
              {token.is_active ? (
                <span className="text-green-400">Active</span>
              ) : (
                <span className="text-gray-500">Inactive</span>
              )}
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
          <div className="flex gap-2">
            <button
              onClick={() =>
                toggleMutation.mutate({
                  id: token.id,
                  is_active: !token.is_active,
                })
              }
              className="px-3 py-1 rounded text-sm border border-gray-700 hover:bg-gray-800"
            >
              {token.is_active ? "Disable" : "Enable"}
            </button>
            <button
              onClick={() => {
                if (confirm("Delete this token?")) {
                  deleteMutation.mutate(token.id);
                }
              }}
              className="px-3 py-1 rounded text-sm border border-red-800 text-red-400 hover:bg-red-900/30"
            >
              Delete
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
