"use client";

import { useSession } from "next-auth/react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { getUser, updateUser, type HeartRateVisibility } from "@/lib/api";
import { PulsoidToken } from "@/components/pulsoid-token";
import { TimezoneSelect } from "@/components/timezone-select";
import { useSearchParams } from "next/navigation";
import { useState, useEffect, Suspense } from "react";

export default function SettingsPage() {
  return (
    <Suspense>
      <SettingsContent />
    </Suspense>
  );
}

function SettingsContent() {
  const { data: session, status } = useSession();
  const userId = session?.user?.id;
  const queryClient = useQueryClient();
  const searchParams = useSearchParams();
  const oauthResult = searchParams.get("pulsoid");

  const { data: user } = useQuery({
    queryKey: ["user", userId],
    queryFn: () => getUser(userId!),
    enabled: !!userId,
  });

  const [editName, setEditName] = useState("");
  const [editTimezone, setEditTimezone] = useState("");
  const [editVisibility, setEditVisibility] =
    useState<HeartRateVisibility>("group_default");

  useEffect(() => {
    if (user) {
      setEditName(user.display_name);
      setEditTimezone(user.timezone);
      setEditVisibility(user.heart_rate_visibility);
    }
  }, [user]);

  const updateMutation = useMutation({
    mutationFn: (data: {
      display_name?: string;
      timezone?: string;
      heart_rate_visibility?: HeartRateVisibility;
    }) => updateUser(userId!, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["user", userId] });
      queryClient.invalidateQueries({ queryKey: ["users"] });
    },
  });

  const hasChanges =
    user &&
    (editName !== user.display_name ||
      editTimezone !== user.timezone ||
      editVisibility !== user.heart_rate_visibility);

  if (status === "loading") return null;
  if (!userId) return null;

  return (
    <div>
      <h1 className="text-2xl font-bold mb-6">Settings</h1>

      <section className="mb-8">
        <h2 className="text-lg font-semibold mb-3">Profile</h2>
        <div className="flex flex-col gap-3 max-w-md">
          <div>
            <label className="block text-sm text-gray-400 mb-1">Name</label>
            <input
              type="text"
              value={editName}
              onChange={(e) => setEditName(e.target.value)}
              className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full"
            />
          </div>
          <div>
            <label className="block text-sm text-gray-400 mb-1">Timezone</label>
            <TimezoneSelect
              value={editTimezone}
              onChange={setEditTimezone}
              className="w-full"
            />
          </div>
          <div>
            <label className="block text-sm text-gray-400 mb-1">
              心拍データの公開範囲
            </label>
            <select
              value={editVisibility}
              onChange={(e) =>
                setEditVisibility(e.target.value as HeartRateVisibility)
              }
              className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full"
            >
              <option value="group_default">グループごとの設定に準拠</option>
              <option value="private">グループごとの設定にかかわらず非公開</option>
            </select>
            <p className="text-xs text-gray-500 mt-1">
              「グループごとの設定に準拠」を選択すると、所属するグループごとの共有設定に従います。「非公開」を選択すると、グループの設定にかかわらず自分だけが閲覧できます。
            </p>
          </div>
          {hasChanges && (
            <button
              onClick={() =>
                updateMutation.mutate({
                  display_name: editName.trim(),
                  timezone: editTimezone,
                  heart_rate_visibility: editVisibility,
                })
              }
              disabled={updateMutation.isPending || !editName.trim()}
              className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm disabled:opacity-50 self-start"
            >
              {updateMutation.isPending ? "Saving..." : "Save"}
            </button>
          )}
        </div>
      </section>

      <section className="mb-8">
        <h2 className="text-lg font-semibold mb-3">Pulsoid</h2>
        <PulsoidToken userId={userId} oauthResult={oauthResult} />
      </section>
    </div>
  );
}
