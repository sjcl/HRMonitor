"use client";

import { useEffect, useRef, useState, useCallback } from "react";

export interface LatestHeartRate {
  user_id: string;
  bpm: number;
  recorded_at: number;
  received_at: number;
}

interface SnapshotMessage {
  type: "snapshot";
  data: Record<string, LatestHeartRate | null>;
}

interface UpdateMessage {
  type: "update";
  data: LatestHeartRate;
}

type ServerMessage = SnapshotMessage | UpdateMessage;

function buildWsUrl(path: string): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}${path}`;
}

type SessionStatus = "authenticated" | "unauthenticated" | "error";

async function checkSession(): Promise<SessionStatus> {
  try {
    const res = await fetch("/api/auth/session");
    if (!res.ok) return "error";
    const data = await res.json();
    return data?.user ? "authenticated" : "unauthenticated";
  } catch {
    return "error";
  }
}

// ---------------------------------------------------------------------------
// Shared WebSocket connection hook
// ---------------------------------------------------------------------------

interface UseWsConnectionOptions {
  /** WS path (e.g. "/api/ws/me"). null = don't connect. */
  path: string | null;
  /** Called for each incoming server message. */
  onMessage: (msg: ServerMessage) => void;
}

function useWsConnection({
  path,
  onMessage,
}: UseWsConnectionOptions): { reconnectCount: number } {
  const [reconnectCount, setReconnectCount] = useState(0);
  const wsRef = useRef<WebSocket | null>(null);
  const hasConnectedRef = useRef(false);
  const wasDisconnectedRef = useRef(false);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const backoffRef = useRef(1000);
  const onMessageRef = useRef(onMessage);
  onMessageRef.current = onMessage;

  useEffect(() => {
    if (path === null) return;
    const currentPath = path;

    let cancelled = false;

    function scheduleReconnect() {
      if (cancelled) return;
      wasDisconnectedRef.current = true;
      const delay = backoffRef.current;
      reconnectTimerRef.current = setTimeout(() => {
        reconnectTimerRef.current = null;
        backoffRef.current = Math.min(backoffRef.current * 2, 30000);
        connect();
      }, delay);
    }

    async function connect() {
      if (typeof window === "undefined") return;

      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }

      const sessionStatus = await checkSession();
      if (cancelled) return;
      if (sessionStatus === "unauthenticated") {
        window.location.href = "/login";
        return;
      }
      if (sessionStatus === "error") {
        scheduleReconnect();
        return;
      }

      const ws = new WebSocket(buildWsUrl(currentPath));
      wsRef.current = ws;

      ws.onopen = () => {
        if (cancelled || ws !== wsRef.current) {
          ws.close();
          return;
        }
        if (hasConnectedRef.current && wasDisconnectedRef.current) {
          setReconnectCount((c) => c + 1);
        }
        hasConnectedRef.current = true;
        wasDisconnectedRef.current = false;
        backoffRef.current = 1000;
      };

      ws.onmessage = (event) => {
        if (ws !== wsRef.current) return;
        try {
          const msg: ServerMessage = JSON.parse(event.data);
          onMessageRef.current(msg);
        } catch {
          // Ignore malformed messages
        }
      };

      ws.onclose = () => {
        if (ws !== wsRef.current) return;
        wsRef.current = null;
        scheduleReconnect();
      };

      ws.onerror = () => {
        ws.close();
      };
    }

    connect();

    return () => {
      cancelled = true;
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      const ws = wsRef.current;
      wsRef.current = null;
      ws?.close();
    };
  }, [path]);

  return { reconnectCount };
}

// ---------------------------------------------------------------------------
// /api/ws/me — own heart rate
// ---------------------------------------------------------------------------

export function useMyHeartRateWs(): {
  data: LatestHeartRate | null;
  reconnectCount: number;
} {
  const [data, setData] = useState<LatestHeartRate | null>(null);

  const onMessage = useCallback((msg: ServerMessage) => {
    if (msg.type === "snapshot") {
      const values = Object.values(msg.data);
      setData(values[0] ?? null);
    } else if (msg.type === "update") {
      setData(msg.data);
    }
  }, []);

  const { reconnectCount } = useWsConnection({
    path: "/api/ws/me",
    onMessage,
  });

  return { data, reconnectCount };
}

// ---------------------------------------------------------------------------
// /api/ws/users/{id} — specific user's heart rate
// ---------------------------------------------------------------------------

export function useUserHeartRateWs(
  userId: string | null,
): { data: LatestHeartRate | null; reconnectCount: number } {
  const [data, setData] = useState<LatestHeartRate | null>(null);

  // Reset data when userId changes
  const prevUserIdRef = useRef(userId);
  if (prevUserIdRef.current !== userId) {
    prevUserIdRef.current = userId;
    setData(null);
  }

  const onMessage = useCallback((msg: ServerMessage) => {
    if (msg.type === "snapshot") {
      const values = Object.values(msg.data);
      setData(values[0] ?? null);
    } else if (msg.type === "update") {
      setData(msg.data);
    }
  }, []);

  const path = userId ? `/api/ws/users/${userId}` : null;
  const { reconnectCount } = useWsConnection({ path, onMessage });

  return { data, reconnectCount };
}

// ---------------------------------------------------------------------------
// /api/ws/groups/{id} — group heart rates
// ---------------------------------------------------------------------------

export function useGroupHeartRateWs(
  groupId: string | null,
): { data: Map<string, LatestHeartRate>; reconnectCount: number } {
  const [data, setData] = useState<Map<string, LatestHeartRate>>(new Map());

  // Batch pending updates and flush once per animation frame
  const pendingUpdatesRef = useRef<Map<string, LatestHeartRate>>(new Map());
  const rafRef = useRef<number | null>(null);

  const flushUpdates = useCallback(() => {
    rafRef.current = null;
    const pending = pendingUpdatesRef.current;
    if (pending.size === 0) return;
    const batch = new Map(pending);
    pending.clear();
    setData((prev) => {
      const next = new Map(prev);
      for (const [uid, hr] of batch) {
        next.set(uid, hr);
      }
      return next;
    });
  }, []);

  const onMessage = useCallback(
    (msg: ServerMessage) => {
      if (msg.type === "snapshot") {
        // Snapshots are infrequent — apply immediately
        setData((prev) => {
          const next = new Map(prev);
          for (const [userId, item] of Object.entries(msg.data)) {
            if (item) {
              next.set(userId, item);
            } else {
              next.delete(userId);
            }
          }
          return next;
        });
      } else if (msg.type === "update") {
        pendingUpdatesRef.current.set(msg.data.user_id, msg.data);
        if (rafRef.current === null) {
          rafRef.current = requestAnimationFrame(flushUpdates);
        }
      }
    },
    [flushUpdates],
  );

  // Reset data and cancel pending updates when groupId changes (or on unmount)
  useEffect(() => {
    setData(new Map());
    return () => {
      pendingUpdatesRef.current.clear();
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
    };
  }, [groupId]);

  const path = groupId ? `/api/ws/groups/${groupId}` : null;
  const { reconnectCount } = useWsConnection({ path, onMessage });

  return { data, reconnectCount };
}
