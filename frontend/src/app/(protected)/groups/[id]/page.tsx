"use client";

import { useSession } from "next-auth/react";
import Link from "next/link";
import { useParams, useRouter } from "next/navigation";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  getGroup,
  updateGroup,
  deleteGroup,
  updateMyMembership,
  leaveGroup,
  createInvite,
  listInvites,
  revokeInvite,
  formatGroupDisplayName,
} from "@/lib/groups-api";
import { UserAvatar } from "@/components/user-avatar";
import { GroupHeartRateChart } from "@/components/group-heart-rate-chart";
import { useGroupHeartRateWs } from "@/lib/ws";
import { useState } from "react";

export default function GroupDetailPage() {
  const { id } = useParams<{ id: string }>();
  const { data: session } = useSession();
  const router = useRouter();
  const queryClient = useQueryClient();

  const { data: group, isLoading } = useQuery({
    queryKey: ["group", id],
    queryFn: () => getGroup(id),
  });

  const { data: liveHrData, reconnectCount } = useGroupHeartRateWs(group ? id : null);

  const [invitesOpen, setInvitesOpen] = useState(false);

  const { data: invites } = useQuery({
    queryKey: ["group-invites", id],
    queryFn: () => listInvites(id),
    enabled:
      invitesOpen &&
      !!group &&
      (group.my_role === "owner" || group.invite_policy === "group+"),
  });

  const [editing, setEditing] = useState(false);
  const [editName, setEditName] = useState("");
  const [editPolicy, setEditPolicy] = useState("");
  const [newInviteToken, setNewInviteToken] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const isOwner = group?.my_role === "owner";

  const updateMutation = useMutation({
    mutationFn: () =>
      updateGroup(id, {
        name: editName.trim(),
        invite_policy: editPolicy,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["group", id] });
      queryClient.invalidateQueries({ queryKey: ["groups"] });
      setEditing(false);
    },
  });

  const sharingMutation = useMutation({
    mutationFn: (sharing: boolean) => updateMyMembership(id, sharing),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["group", id] });
      queryClient.invalidateQueries({ queryKey: ["groups"] });
    },
  });

  const deleteMutation = useMutation({
    mutationFn: () => deleteGroup(id),
    onSuccess: () => {
      router.push("/groups");
    },
  });

  const leaveMutation = useMutation({
    mutationFn: () => leaveGroup(id),
    onSuccess: () => {
      router.push("/groups");
    },
  });

  const createInviteMutation = useMutation({
    mutationFn: () => createInvite(id, {}),
    onSuccess: (data) => {
      setNewInviteToken(data.token);
      setCopied(false);
      queryClient.invalidateQueries({ queryKey: ["group-invites", id] });
    },
  });

  const revokeMutation = useMutation({
    mutationFn: (inviteId: string) => revokeInvite(id, inviteId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["group-invites", id] });
    },
  });

  if (isLoading) return <p className="text-gray-400">読み込み中...</p>;
  if (!group) return <p className="text-gray-400">グループが見つかりません</p>;

  const inviteUrl = newInviteToken
    ? `${window.location.origin}/invite/${newInviteToken}`
    : null;

  const canInvite =
    isOwner || group.invite_policy === "group+";

  return (
    <div>
      {/* Header */}
      <div className="flex items-center justify-between mb-6">
        <div>
          <h1 className="text-2xl font-bold">
            {formatGroupDisplayName(group.display_name, group.name, group.members.length)}
          </h1>
          {group.name && (
            <p className="text-sm text-gray-400 mt-1">
              グループ名: {group.name}
            </p>
          )}
        </div>
        {!editing && isOwner && (
          <button
            onClick={() => {
              setEditName(group.name ?? "");
              setEditPolicy(group.invite_policy);
              setEditing(true);
            }}
            className="text-sm text-blue-400 hover:text-blue-300"
          >
            編集
          </button>
        )}
      </div>

      {/* Edit form */}
      {editing && (
        <div className="bg-gray-900 border border-gray-700 rounded-lg p-4 mb-6">
          <div className="flex flex-col gap-3">
            <div>
              <label className="block text-sm text-gray-400 mb-1">
                グループ名
              </label>
              <input
                type="text"
                value={editName}
                onChange={(e) => setEditName(e.target.value)}
                maxLength={50}
                className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full text-sm"
              />
            </div>
            <div>
              <label className="block text-sm text-gray-400 mb-1">
                招待ポリシー
              </label>
              <select
                value={editPolicy}
                onChange={(e) => setEditPolicy(e.target.value)}
                className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full text-sm"
              >
                <option value="group">Ownerのみ招待可能</option>
                <option value="group+">Member全員が招待可能</option>
              </select>
            </div>
            <div className="flex gap-2">
              <button
                onClick={() => updateMutation.mutate()}
                disabled={
                  updateMutation.isPending ||
                  [...editName.trim()].length > 50
                }
                className="bg-blue-600 hover:bg-blue-700 px-4 py-2 rounded text-sm disabled:opacity-50"
              >
                保存
              </button>
              <button
                onClick={() => setEditing(false)}
                className="bg-gray-700 hover:bg-gray-600 px-4 py-2 rounded text-sm"
              >
                キャンセル
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Heart Rate Chart */}
      {session?.user?.id && (
        <GroupHeartRateChart
          groupId={id}
          members={group.members}
          currentUserId={session.user.id}
          liveHrData={liveHrData}
          wsReconnectCount={reconnectCount}
        />
      )}

      {/* Members */}
      <div className="mb-6">
        <h2 className="text-lg font-semibold mb-3">
          メンバー{group.members.length <= 2 && ` (${group.members.length})`}
        </h2>
        <div className="grid grid-cols-1 md:grid-cols-2 gap-2">
          {group.members.map((member) => {
            const hr = liveHrData.get(member.user_id);
            return (
              <div
                key={member.user_id}
                className="bg-gray-900 border border-gray-700 rounded-lg p-3 grid grid-cols-[auto_1fr_auto] items-center gap-3"
              >
                <UserAvatar
                  name={member.display_name}
                  src={member.avatar_url}
                  size="md"
                />
                <div className="min-w-0">
                  <Link
                    href={`/users/${member.user_id}?from=${id}`}
                    className="font-medium hover:underline"
                  >
                    {member.display_name}
                  </Link>
                  {member.role === "owner" && group.members.length !== 2 && (
                    <span className="ml-2 text-xs bg-yellow-600/30 text-yellow-400 px-2 py-0.5 rounded">
                      Owner
                    </span>
                  )}
                </div>
                <div className="flex items-center gap-3">
                  <span className="w-24 text-right text-lg font-mono font-bold text-red-400">
                    {hr ? `${hr.bpm} BPM` : ""}
                  </span>
                  <span className="w-16 text-xs text-gray-400">
                    {hr
                      ? new Date(hr.recorded_at * 1000).toLocaleTimeString("ja-JP", {
                          hour: "2-digit",
                          minute: "2-digit",
                          second: "2-digit",
                        })
                      : ""}
                  </span>
                  <span
                    className={`w-12 text-right text-sm ${
                      member.sharing ? "text-green-400" : "text-gray-500"
                    }`}
                  >
                    {member.sharing ? "共有中" : "非共有"}
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      {/* Invitations */}
      {canInvite && (
        <div className="mb-6">
          <div className="flex items-center justify-between mb-3">
            <button
              onClick={() => setInvitesOpen((v) => !v)}
              className="flex items-center gap-2 text-lg font-semibold hover:text-gray-300"
            >
              <span className="text-sm text-gray-400">
                {invitesOpen ? "▼" : "▶"}
              </span>
              招待
            </button>
            <button
              onClick={() => {
                setInvitesOpen(true);
                createInviteMutation.mutate();
              }}
              disabled={createInviteMutation.isPending}
              className="bg-blue-600 hover:bg-blue-700 px-3 py-1.5 rounded text-sm disabled:opacity-50"
            >
              招待を作成
            </button>
          </div>

          {invitesOpen && (
            <>
              {/* New invite token display */}
              {inviteUrl && (
                <div className="bg-green-900/30 border border-green-700 rounded-lg p-4 mb-4">
                  <p className="text-sm text-green-400 mb-2">
                    招待リンクが作成されました。このリンクは一度だけ表示されます。
                  </p>
                  <div className="flex items-center gap-2">
                    <input
                      type="text"
                      readOnly
                      value={inviteUrl}
                      className="bg-gray-800 border border-gray-700 rounded px-3 py-2 w-full text-sm font-mono"
                    />
                    <button
                      onClick={() => {
                        navigator.clipboard.writeText(inviteUrl);
                        setCopied(true);
                      }}
                      className="bg-gray-700 hover:bg-gray-600 px-3 py-2 rounded text-sm whitespace-nowrap"
                    >
                      {copied ? "コピー済み" : "コピー"}
                    </button>
                  </div>
                </div>
              )}

              {/* Active invites list */}
              {invites && invites.length > 0 && (
                <div className="flex flex-col gap-2">
                  {invites.map((invite) => (
                    <div
                      key={invite.id}
                      className="bg-gray-900 border border-gray-700 rounded-lg p-3 flex items-center justify-between"
                    >
                      <div className="text-sm">
                        <span className="text-gray-400">
                          {invite.created_by_name}が作成
                        </span>
                        <span className="text-gray-500 mx-2">・</span>
                        <span className="text-gray-400">
                          {new Date(invite.expires_at * 1000).toLocaleDateString()}
                          まで
                        </span>
                        {invite.max_uses && (
                          <>
                            <span className="text-gray-500 mx-2">・</span>
                            <span className="text-gray-400">
                              {invite.use_count}/{invite.max_uses}回使用
                            </span>
                          </>
                        )}
                      </div>
                      {(isOwner || invite.created_by === session?.user?.id) && (
                        <button
                          onClick={() => revokeMutation.mutate(invite.id)}
                          disabled={revokeMutation.isPending}
                          className="text-sm text-red-400 hover:text-red-300 disabled:opacity-50"
                        >
                          無効化
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )}

              {invites && invites.length === 0 && (
                <p className="text-sm text-gray-400">
                  有効な招待はありません。
                </p>
              )}
            </>
          )}
        </div>
      )}

      {/* Controls */}
      <div className="border-t border-gray-700 pt-6">
        <h2 className="text-lg font-semibold mb-4">コントロール</h2>

        {/* Sharing toggle */}
        <div className="bg-gray-900 border border-gray-700 rounded-lg p-4 mb-3">
          <div className="flex items-center justify-between">
            <div>
              <h3 className="font-medium">心拍データの共有</h3>
              <p className="text-sm text-gray-400 mt-1">
                このグループのメンバーに心拍データを公開する
              </p>
            </div>
            <button
              onClick={() => sharingMutation.mutate(!group.my_sharing)}
              className={`w-12 h-7 rounded-full transition-colors relative ${
                group.my_sharing ? "bg-blue-600" : "bg-gray-600"
              }`}
            >
              <span
                className={`absolute top-1 w-5 h-5 rounded-full bg-white transition-transform ${
                  group.my_sharing ? "left-6" : "left-1"
                }`}
              />
            </button>
          </div>
        </div>

        {/* Delete/Leave */}
        <div className="bg-gray-900 border border-red-900/50 rounded-lg p-4">
          <div className="flex items-center justify-between">
            <div>
              <h3 className="font-medium text-red-400">
                {isOwner ? "グループを削除" : "グループから退出"}
              </h3>
              <p className="text-sm text-gray-400 mt-1">
                {isOwner
                  ? "グループとすべてのデータを削除します"
                  : "このグループから退出します"}
              </p>
            </div>
            {isOwner ? (
              <button
                onClick={() => {
                  if (confirm("このグループを削除しますか？メンバー全員が退出されます。")) {
                    deleteMutation.mutate();
                  }
                }}
                disabled={deleteMutation.isPending}
                className="text-sm bg-red-600 hover:bg-red-700 px-4 py-2 rounded disabled:opacity-50"
              >
                削除
              </button>
            ) : (
              <button
                onClick={() => {
                  if (confirm("このグループから退出しますか？")) {
                    leaveMutation.mutate();
                  }
                }}
                disabled={leaveMutation.isPending}
                className="text-sm bg-red-600 hover:bg-red-700 px-4 py-2 rounded disabled:opacity-50"
              >
                退出
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
