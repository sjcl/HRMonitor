"use client";

import { useParams, useRouter } from "next/navigation";
import { SessionProvider, useSession } from "next-auth/react";
import { useQuery, useMutation } from "@tanstack/react-query";
import { getInviteInfo, acceptInvite } from "@/lib/groups-api";
import { ApiError } from "@/lib/api";

export default function InvitePage() {
  return (
    <SessionProvider>
      <main className="max-w-5xl mx-auto p-6">
        <InviteContent />
      </main>
    </SessionProvider>
  );
}

function InviteContent() {
  const { token } = useParams<{ token: string }>();
  const { data: session, status: sessionStatus } = useSession();
  const router = useRouter();

  const isLoggedIn = sessionStatus === "authenticated" && !!session;
  const isLoading = sessionStatus === "loading";

  const {
    data: invite,
    error,
    isLoading: inviteLoading,
  } = useQuery({
    queryKey: ["invite", token],
    queryFn: () => getInviteInfo(token),
    enabled: isLoggedIn,
    retry: false,
  });

  const acceptMutation = useMutation({
    mutationFn: () => acceptInvite(token),
    onSuccess: (data) => {
      router.push(`/groups/${data.group_id}`);
    },
  });

  if (isLoading) {
    return (
      <div className="flex items-center justify-center min-h-[60vh]">
        <p className="text-gray-400">読み込み中...</p>
      </div>
    );
  }

  // Not logged in: show login prompt
  if (!isLoggedIn) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-6">
        <h1 className="text-2xl font-bold">グループへの招待</h1>
        <p className="text-gray-400">
          グループに参加するにはログインが必要です。
        </p>
        <a
          href={`/login?callbackUrl=/invite/${token}`}
          className="bg-[#5865F2] hover:bg-[#4752C4] px-6 py-3 rounded-lg text-white font-medium transition-colors"
        >
          ログインして参加する
        </a>
      </div>
    );
  }

  if (inviteLoading) {
    return (
      <div className="flex items-center justify-center min-h-[60vh]">
        <p className="text-gray-400">招待情報を読み込み中...</p>
      </div>
    );
  }

  // API error
  if (error) {
    const apiError = error as ApiError;
    const message =
      apiError.status === 404
        ? "この招待リンクは存在しません。"
        : "招待情報の取得に失敗しました。";
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <h1 className="text-2xl font-bold">招待エラー</h1>
        <p className="text-gray-400">{message}</p>
      </div>
    );
  }

  if (!invite) return null;

  const displayName = invite.group_display_name ?? invite.group_name ?? "グループ";

  // Already a member
  if (invite.already_member) {
    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <h1 className="text-2xl font-bold">{displayName}</h1>
        <p className="text-gray-400">既にこのグループに参加しています。</p>
        <a
          href={`/groups/${invite.group_id}`}
          className="bg-blue-600 hover:bg-blue-700 px-6 py-3 rounded-lg font-medium transition-colors"
        >
          グループを開く
        </a>
      </div>
    );
  }

  // Invalid invite
  if (!invite.valid) {
    const reasonText =
      invite.reason === "expired"
        ? "この招待リンクは期限切れです。"
        : invite.reason === "revoked"
          ? "この招待リンクは無効化されています。"
          : invite.reason === "usage_limit_reached"
            ? "この招待リンクは使用回数の上限に達しました。"
            : invite.reason === "not_for_you"
              ? "この招待リンクはあなた宛てではありません。"
              : "この招待リンクは無効です。";

    return (
      <div className="flex flex-col items-center justify-center min-h-[60vh] gap-4">
        <h1 className="text-2xl font-bold">招待エラー</h1>
        <p className="text-gray-400">{reasonText}</p>
      </div>
    );
  }

  // Accept error
  const acceptError = acceptMutation.error as ApiError | null;
  const acceptErrorText = acceptError
    ? acceptError.status === 409
      ? "既にこのグループに参加しています。"
      : acceptError.status === 410
        ? "この招待リンクは無効です。"
        : acceptError.status === 403
          ? "この招待リンクはあなた宛てではありません。"
          : "参加に失敗しました。"
    : null;

  // Valid invite: show confirmation
  return (
    <div className="flex flex-col items-center justify-center min-h-[60vh] gap-6">
      <h1 className="text-2xl font-bold">{displayName}</h1>
      <p className="text-gray-400">
        {invite.inviter_name}さんからの招待です。
      </p>
      <p className="text-sm text-gray-500">
        有効期限: {new Date(invite.expires_at * 1000).toLocaleString()}
      </p>

      {acceptErrorText && (
        <p className="text-red-400 text-sm">{acceptErrorText}</p>
      )}

      <button
        onClick={() => acceptMutation.mutate()}
        disabled={acceptMutation.isPending}
        className="bg-blue-600 hover:bg-blue-700 px-6 py-3 rounded-lg font-medium transition-colors disabled:opacity-50"
      >
        {acceptMutation.isPending ? "参加中..." : "グループに参加する"}
      </button>
    </div>
  );
}
