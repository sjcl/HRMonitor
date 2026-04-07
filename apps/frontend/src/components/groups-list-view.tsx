"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  getGroups,
  createGroup,
  updateMyMembership,
  formatGroupDisplayName,
} from "@/lib/groups-api";
import { useState } from "react";
import Link from "next/link";

export function GroupsListView() {
  const queryClient = useQueryClient();
  const [showCreate, setShowCreate] = useState(false);
  const [newName, setNewName] = useState("");
  const [newPolicy, setNewPolicy] = useState("group");

  const { data: groups, isLoading } = useQuery({
    queryKey: ["groups"],
    queryFn: getGroups,
  });

  const createMutation = useMutation({
    mutationFn: () =>
      createGroup({
        name: newName.trim() || undefined,
        invite_policy: newPolicy,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["groups"] });
      setShowCreate(false);
      setNewName("");
      setNewPolicy("group");
    },
  });

  const sharingMutation = useMutation({
    mutationFn: ({ groupId, sharing }: { groupId: string; sharing: boolean }) =>
      updateMyMembership(groupId, sharing),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["groups"] });
    },
  });

  return (
    <div className="mb-8">
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">グループ</h2>
        <button
          onClick={() => setShowCreate(!showCreate)}
          className="bg-blue-600 hover:bg-blue-700 px-3 py-1.5 rounded text-sm"
        >
          グループを作成
        </button>
      </div>

      {showCreate && (
        <div className="bg-gray-900 border border-gray-700 rounded-lg p-4 mb-4 max-w-md">
          <div className="flex flex-col gap-3">
            <div>
              <label className="block text-sm text-gray-400 mb-1">
                グループ名（任意）
              </label>
              <input
                type="text"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="未設定の場合、メンバー名が表示されます"
                className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full text-sm"
              />
            </div>
            <div>
              <label className="block text-sm text-gray-400 mb-1">
                招待ポリシー
              </label>
              <select
                value={newPolicy}
                onChange={(e) => setNewPolicy(e.target.value)}
                className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full text-sm"
              >
                <option value="group">オーナーのみ招待可能</option>
                <option value="group+">メンバー全員が招待可能</option>
              </select>
            </div>
            <div className="flex gap-2">
              <button
                onClick={() => createMutation.mutate()}
                disabled={createMutation.isPending}
                className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm disabled:opacity-50"
              >
                {createMutation.isPending ? "作成中..." : "作成"}
              </button>
              <button
                onClick={() => setShowCreate(false)}
                className="bg-gray-700 hover:bg-gray-600 px-4 py-2 rounded text-sm"
              >
                キャンセル
              </button>
            </div>
          </div>
        </div>
      )}

      {isLoading && <p className="text-gray-400">読み込み中...</p>}

      {groups && groups.length === 0 && (
        <p className="text-gray-400">
          グループに参加していません。グループを作成するか、招待リンクから参加してください。
        </p>
      )}

      {groups && groups.length > 0 && (
        <div className="flex flex-col gap-3">
          {groups.map((group) => (
            <Link
              key={group.id}
              href={`/groups/${group.id}`}
              className="bg-gray-900 border border-gray-700 rounded-lg p-4 hover:border-gray-500 transition-colors block"
            >
              <div className="flex items-center justify-between">
                <div>
                  <h3 className="font-medium">
                    {formatGroupDisplayName(group.display_name, group.name, group.member_count)}
                  </h3>
                  <p className="text-sm text-gray-400 mt-1">
                    {group.member_count}人のメンバー ・{" "}
                    {group.my_role === "owner" ? "オーナー" : "メンバー"} ・{" "}
                    {group.invite_policy === "group+"
                      ? "全員招待可"
                      : "オーナーのみ招待可"}
                  </p>
                </div>
                <div className="flex items-center gap-3">
                  <label
                    className="flex items-center gap-2 text-sm"
                    onClick={(e) => e.preventDefault()}
                  >
                    <span className="text-gray-400">共有</span>
                    <button
                      onClick={(e) => {
                        e.preventDefault();
                        sharingMutation.mutate({
                          groupId: group.id,
                          sharing: !group.my_sharing,
                        });
                      }}
                      className={`w-10 h-6 rounded-full transition-colors relative ${
                        group.my_sharing ? "bg-blue-600" : "bg-gray-600"
                      }`}
                    >
                      <span
                        className={`absolute top-1 w-4 h-4 rounded-full bg-white transition-transform ${
                          group.my_sharing ? "left-5" : "left-1"
                        }`}
                      />
                    </button>
                  </label>
                </div>
              </div>
            </Link>
          ))}
        </div>
      )}
    </div>
  );
}
