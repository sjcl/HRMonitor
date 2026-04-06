import { ApiError } from "./api";

// --- Types ---

export interface GroupListItem {
  id: string;
  name: string | null;
  display_name: string | null;
  member_count: number;
  my_sharing: boolean;
  my_role: string;
  invite_policy: string;
  created_at: number;
}

export interface GroupMemberInfo {
  user_id: string;
  display_name: string;
  avatar_url: string | null;
  role: string;
  sharing: boolean;
}

export interface GroupDetail {
  id: string;
  name: string | null;
  display_name: string | null;
  invite_policy: string;
  my_sharing: boolean;
  my_role: string;
  members: GroupMemberInfo[];
  created_at: number;
}

export interface InviteListItem {
  id: string;
  created_by_name: string;
  expires_at: number;
  max_uses: number | null;
  use_count: number;
  created_at: number;
}

export interface InviteInfo {
  group_name: string | null;
  group_display_name: string | null;
  group_id: string;
  inviter_name: string;
  expires_at: number;
  valid: boolean;
  reason: string | null;
  already_member: boolean;
}

export interface CreateInviteResponse {
  id: string;
  token: string;
  expires_at: number;
}

export interface AcceptInviteResponse {
  group_id: string;
}

// --- API functions ---

async function fetchJson<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  if (res.status === 401) {
    window.location.href = "/login";
    throw new ApiError(401, "Unauthorized");
  }
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body.error || `HTTP ${res.status}`);
  }
  if (res.status === 204) return undefined as T;
  return res.json();
}

export function getGroups() {
  return fetchJson<GroupListItem[]>("/api/groups");
}

export function createGroup(data: {
  name?: string;
  invite_policy?: string;
}) {
  return fetchJson<GroupDetail>("/api/groups", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export function getGroup(id: string) {
  return fetchJson<GroupDetail>(`/api/groups/${id}`);
}

export function updateGroup(
  id: string,
  data: { name?: string; invite_policy?: string },
) {
  return fetchJson<GroupDetail>(`/api/groups/${id}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export function deleteGroup(id: string) {
  return fetchJson<void>(`/api/groups/${id}`, { method: "DELETE" });
}

export function updateMyMembership(groupId: string, sharing: boolean) {
  return fetchJson<void>(`/api/groups/${groupId}/members/me`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ sharing }),
  });
}

export function leaveGroup(groupId: string) {
  return fetchJson<void>(`/api/groups/${groupId}/members/me`, {
    method: "DELETE",
  });
}

export function createInvite(
  groupId: string,
  data: {
    expires_in_hours?: number;
    max_uses?: number;
    target_user_id?: string;
  },
) {
  return fetchJson<CreateInviteResponse>(`/api/groups/${groupId}/invites`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(data),
  });
}

export function listInvites(groupId: string) {
  return fetchJson<InviteListItem[]>(`/api/groups/${groupId}/invites`);
}

export function revokeInvite(groupId: string, inviteId: string) {
  return fetchJson<void>(`/api/groups/${groupId}/invites/${inviteId}`, {
    method: "DELETE",
  });
}

export function getInviteInfo(token: string) {
  return fetchJson<InviteInfo>(`/api/invites/${token}`);
}

export function acceptInvite(token: string) {
  return fetchJson<AcceptInviteResponse>(`/api/invites/${token}/accept`, {
    method: "POST",
  });
}
