"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getUsers, createUser } from "@/lib/api";
import { useState } from "react";
import Link from "next/link";
import { AllUsersHeartRateChart } from "@/components/all-users-heart-rate-chart";
import { TimezoneSelect, getBrowserTimezone } from "@/components/timezone-select";

export default function UsersPage() {
  const queryClient = useQueryClient();
  const [newName, setNewName] = useState("");
  const [newTimezone, setNewTimezone] = useState(getBrowserTimezone);
  const [showForm, setShowForm] = useState(false);

  const { data: users, isLoading } = useQuery({
    queryKey: ["users"],
    queryFn: getUsers,
    refetchInterval: 5000,
  });

  const createMutation = useMutation({
    mutationFn: ({ name, timezone }: { name: string; timezone: string }) =>
      createUser(name, timezone),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["users"] });
      setNewName("");
      setNewTimezone(getBrowserTimezone());
      setShowForm(false);
    },
  });

  return (
    <div>
      <div className="flex items-center justify-between mb-6">
        <h1 className="text-2xl font-bold">Users</h1>
        <button
          onClick={() => setShowForm(!showForm)}
          className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm"
        >
          Add User
        </button>
      </div>

      {showForm && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            if (newName.trim())
              createMutation.mutate({ name: newName.trim(), timezone: newTimezone });
          }}
          className="mb-6 flex flex-col gap-2"
        >
          <div className="flex gap-2">
            <input
              type="text"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder="User name"
              className="bg-gray-800 border border-gray-700 rounded px-3 py-2 flex-1"
              autoFocus
            />
            <button
              type="submit"
              disabled={createMutation.isPending}
              className="bg-green-600 hover:bg-green-700 px-4 py-2 rounded text-sm disabled:opacity-50"
            >
              Create
            </button>
          </div>
          <TimezoneSelect value={newTimezone} onChange={setNewTimezone} />
        </form>
      )}

      <AllUsersHeartRateChart />

      {isLoading ? (
        <p className="text-gray-400">Loading...</p>
      ) : (
        <div className="border border-gray-800 rounded-lg overflow-hidden">
          <table className="w-full">
            <thead className="bg-gray-900">
              <tr>
                <th className="text-left px-4 py-3 text-sm text-gray-400">Name</th>
                <th className="text-right px-4 py-3 text-sm text-gray-400">Latest BPM</th>
                <th className="text-right px-4 py-3 text-sm text-gray-400">Pulsoid</th>
              </tr>
            </thead>
            <tbody>
              {users?.map((user) => (
                <tr
                  key={user.id}
                  className="border-t border-gray-800 hover:bg-gray-900/50"
                >
                  <td className="px-4 py-3">
                    <Link
                      href={`/users/${user.id}`}
                      className="text-blue-400 hover:underline"
                    >
                      {user.name}
                    </Link>
                  </td>
                  <td className="px-4 py-3 text-right">
                    <BpmBadge bpm={user.latest_bpm} />
                  </td>
                  <td className="px-4 py-3 text-right">
                    {user.has_pulsoid_token ? (
                      <span className="inline-block w-2 h-2 rounded-full bg-green-400" title="Connected" />
                    ) : (
                      <span className="text-gray-600">--</span>
                    )}
                  </td>
                </tr>
              ))}
              {users?.length === 0 && (
                <tr>
                  <td colSpan={3} className="px-4 py-8 text-center text-gray-500">
                    No users yet
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function BpmBadge({ bpm }: { bpm: number | null }) {
  if (bpm === null) return <span className="text-gray-600">--</span>;

  const color =
    bpm >= 60 && bpm <= 100
      ? "text-green-400"
      : bpm > 100
        ? "text-yellow-400"
        : "text-yellow-400";

  return <span className={`font-mono font-bold ${color}`}>{bpm}</span>;
}
