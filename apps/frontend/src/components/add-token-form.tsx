"use client";

import { useMutation, useQueryClient } from "@tanstack/react-query";
import { createToken } from "@/lib/api";
import { useState } from "react";

export function AddTokenForm({ userId }: { userId: string }) {
  const queryClient = useQueryClient();
  const [label, setLabel] = useState("");
  const [accessToken, setAccessToken] = useState("");
  const [open, setOpen] = useState(false);

  const mutation = useMutation({
    mutationFn: () =>
      createToken(userId, {
        label: label.trim() || undefined,
        access_token: accessToken.trim(),
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["tokens", userId] });
      setLabel("");
      setAccessToken("");
      setOpen(false);
    },
  });

  if (!open) {
    return (
      <button
        onClick={() => setOpen(true)}
        className="text-sm text-blue-400 hover:underline"
      >
        + Add Token
      </button>
    );
  }

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        if (accessToken.trim()) mutation.mutate();
      }}
      className="border border-gray-800 rounded-lg p-4 space-y-3"
    >
      <input
        type="text"
        value={label}
        onChange={(e) => setLabel(e.target.value)}
        placeholder="Label (optional)"
        className="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm"
      />
      <input
        type="password"
        value={accessToken}
        onChange={(e) => setAccessToken(e.target.value)}
        placeholder="Pulsoid Access Token"
        className="w-full bg-gray-800 border border-gray-700 rounded px-3 py-2 text-sm"
        required
      />
      {mutation.error && (
        <p className="text-red-400 text-sm">{mutation.error.message}</p>
      )}
      <div className="flex gap-2">
        <button
          type="submit"
          disabled={mutation.isPending}
          className="bg-green-600 hover:bg-green-700 px-4 py-2 rounded text-sm disabled:opacity-50"
        >
          Add
        </button>
        <button
          type="button"
          onClick={() => setOpen(false)}
          className="px-4 py-2 rounded text-sm border border-gray-700 hover:bg-gray-800"
        >
          Cancel
        </button>
      </div>
    </form>
  );
}
