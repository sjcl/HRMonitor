"use client";

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  getGroups,
  createGroup,
  createInvite,
  updateMyMembership,
  formatGroupDisplayName,
} from "@/lib/groups-api";
import { useState, useEffect, useCallback } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { UserAvatar } from "@/components/user-avatar";

type ModalStep = null | "choose" | "group" | "personal-pending" | "personal-result";

export function GroupsListView() {
  const queryClient = useQueryClient();
  const router = useRouter();
  const [modalStep, setModalStep] = useState<ModalStep>(null);
  const [newName, setNewName] = useState("");
  const [newPolicy, setNewPolicy] = useState("group");
  const [inviteUrl, setInviteUrl] = useState("");
  const [createdGroupId, setCreatedGroupId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const { data: groups, isLoading } = useQuery({
    queryKey: ["groups"],
    queryFn: getGroups,
  });

  const closeModal = useCallback(() => {
    const groupId = createdGroupId;
    setModalStep(null);
    setNewName("");
    setNewPolicy("group");
    setInviteUrl("");
    setCopied(false);
    setError(null);
    setCreatedGroupId(null);
    if (groupId) {
      queryClient.invalidateQueries({ queryKey: ["groups"] });
      router.push(`/groups/${groupId}`);
    }
  }, [createdGroupId, queryClient, router]);

  useEffect(() => {
    if (!modalStep) return;
    const handleEsc = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    document.addEventListener("keydown", handleEsc);
    return () => document.removeEventListener("keydown", handleEsc);
  }, [modalStep, closeModal]);

  const handlePersonal = async () => {
    setModalStep("personal-pending");
    setError(null);
    try {
      const group = await createGroup({});
      setCreatedGroupId(group.id);
      const invite = await createInvite(group.id, {
        max_uses: 1,
        expires_in_hours: 24,
      });
      setInviteUrl(`${window.location.origin}/invite/${invite.token}`);
      setModalStep("personal-result");
      queryClient.invalidateQueries({ queryKey: ["groups"] });
    } catch {
      setError("作成に失敗しました。もう一度お試しください。");
      setModalStep("choose");
    }
  };

  const createGroupMutation = useMutation({
    mutationFn: () =>
      createGroup({
        name: newName.trim() || undefined,
        invite_policy: newPolicy,
      }),
    onSuccess: (group) => {
      setCreatedGroupId(group.id);
      queryClient.invalidateQueries({ queryKey: ["groups"] });
      setModalStep(null);
      setNewName("");
      setNewPolicy("group");
      router.push(`/groups/${group.id}`);
    },
  });

  const sharingMutation = useMutation({
    mutationFn: ({ groupId, sharing }: { groupId: string; sharing: boolean }) =>
      updateMyMembership(groupId, sharing),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["groups"] });
    },
  });

  const handleCopy = async () => {
    await navigator.clipboard.writeText(inviteUrl);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  };

  return (
    <div className="mb-8">
      <div className="flex items-center justify-between mb-4">
        <h2 className="text-lg font-semibold">グループ</h2>
        <button
          onClick={() => setModalStep("choose")}
          className="bg-blue-600 hover:bg-blue-700 px-3 py-1.5 rounded text-sm"
        >
          グループを作成
        </button>
      </div>

      {/* Modal overlay */}
      {modalStep && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
          onClick={(e) => {
            if (e.target === e.currentTarget) closeModal();
          }}
        >
          <div className="bg-gray-900 border border-gray-700 rounded-xl p-6 w-full max-w-md mx-4 shadow-2xl">
            {/* Step: Choose */}
            {modalStep === "choose" && (
              <>
                <h3 className="text-lg font-semibold text-center mb-6">
                  誰と共有しますか？
                </h3>
                {error && (
                  <p className="text-red-400 text-sm text-center mb-4">{error}</p>
                )}
                <div className="grid grid-cols-2 gap-4">
                  <button
                    onClick={handlePersonal}
                    className="flex flex-col items-center gap-3 p-6 bg-gray-800 hover:bg-gray-750 border border-gray-700 hover:border-blue-500 rounded-xl transition-colors"
                  >
                    <svg
                      className="w-12 h-12 text-blue-400"
                      fill="none"
                      viewBox="0 0 24 24"
                      stroke="currentColor"
                      strokeWidth={1.5}
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        d="M15.75 6a3.75 3.75 0 1 1-7.5 0 3.75 3.75 0 0 1 7.5 0ZM4.501 20.118a7.5 7.5 0 0 1 14.998 0A17.933 17.933 0 0 1 12 21.75c-2.676 0-5.216-.584-7.499-1.632Z"
                      />
                    </svg>
                    <span className="font-medium text-base">個人</span>
                    <span className="text-xs text-gray-400 text-center">
                      特定の1人と共有
                    </span>
                  </button>
                  <button
                    onClick={() => setModalStep("group")}
                    className="flex flex-col items-center gap-3 p-6 bg-gray-800 hover:bg-gray-750 border border-gray-700 hover:border-blue-500 rounded-xl transition-colors"
                  >
                    <svg
                      className="w-12 h-12 text-blue-400"
                      fill="none"
                      viewBox="0 0 24 24"
                      stroke="currentColor"
                      strokeWidth={1.5}
                    >
                      <path
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        d="M18 18.72a9.094 9.094 0 0 0 3.741-.479 3 3 0 0 0-4.682-2.72m.94 3.198.001.031c0 .225-.012.447-.037.666A11.944 11.944 0 0 1 12 21c-2.17 0-4.207-.576-5.963-1.584A6.062 6.062 0 0 1 6 18.719m12 0a5.971 5.971 0 0 0-.941-3.197m0 0A5.995 5.995 0 0 0 12 12.75a5.995 5.995 0 0 0-5.058 2.772m0 0a3 3 0 0 0-4.681 2.72 8.986 8.986 0 0 0 3.74.477m.94-3.197a5.971 5.971 0 0 0-.94 3.197M15 6.75a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm6 3a2.25 2.25 0 1 1-4.5 0 2.25 2.25 0 0 1 4.5 0Zm-13.5 0a2.25 2.25 0 1 1-4.5 0 2.25 2.25 0 0 1 4.5 0Z"
                      />
                    </svg>
                    <span className="font-medium text-base">グループ</span>
                    <span className="text-xs text-gray-400 text-center">
                      複数人で共有
                    </span>
                  </button>
                </div>
                <p className="text-xs text-gray-500 text-center mt-4">
                  後からでも変更できます
                </p>
              </>
            )}

            {/* Step: Personal pending */}
            {modalStep === "personal-pending" && (
              <div className="flex flex-col items-center gap-4 py-8">
                <div className="w-8 h-8 border-2 border-blue-400 border-t-transparent rounded-full animate-spin" />
                <p className="text-gray-400 text-sm">作成中...</p>
              </div>
            )}

            {/* Step: Personal result */}
            {modalStep === "personal-result" && (
              <>
                <h3 className="text-lg font-semibold text-center mb-2">
                  招待リンクを共有してください
                </h3>
                <p className="text-sm text-gray-400 text-center mb-4">
                  このリンクは1回のみ使用可能で、24時間後に期限切れになります。
                </p>
                <div className="bg-gray-800 border border-gray-700 rounded-lg p-3 flex items-center gap-2">
                  <input
                    type="text"
                    readOnly
                    value={inviteUrl}
                    className="bg-transparent flex-1 text-sm text-gray-200 outline-none min-w-0"
                  />
                  <button
                    onClick={handleCopy}
                    className="bg-blue-600 hover:bg-blue-700 px-3 py-1.5 rounded text-sm flex-shrink-0"
                  >
                    {copied ? "コピー済み" : "コピー"}
                  </button>
                </div>
                <button
                  onClick={closeModal}
                  className="mt-4 w-full bg-gray-700 hover:bg-gray-600 py-2 rounded text-sm"
                >
                  閉じる
                </button>
              </>
            )}

            {/* Step: Group form */}
            {modalStep === "group" && (
              <>
                <h3 className="text-lg font-semibold mb-4">グループを作成</h3>
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
                  <div className="flex gap-2 mt-1">
                    <button
                      onClick={() => createGroupMutation.mutate()}
                      disabled={createGroupMutation.isPending}
                      className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm disabled:opacity-50 flex-1"
                    >
                      {createGroupMutation.isPending ? "作成中..." : "作成"}
                    </button>
                    <button
                      onClick={() => setModalStep("choose")}
                      className="bg-gray-700 hover:bg-gray-600 px-4 py-2 rounded text-sm"
                    >
                      戻る
                    </button>
                  </div>
                </div>
              </>
            )}
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
              <div className="flex items-center gap-4">
                <div className="relative w-9 h-9 flex-shrink-0">
                  {group.member_previews.length === 0 ? (
                    <UserAvatar
                      src={undefined}
                      name="?"
                      size="xl"
                    />
                  ) : group.member_previews.length === 1 ? (
                    <UserAvatar
                      src={group.member_previews[0].avatar_url}
                      name={group.member_previews[0].display_name}
                      size="xl"
                    />
                  ) : (
                    <>
                      <div className="absolute top-0 left-0 ring-2 ring-gray-900 rounded-full">
                        <UserAvatar
                          src={group.member_previews[0].avatar_url}
                          name={group.member_previews[0].display_name}
                          size="sm"
                        />
                      </div>
                      <div className="absolute bottom-0 right-0 ring-2 ring-gray-900 rounded-full">
                        <UserAvatar
                          src={group.member_previews[1].avatar_url}
                          name={group.member_previews[1].display_name}
                          size="sm"
                        />
                      </div>
                    </>
                  )}
                </div>
                <div className="flex-1 min-w-0">
                  <h3 className="font-medium truncate">
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
                <div className="flex items-center gap-3 flex-shrink-0">
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
