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

function buildWsUrl(): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  return `${proto}//${location.host}/api/ws/heart-rates`;
}

export function useHeartRateWs(
  userIds: string[],
): { data: Map<string, LatestHeartRate>; reconnectCount: number } {
  const [data, setData] = useState<Map<string, LatestHeartRate>>(new Map());
  const [reconnectCount, setReconnectCount] = useState(0);
  const wsRef = useRef<WebSocket | null>(null);
  const subscribedRef = useRef<Set<string>>(new Set());
  const userIdsRef = useRef(userIds);
  const hasConnectedRef = useRef(false);
  const wasDisconnectedRef = useRef(false);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const backoffRef = useRef(1000);

  // Batch pending WS updates and flush once per animation frame
  const pendingUpdatesRef = useRef<Map<string, LatestHeartRate>>(new Map());
  const rafRef = useRef<number | null>(null);

  // Keep userIdsRef in sync
  userIdsRef.current = userIds;
  const userIdsKey = userIds.slice().sort().join(",");

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

  const connect = useCallback(() => {
    if (typeof window === "undefined") return;

    // Clear any pending reconnect timer
    if (reconnectTimerRef.current) {
      clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }

    const ws = new WebSocket(buildWsUrl());
    wsRef.current = ws;

    ws.onopen = () => {
      if (hasConnectedRef.current && wasDisconnectedRef.current) {
        setReconnectCount((c) => c + 1);
      }
      hasConnectedRef.current = true;
      wasDisconnectedRef.current = false;
      backoffRef.current = 1000;

      const ids = userIdsRef.current;
      if (ids.length > 0) {
        ws.send(JSON.stringify({ type: "subscribe", user_ids: ids }));
        subscribedRef.current = new Set(ids);
      }
    };

    ws.onmessage = (event) => {
      if (ws !== wsRef.current) return;
      try {
        const msg: ServerMessage = JSON.parse(event.data);
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
          // Batch updates and flush once per animation frame
          pendingUpdatesRef.current.set(msg.data.user_id, msg.data);
          if (rafRef.current === null) {
            rafRef.current = requestAnimationFrame(flushUpdates);
          }
        }
      } catch {
        // Ignore malformed messages
      }
    };

    ws.onclose = () => {
      if (ws !== wsRef.current) return;
      wsRef.current = null;
      wasDisconnectedRef.current = true;
      const delay = backoffRef.current;
      reconnectTimerRef.current = setTimeout(() => {
        reconnectTimerRef.current = null;
        backoffRef.current = Math.min(backoffRef.current * 2, 30000);
        connect();
      }, delay);
    };

    ws.onerror = () => {
      ws.close();
    };
  }, []);

  // Connect on mount only
  useEffect(() => {
    connect();
    return () => {
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      const ws = wsRef.current;
      wsRef.current = null;
      ws?.close();
    };
  }, [connect]);

  // Handle subscription changes while connected
  useEffect(() => {
    const ws = wsRef.current;
    if (!ws || ws.readyState !== WebSocket.OPEN) return;

    const currentIds = new Set(userIds);
    const prevIds = subscribedRef.current;

    const toSubscribe = userIds.filter((id) => !prevIds.has(id));
    const toUnsubscribe = [...prevIds].filter((id) => !currentIds.has(id));

    if (toSubscribe.length > 0) {
      ws.send(JSON.stringify({ type: "subscribe", user_ids: toSubscribe }));
    }
    if (toUnsubscribe.length > 0) {
      ws.send(
        JSON.stringify({ type: "unsubscribe", user_ids: toUnsubscribe }),
      );
      setData((prev) => {
        const next = new Map(prev);
        for (const id of toUnsubscribe) next.delete(id);
        return next;
      });
    }

    subscribedRef.current = currentIds;
  }, [userIdsKey]);

  return { data, reconnectCount };
}
