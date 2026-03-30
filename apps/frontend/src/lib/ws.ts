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
  data: (LatestHeartRate | null)[];
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

export function useHeartRateWs(userIds: string[]): Map<string, LatestHeartRate> {
  const [data, setData] = useState<Map<string, LatestHeartRate>>(new Map());
  const wsRef = useRef<WebSocket | null>(null);
  const subscribedRef = useRef<Set<string>>(new Set());
  const userIdsKey = userIds.slice().sort().join(",");

  const connect = useCallback(() => {
    if (typeof window === "undefined") return;

    const ws = new WebSocket(buildWsUrl());
    wsRef.current = ws;

    ws.onopen = () => {
      if (userIds.length > 0) {
        ws.send(JSON.stringify({ type: "subscribe", user_ids: userIds }));
        subscribedRef.current = new Set(userIds);
      }
    };

    ws.onmessage = (event) => {
      try {
        const msg: ServerMessage = JSON.parse(event.data);
        if (msg.type === "snapshot") {
          setData((prev) => {
            const next = new Map(prev);
            for (const item of msg.data) {
              if (item) next.set(item.user_id, item);
            }
            return next;
          });
        } else if (msg.type === "update") {
          setData((prev) => {
            const next = new Map(prev);
            next.set(msg.data.user_id, msg.data);
            return next;
          });
        }
      } catch {
        // Ignore malformed messages
      }
    };

    let backoff = 1000;
    ws.onclose = () => {
      wsRef.current = null;
      // Reconnect with backoff
      setTimeout(() => {
        backoff = Math.min(backoff * 2, 30000);
        connect();
      }, backoff);
    };

    ws.onerror = () => {
      ws.close();
    };
  }, [userIdsKey]);

  // Connect on mount, reconnect when userIds change
  useEffect(() => {
    connect();
    return () => {
      wsRef.current?.close();
      wsRef.current = null;
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
      ws.send(JSON.stringify({ type: "unsubscribe", user_ids: toUnsubscribe }));
    }

    subscribedRef.current = currentIds;
  }, [userIdsKey]);

  return data;
}
