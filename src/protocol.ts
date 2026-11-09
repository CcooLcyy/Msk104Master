import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";

export interface ConnectionConfig {
  host: string;
  port: number;
  commonAddress: number;
}

export interface FrameRow {
  id: number;
  time: string;
  direction: string;
  kind: string;
  length: number;
  raw: string;
  summary: string;
  parsed: string;
}

export interface PointRow {
  id: string;
  time: string;
  commonAddress: number;
  address: number;
  typeId: number;
  value: string;
  quality: string;
  timestamp: string;
}

type SessionState = "disconnected" | "connecting" | "starting" | "started" | "disconnecting";
type DisplayData = { frames: FrameRow[]; points: PointRow[]; events: PointRow[] };
interface ProtocolSnapshot extends DisplayData {
  state: SessionState;
  error: string | null;
  interrogation: string;
  interrogationPending: boolean;
  rxFrames: number;
  txFrames: number;
  rxBytes: number;
  txBytes: number;
  lastReceive: string | null;
}

type ProtocolAction =
  | { kind: "connect"; config: ConnectionConfig }
  | { kind: "disconnect" | "interrogate" | "clear" };

const emptyDisplay: DisplayData = { frames: [], points: [], events: [] };
const emptyCounters = {
  rxFrames: 0,
  txFrames: 0,
  rxBytes: 0,
  txBytes: 0,
  lastReceive: null,
};
const initialSnapshot: ProtocolSnapshot = {
  ...emptyDisplay,
  ...emptyCounters,
  state: "disconnected",
  error: null,
  interrogation: "未执行",
  interrogationPending: false,
};

const actions = {
  connect: { command: "protocol_connect", label: "连接" },
  disconnect: { command: "protocol_disconnect", label: "断开" },
  interrogate: { command: "protocol_interrogate", label: "总召" },
  clear: { command: "protocol_clear", label: "清空显示" },
};

export const stateLabels: Record<SessionState, string> = {
  disconnected: "未连接",
  connecting: "连接中",
  starting: "等待 STARTDT 确认",
  started: "数据传输已启动",
  disconnecting: "断开中",
};

export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

const timeFormat = new Intl.DateTimeFormat("zh-CN", {
  hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3,
  hourCycle: "h23",
});

export function displayTime(time: string): string {
  if (!/^\d+$/.test(time)) return time;
  const date = new Date(Number(time));
  return Number.isNaN(date.getTime()) ? time : timeFormat.format(date);
}

export function directionLabel(direction: string): string {
  switch (direction) {
    case "receive": return "接收";
    case "send": return "发送";
    case "sendFailed": return "发送失败";
    default: return direction;
  }
}

export function useProtocol() {
  const [snapshot, setSnapshot] = useState(initialSnapshot);
  const [display, setDisplay] = useState(emptyDisplay);
  const [paused, setPaused] = useState(false);
  const [pending, setPending] = useState<ProtocolAction["kind"] | null>(null);
  const [actionError, setActionError] = useState("");
  const [readError, setReadError] = useState("");
  const [displayRevision, setDisplayRevision] = useState(0);
  const actionBusy = useRef(false);
  const pausedRef = useRef(false);
  const latestDisplay = useRef(emptyDisplay);
  const generation = useRef(0);
  const polling = useRef<Promise<void> | null>(null);

  const readSnapshot = useCallback(async (isCurrent: () => boolean) => {
    try {
      const result = await invoke<ProtocolSnapshot>("protocol_snapshot");
      if (!isCurrent()) return;
      setReadError("");
      setSnapshot(result);
      latestDisplay.current = result;
      if (!pausedRef.current) setDisplay(result);
    } catch (error: unknown) {
      if (isCurrent()) setReadError(`读取协议状态失败：${errorMessage(error)}`);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    let timer: number | undefined;
    async function poll() {
      if (!actionBusy.current && !polling.current) {
        const current = generation.current;
        const request = readSnapshot(() => !disposed && current === generation.current);
        polling.current = request;
        await request;
        polling.current = null;
      }
      if (!disposed) timer = window.setTimeout(poll, 450);
    }
    void poll();
    return () => {
      disposed = true;
      window.clearTimeout(timer);
    };
  }, [readSnapshot]);

  function togglePause() {
    pausedRef.current = !pausedRef.current;
    setPaused(pausedRef.current);
    if (!pausedRef.current) setDisplay(latestDisplay.current);
  }

  async function run(action: ProtocolAction) {
    if (actionBusy.current) return;
    actionBusy.current = true;
    // 操作开始前发出的快照不能覆盖本次操作的结果。
    const current = ++generation.current;
    setPending(action.kind);
    setActionError("");
    const { command, label } = actions[action.kind];
    try {
      await invoke(command, action.kind === "connect" ? { config: action.config } : undefined);
      if (action.kind === "connect" || action.kind === "clear") {
        latestDisplay.current = emptyDisplay;
        setDisplay(emptyDisplay);
        setDisplayRevision((value) => value + 1);
      }
      if (action.kind === "clear") setSnapshot((value) => ({ ...value, ...emptyDisplay, ...emptyCounters }));
      if (action.kind === "connect") setSnapshot({ ...initialSnapshot, state: "connecting" });
      if (action.kind === "disconnect") setSnapshot((value) => ({ ...value, state: "disconnecting" }));
      if (action.kind === "interrogate") {
        setSnapshot((value) => ({ ...value, interrogation: "总召已排队", interrogationPending: true }));
      }
    } catch (error: unknown) {
      setActionError(`${label}失败：${errorMessage(error)}`);
    } finally {
      // 等已在途的旧请求结束，再读取操作后的状态，避免快照并行。
      await polling.current;
      await readSnapshot(() => current === generation.current);
      actionBusy.current = false;
      setPending(null);
    }
  }

  return { snapshot, display, paused, togglePause, pending, actionBusy, actionError, readError, displayRevision, run };
}
