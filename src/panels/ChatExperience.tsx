import { memo, useCallback, useDeferredValue, useEffect, useLayoutEffect, useMemo, useRef, useState, type DragEvent as ReactDragEvent, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  AlertCircle,
  Bot,
  Brain,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  Circle,
  Clock,
  Code2,
  Copy,
  Eye,
  FileText,
  FolderOpen,
  Image as ImageIcon,
  Layers,
  Loader2,
  MessageSquareText,
  Mic,
  MicOff,
  Network,
  PanelRightClose,
  PanelRightOpen,
  Paperclip,
  Plus,
  RefreshCw,
  Search,
  SendHorizontal,
  Smile,
  Settings2,
  Sparkles,
  Square,
  Terminal,
  Trash2,
  Wrench,
  Zap,
  X
} from "lucide-react";
import { api, isTauri } from "../lib/api";
import { displayTextForMessage, renderTextForMessage, speechTextForMessage } from "../lib/messageText";
import { resolvePersonaAgentBinding, resolvePersonaBoundAgent } from "../lib/personaAgentBinding";
import { PET_THINKING_STATE_EVENT, publishPetThinkingState, type PetThinkingState } from "../lib/petContext";
import { useAppStore } from "../lib/store";
import type { AgentControlCommand, AgentDefinition, AgentRunPhase, AgentRunRecord, AgentRuntimeEvent, ChatAttachment, ChatMessage, EmojiGroup, LlmProvider, ManagedProcessEvent, ModelCatalogEntry, ToolEvent, ToolEventEnvelope } from "../lib/types";
import { Avatar } from "../components/common";

type ComposerAttachment = ChatAttachment & {
  preview: string | null;
  status: "ready" | "staging" | "error";
  error?: string;
};

type VoiceInputState = "idle" | "listening" | "recording" | "transcribing";

type SpeechRecognitionLike = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  onresult: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
  onend: (() => void) | null;
  start: () => void;
  stop: () => void;
  abort: () => void;
};

type SpeechRecognitionConstructor = new () => SpeechRecognitionLike;

type ArtifactTarget = {
  path: string;
  title: string;
  kind: "image" | "file";
  source: string;
};

type EmojiPathIndexes = {
  byPath: Map<string, string>;
  byFile: Map<string, string>;
};

type ShortMemoryMessageStat = {
  label: string;
  tone: "tokens" | "messages";
};

type ThinkingCard = {
  key: string;
  provider: string;
  kind: string;
  title: string;
  summary: string;
  redacted: boolean;
  encrypted: boolean;
  streaming: boolean;
};

type MessageRenderMode = "normal" | "thinking" | "content";

type MessageRenderItem = {
  key: string;
  elementId: string;
  message: ChatMessage;
  mode: MessageRenderMode;
  cards?: ThinkingCard[];
};

const DEFAULT_RENDERED_MESSAGES = 180;
const DEFAULT_ARTIFACT_SCAN_LIMIT = 80;
const DEFAULT_MESSAGE_PREVIEW_CHARS = 12_000;
const DEFAULT_STREAM_CHARS_PER_SECOND = 36;
const DEFAULT_THINKING_MIN_VISIBLE_MS = 1800;
const DEFAULT_BOTTOM_FOLLOW_THRESHOLD_PX = 180;
const DEFAULT_ACTIVE_POLL_INTERVAL_MS = 1500;
const DEFAULT_IDLE_POLL_INTERVAL_MS = 3000;
type ConversationScrollMemory = {
  top: number;
  anchorMessageId?: string;
  anchorOffset?: number;
};

type NativeFileDropPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
  position?: { x: number; y: number };
  windowLabel?: string;
};

const conversationScrollPositionCache = new Map<string, ConversationScrollMemory>();
const RUNNING_TOOL_STARTED_AT = "__runningToolStartedAt";

function fileNameFromLocalPath(path: string) {
  return path.split(/[\\/]/).pop() || "attachment";
}

function hasFileDragData(dataTransfer: DataTransfer | null) {
  if (!dataTransfer) return false;
  if (dataTransfer.files.length > 0) return true;
  return Array.from(dataTransfer.types).includes("Files")
    || Array.from(dataTransfer.items).some((item) => item.kind === "file");
}

function clampCount(value: number | undefined, fallback: number, min: number, max: number) {
  if (!Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(value ?? fallback)));
}

function previewText(text: string, limit: number) {
  if (text.length <= limit) return text;
  return `${text.slice(0, limit)}\n\n[内容过长，界面仅预览前 ${limit} 个字符；复制按钮仍会复制完整消息。]`;
}

function composerErrorText(error: unknown) {
  const raw = error instanceof Error
    ? error.message
    : typeof error === "string"
      ? error
      : String(error ?? "");
  const text = raw.replace(/^bad request:\s*/i, "").trim();
  if (!text) return "发送失败。";
  return `发送失败：${text.length > 80 ? `${text.slice(0, 80)}...` : text}`;
}

async function playVoiceArtifact(path: string) {
  if (!isTauri()) return false;
  try {
    await api.playChatAudio?.(path);
    return true;
  } catch (error) {
    console.warn("chat voice playback failed, falling back to web audio:", error);
    return false;
  }
}

function normalizeToolDetailText(text: string) {
  return text.trim().replace(/\s+/g, " ");
}

function estimateMessageTokens(text: string): number {
  if (!text) return 0;
  let tokens = 0;
  const chars = Array.from(text);
  let i = 0;
  while (i < chars.length) {
    const ch = chars[i];
    const code = ch.codePointAt(0)!;
    if (/\s/.test(ch)) {
      tokens += 0.25;
      i++;
    } else if (/[a-zA-Z]/.test(ch)) {
      let start = i;
      while (i < chars.length && /[a-zA-Z]/.test(chars[i])) i++;
      tokens += Math.ceil((i - start) / 3.5) || 1;
    } else if (/\d/.test(ch)) {
      let start = i;
      while (i < chars.length && /\d/.test(chars[i])) i++;
      tokens += Math.ceil((i - start) / 2.5) || 1;
    } else if (code < 128) {
      tokens += 1;
      i++;
    } else {
      if ((code >= 0x4E00 && code <= 0x9FFF) ||
          (code >= 0x3400 && code <= 0x4DBF) ||
          (code >= 0xF900 && code <= 0xFAFF)) {
        tokens += 1.5;
      } else if ((code >= 0x3000 && code <= 0x303F) ||
                 (code >= 0xFF00 && code <= 0xFFEF)) {
        tokens += 1;
      } else {
        tokens += 2;
      }
      i++;
    }
  }
  return Math.max(1, Math.ceil(tokens));
}

function formatTokenK(tokens: number) {
  return `${Math.max(1, Math.round(tokens / 1000))}K`;
}

function useRevealedText(
  text: string,
  enabled: boolean,
  charsPerSecond: number,
  onDone?: () => void
) {
  const [visibleText, setVisibleText] = useState(enabled ? "" : text);
  const targetTextRef = useRef(text);
  const onDoneRef = useRef(onDone);
  const completedTextRef = useRef("");
  const visibleCountRef = useRef(enabled ? 0 : text.length);

  useEffect(() => {
    if (!enabled) {
      targetTextRef.current = text;
      completedTextRef.current = text;
      visibleCountRef.current = text.length;
      setVisibleText(text);
      return;
    }
    targetTextRef.current = text;
    if (!text) {
      completedTextRef.current = "";
      visibleCountRef.current = 0;
      setVisibleText("");
      onDoneRef.current?.();
      return;
    }
    setVisibleText((current) => {
      const next = text.startsWith(current) ? current : "";
      visibleCountRef.current = next.length;
      if (next && next.length >= text.length && completedTextRef.current !== text) {
        completedTextRef.current = text;
        window.setTimeout(() => onDoneRef.current?.(), 0);
      }
      return next;
    });
  }, [enabled, text]);

  useEffect(() => {
    onDoneRef.current = onDone;
  }, [onDone]);

  useEffect(() => {
    if (!enabled) return;
    const stepMs = 48;
    let lastTickAt = performance.now();
    const timer = window.setInterval(() => {
      const now = performance.now();
      const elapsedSeconds = Math.max(0.016, (now - lastTickAt) / 1000);
      lastTickAt = now;
      const charsPerStep = Math.max(1, Math.ceil(charsPerSecond * elapsedSeconds));
      setVisibleText((current) => {
        const target = targetTextRef.current;
        visibleCountRef.current = Math.max(current.length, visibleCountRef.current);
        const nextCount = Math.min(target.length, visibleCountRef.current + charsPerStep);
        visibleCountRef.current = nextCount;
        const next = target.slice(0, nextCount);
        if (nextCount >= target.length && completedTextRef.current !== target) {
          completedTextRef.current = target;
          window.setTimeout(() => onDoneRef.current?.(), 0);
        }
        return next;
      });
    }, stepMs);
    return () => {
      window.clearInterval(timer);
    };
  }, [charsPerSecond, enabled]);

  return visibleText;
}

function parseToolEvent(content: string): ToolEvent | null {
  try {
    const parsed = JSON.parse(content) as Partial<ToolEventEnvelope>;
    if (parsed?.type === "toolEvent" && parsed.event) return parsed.event;
  } catch {
    return null;
  }
  return null;
}

function toolEventStartKey(event: Pick<ToolEvent, "callId" | "referenceId" | "serverId" | "toolName">) {
  if (event.callId) return `call:${event.callId}`;
  if (event.referenceId) return `ref:${event.referenceId}`;
  return `${event.serverId}.${event.toolName}`;
}

function toolEventStartedAt(event: ToolEvent): string | null {
  const raw = event.raw as Record<string, unknown> | null | undefined;
  const value = raw?.[RUNNING_TOOL_STARTED_AT];
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function withToolEventStartedAt(event: ToolEvent, startedAt?: string | null): ToolEvent {
  if (!startedAt || event.status !== "running" || toolEventStartedAt(event)) return event;
  const raw = event.raw && typeof event.raw === "object" && !Array.isArray(event.raw)
    ? event.raw as Record<string, unknown>
    : {};
  return {
    ...event,
    raw: {
      ...raw,
      [RUNNING_TOOL_STARTED_AT]: startedAt
    }
  };
}

function parseManagedProcessEvent(content: string): ManagedProcessEvent | null {
  try {
    const parsed = JSON.parse(content) as { type?: string; event?: ManagedProcessEvent };
    if (parsed?.type === "managedProcessEvent" && parsed.event) return parsed.event;
  } catch {
    return null;
  }
  return null;
}

function formatTime(value?: string | number | null) {
  if (!value) return "";
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

function formatDurationMs(value: number) {
  const ms = Math.max(0, Math.floor(value));
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const minutes = Math.floor(ms / 60_000);
  const seconds = Math.floor((ms % 60_000) / 1000);
  return `${minutes}m ${seconds.toString().padStart(2, "0")}s`;
}

function runStateLabel(state: string) {
  const labels: Record<string, string> = {
    pending: "排队中",
    started: "任务已启动",
    planning: "正在规划",
    running: "正在思考",
    running_tool: "执行中...",
    tool_completed: "成功",
    pendingApproval: "等待审批",
    finalizing: "正在整理",
    completed: "已完成",
    failed: "失败",
    aborted: "已停止"
  };
  return labels[state] ?? state;
}

function runPhaseLabel(phase: string) {
  const labels: Record<string, string> = {
    planner_started: "开始规划",
    planner_decision: "规划决策",
    approval_required: "等待审批",
    tool_started: "执行中...",
    tool_message_recorded: "成功",
    tool_batch_started: "执行中...",
    tool_batch_completed: "成功",
    steer_injected: "用户补充已注入",
    subagent_started: "子任务启动",
    subagent_completed: "子任务完成",
    subagent_failed: "子任务失败",
    subagent_aborted: "子任务已停止",
    acp_session_update: "ACP 工具更新",
    acp_permission_decision: "ACP 权限决策",
    memory_delegation_observed: "委派观察记录",
    llm_retry: "模型请求重试",
    llm_failover: "模型故障切换",
    llm_recovery: "模型错误恢复",
    llm_preflight_compaction: "上下文预压缩",
    finalizing: "整理结果"
  };
  return labels[phase] ?? phase;
}

function isTerminalRunState(state?: string | null) {
  return state === "completed" || state === "failed" || state === "aborted";
}

function compactRunText(value?: string | null, limit = 120) {
  const text = value?.trim() ?? "";
  if (!text) return "";
  return text.length > limit ? `${text.slice(0, limit)}...` : text;
}

function queueStatusLabel(status: string) {
  const labels: Record<string, string> = {
    pending: "排队中",
    running: "执行中",
    completed: "已完成",
    failed: "失败",
    canceled: "已取消"
  };
  return labels[status] ?? status;
}

function shortRuntimeId(value?: string | null) {
  if (!value) return "";
  const text = value.trim();
  if (text.length <= 14) return text;
  const parts = text.split("-");
  const prefix = parts[0] || "id";
  return `${prefix}-${text.slice(-8)}`;
}

function subagentTitle(run: AgentRunRecord) {
  const index = typeof run.subagentIndex === "number" ? `#${run.subagentIndex}` : "";
  const role = run.subagentRole?.trim() || "subagent";
  return [index, role].filter(Boolean).join(" ");
}

function managedProcessEventLabel(type: string) {
  const labels: Record<string, string> = {
    completed: "进程完成",
    stopped: "进程已停止",
    watch_match: "进程输出匹配",
    watch_disabled: "进程观察已降级"
  };
  return labels[type] ?? type;
}

function managedProcessEventText(event: ManagedProcessEvent) {
  const detail = event.detail ?? {};
  const parts = [
    event.label || event.processId,
    typeof detail.exitCode === "number" ? `exit ${detail.exitCode}` : "",
    typeof detail.pattern === "string" ? `匹配 ${detail.pattern}` : "",
    typeof detail.stream === "string" ? detail.stream : "",
    typeof detail.line === "string" ? detail.line : "",
    typeof detail.reason === "string" ? detail.reason : ""
  ].filter(Boolean);
  return parts.join(" · ");
}

function runtimeEventTime(event: AgentRuntimeEvent) {
  return event.createdAt ?? event.created_at ?? "";
}

function runtimeEventText(event: AgentRuntimeEvent) {
  const runId = event.runId ?? event.run_id;
  const queueItemId = event.queueItemId ?? event.queue_item_id;
  const taskId = event.taskId ?? event.task_id;
  const processId = event.processId ?? event.process_id;
  return [
    event.kind,
    event.status,
    taskId ? `task ${shortRuntimeId(taskId)}` : "",
    runId ? `run ${shortRuntimeId(runId)}` : "",
    queueItemId ? `queue ${shortRuntimeId(queueItemId)}` : "",
    processId ? `process ${shortRuntimeId(processId)}` : "",
    event.source
  ].filter(Boolean).join(" · ");
}

function phaseDetailText(detail: unknown) {
  if (!detail || typeof detail !== "object") return "";
  const data = detail as Record<string, unknown>;
  const serverTool = typeof data.serverId === "string" && typeof data.toolName === "string"
    ? `${data.serverId}.${data.toolName}`
    : "";
  const acpUpdates = (Array.isArray(data.acpSessionUpdates) ? data.acpSessionUpdates.length : 0) + (data.update ? 1 : 0);
  const permissionDecisions = (Array.isArray(data.permissionDecisions) ? data.permissionDecisions.length : 0) + (data.decision ? 1 : 0);
  const parts = [
    typeof data.iteration === "number" ? `#${data.iteration}` : "",
    serverTool,
    typeof data.tool === "string" ? data.tool : "",
    typeof data.providerId === "string" ? data.providerId : "",
    typeof data.kind === "string" ? data.kind : "",
    typeof data.status === "string" ? data.status : "",
    typeof data.count === "number" ? `${data.count} calls` : "",
    typeof data.observationCount === "number" ? `${data.observationCount} observations` : "",
    acpUpdates > 0 ? `${acpUpdates} ACP updates` : "",
    permissionDecisions > 0 ? `${permissionDecisions} permissions` : "",
    typeof data.note === "string" ? data.note : "",
    typeof data.message === "string" ? data.message : "",
    typeof data.summaryTokens === "number" ? `${data.summaryTokens} tokens` : ""
  ].filter(Boolean);
  return parts.join(" · ");
}

function acpUpdateLinesFromDetail(detail: unknown) {
  if (!detail || typeof detail !== "object") return [];
  const data = detail as Record<string, unknown>;
  const updates = [
    ...(Array.isArray(data.acpSessionUpdates) ? data.acpSessionUpdates : []),
    data.update
  ].filter(Boolean);
  const permissions = [
    ...(Array.isArray(data.permissionDecisions) ? data.permissionDecisions : []),
    data.decision
  ].filter(Boolean);
  const updateLines = updates.map((item) => {
    if (!item || typeof item !== "object") return "";
    const update = item as Record<string, unknown>;
    const kind = typeof update.sessionUpdate === "string"
      ? update.sessionUpdate
      : typeof update.session_update === "string"
        ? update.session_update
        : "update";
    if (kind === "tool_call" || kind === "tool_call_update") {
      const title = typeof update.title === "string" && update.title.trim() ? update.title.trim() : "tool";
      const status = typeof update.status === "string" && update.status.trim() ? update.status.trim() : (kind === "tool_call" ? "started" : "updated");
      const rawCallId = typeof update.toolCallId === "string"
        ? update.toolCallId
        : typeof update.tool_call_id === "string"
          ? update.tool_call_id
          : "";
      const callId = rawCallId.trim() ? ` · ${rawCallId.trim()}` : "";
      return `${kind === "tool_call" ? "ACP 工具启动" : "ACP 工具更新"} · ${title} · ${status}${callId}`;
    }
    if (kind === "plan") {
      const entries = Array.isArray(update.entries) ? update.entries : [];
      const active = entries
        .map((entry) => entry && typeof entry === "object" ? entry as Record<string, unknown> : null)
        .filter((entry) => entry && entry.status !== "completed")
        .slice(0, 2)
        .map((entry) => typeof entry?.content === "string" ? entry.content : "")
        .filter(Boolean);
      return `ACP 计划更新 · ${entries.length} 项${active.length ? ` · ${active.join(" / ")}` : ""}`;
    }
    if (kind === "available_commands_update") {
      const count = typeof update.availableCommandCount === "number" ? update.availableCommandCount : 0;
      return `ACP 可用命令 · ${count}`;
    }
    if (kind === "queue_update") {
      const status = typeof update.status === "string" && update.status.trim() ? queueStatusLabel(update.status.trim()) : "队列更新";
      const queueId = typeof update.queueId === "string"
        ? update.queueId
        : typeof update.queue_id === "string"
          ? update.queue_id
          : "";
      const position = typeof update.position === "number" && update.position > 0 ? `#${update.position}` : "";
      const pendingCount = typeof update.pendingCount === "number"
        ? `${update.pendingCount} pending`
        : typeof update.pending_count === "number"
          ? `${update.pending_count} pending`
          : "";
      const activeRunId = typeof update.activeRunId === "string"
        ? update.activeRunId
        : typeof update.active_run_id === "string"
          ? update.active_run_id
          : "";
      return ["ACP 队列", status, position, pendingCount, activeRunId, queueId].filter(Boolean).join(" · ");
    }
    return `ACP ${kind}`;
  }).filter(Boolean);
  const permissionLines = permissions.map((item) => {
    if (!item || typeof item !== "object") return "";
    const decision = item as Record<string, unknown>;
    const outcome = typeof decision.outcome === "string" ? decision.outcome : "";
    const optionId = typeof decision.optionId === "string" ? decision.optionId : "";
    const params = decision.params && typeof decision.params === "object" ? decision.params as Record<string, unknown> : {};
    const toolCall = (
      (params.toolCall && typeof params.toolCall === "object" ? params.toolCall : null) ||
      (params.tool_call && typeof params.tool_call === "object" ? params.tool_call : null)
    ) as Record<string, unknown> | null;
    const rawInput = toolCall?.rawInput && typeof toolCall.rawInput === "object"
      ? toolCall.rawInput as Record<string, unknown>
      : toolCall?.raw_input && typeof toolCall.raw_input === "object"
        ? toolCall.raw_input as Record<string, unknown>
        : null;
    const title = typeof toolCall?.title === "string" && toolCall.title.trim()
      ? toolCall.title.trim()
      : typeof rawInput?.command === "string" && rawInput.command.trim()
        ? rawInput.command.trim()
        : typeof rawInput?.description === "string" && rawInput.description.trim()
          ? rawInput.description.trim()
          : "";
    const label = decision.decision === "approved" ? "ACP 权限自动允许" : "ACP 权限自动取消";
    return [label, title, outcome, optionId].filter(Boolean).join(" · ");
  }).filter(Boolean);
  return [...updateLines, ...permissionLines];
}

function eventStatusLabel(event: ToolEvent) {
  if (event.status === "running") return "执行中...";
  if (event.ok) return "成功";
  return "失败";
}

function isCanceledToolEvent(event: ToolEvent) {
  return event.status === "canceled" || event.status === "cancelled";
}

function materializeToolEvent(event: ToolEvent, terminalRunState?: string | null): ToolEvent {
  if (event.status !== "running" || !isTerminalRunState(terminalRunState)) return event;
  const summary = terminalRunState === "completed"
    ? "运行已完成，工具调用已结束"
    : terminalRunState === "aborted"
      ? "运行已取消，工具调用已结束"
      : "运行已失败，工具调用已结束";
  return {
    ...event,
    status: terminalRunState === "failed" ? "failed" : "canceled",
    ok: false,
    summary,
    error: summary
  };
}

function toolEventRank(event: ToolEvent) {
  if (isCanceledToolEvent(event)) return -1;
  if (event.status === "running") return 0;
  if (!event.ok || event.status === "failed") return 2;
  return 3;
}

function eventKey(event: ToolEvent, index: number) {
  if (event.callId) return `call:${event.callId}`;
  if (event.referenceId) return `ref:${event.referenceId}`;
  return `${event.serverId}:${event.toolName}:${event.elapsedMs}:${index}`;
}

function toolEventMessageKey(event: ToolEvent) {
  if (event.callId) return `call:${event.callId}`;
  if (event.referenceId) return `ref:${event.referenceId}`;
  return `${event.serverId}.${event.toolName}`;
}

function selectVisibleToolEvents(events: ToolEvent[]) {
  const selected = new Map<string, { index: number; event: ToolEvent }>();
  const suppressed = new Set<string>();
  events.forEach((event, index) => {
    const key = toolEventMessageKey(event);
    if (isCanceledToolEvent(event)) {
      selected.delete(key);
      suppressed.add(key);
      return;
    }
    suppressed.delete(key);
    const previous = selected.get(key);
    if (
      !previous
      || toolEventRank(event) > toolEventRank(previous.event)
      || (
        toolEventRank(event) === toolEventRank(previous.event)
        && index > previous.index
      )
    ) {
      selected.set(key, { index, event });
    }
  });
  return Array.from(selected.values())
    .filter((item) => !suppressed.has(toolEventMessageKey(item.event)))
    .sort((a, b) => a.index - b.index)
    .map((item) => item.event);
}

function runningToolStartTimesFromPhases(phases: AgentRunPhase[] | undefined | null) {
  const starts = new Map<string, string>();
  for (const phase of phases ?? []) {
    if (phase.phase !== "tool_started" && phase.phase !== "tool_batch_started") continue;
    const detail = phase.detail && typeof phase.detail === "object" ? phase.detail as Record<string, unknown> : {};
    const serverId = typeof detail.serverId === "string" ? detail.serverId : "";
    const toolName = typeof detail.toolName === "string" ? detail.toolName : "";
    const callId = typeof detail.callId === "string" ? detail.callId : "";
    const referenceId = typeof detail.referenceId === "string" ? detail.referenceId : "";
    if (!serverId || !toolName) continue;
    starts.set(toolEventStartKey({ callId, referenceId, serverId, toolName }), phase.updatedAt);
  }
  return starts;
}

function agentLabel(agent: AgentDefinition | null | undefined) {
  if (!agent) return "Default Agent";
  return agent.name || agent.id || "Agent";
}

function fileNameFromPath(path: string) {
  return path.split(/[\\/]/).pop() || path;
}

function normalizeEmojiPathKey(path: string): string {
  return path.replace(/\//g, "\\").toLowerCase();
}

function isEmojiAssetPath(path: string): boolean {
  return normalizeEmojiPathKey(path).includes("\\emoji\\");
}

function buildEmojiPathIndexes(groups: EmojiGroup[]): EmojiPathIndexes {
  const byPath = new Map<string, string>();
  const byFile = new Map<string, string>();
  for (const group of groups) {
    const imagePaths = Object.values(group.emotionImages ?? {}).flat();
    const candidates = imagePaths.length > 0 ? imagePaths : group.images;
    for (const imagePath of candidates) {
      byPath.set(normalizeEmojiPathKey(imagePath), imagePath);
      const normalized = normalizeEmojiPathKey(imagePath);
      const markerIndex = normalized.indexOf("\\emoji\\");
      if (markerIndex < 0) continue;
      const segments = normalized.slice(markerIndex + "\\emoji\\".length).split("\\").filter(Boolean);
      if (segments.length < 3) continue;
      const [groupId, emotionId, fileName] = segments;
      byFile.set(`${groupId}::${emotionId}::${fileName}`, imagePath);
    }
  }
  return { byPath, byFile };
}

function repairEmojiAssetPath(path: string, indexes: EmojiPathIndexes): string {
  const normalized = normalizeEmojiPathKey(path);
  const exact = indexes.byPath.get(normalized);
  if (exact) return exact;
  const marker = "\\emoji\\";
  const markerIndex = normalized.indexOf(marker);
  if (markerIndex < 0) return path;
  const segments = normalized.slice(markerIndex + marker.length).split("\\").filter(Boolean);
  if (segments.length < 3) return path;
  const [groupId, emotionId, fileName] = segments;
  return indexes.byFile.get(`${groupId}::${emotionId}::${fileName}`) ?? path;
}

function artifactKind(path: string, mimeType?: string | null): ArtifactTarget["kind"] {
  const lower = path.toLowerCase();
  if (mimeType?.startsWith("image/")) return "image";
  if (/\.(png|jpe?g|webp|gif|bmp|svg)$/i.test(lower)) return "image";
  return "file";
}

function extractArtifactPaths(text: string): ArtifactTarget[] {
  const targets: ArtifactTarget[] = [];
  const seen = new Set<string>();
  const push = (path: string, source: string) => {
    const clean = path.replace(/[，。；;,.!?]+$/u, "");
    if (!clean || seen.has(clean)) return;
    seen.add(clean);
    targets.push({ path: clean, title: fileNameFromPath(clean), kind: artifactKind(clean), source });
  };
  const mediaMarker = /\[media attached:\s*(?:"([^"]+)"|`([^`]+)`|([^\]\(]+?))\s*(?:\(([^)]+)\))?\]/gi;
  let match: RegExpExecArray | null;
  while ((match = mediaMarker.exec(text)) !== null) {
    const path = (match[1] || match[2] || match[3] || "").trim();
    const mimeType = (match[4] || "").trim();
    const clean = path.replace(/[，。；;,.!?]+$/u, "");
    if (!clean || seen.has(clean)) continue;
    seen.add(clean);
    targets.push({ path: clean, title: fileNameFromPath(clean), kind: artifactKind(clean, mimeType), source: "message" });
  }
  const mediaTag = /(?:^|\n)\s*`?MEDIA:\s*(?:"([^"\n]+)"|'([^'\n]+)'|`([^`\n]+)`|([A-Za-z]:[\\/][^\n]+|\/[^\n]+|~\/[^\n]+))`?/gi;
  while ((match = mediaTag.exec(text)) !== null) {
    push((match[1] || match[2] || match[3] || match[4] || "").trim(), "message");
  }
  const tagged = /(?:MEDIA|media|文件|路径|保存到|saved(?: at| to)?)[：:\s]+[`"]?((?:[A-Za-z]:\\|\/|~\/)[^\s`"'<>]+)[`"]?/g;
  while ((match = tagged.exec(text)) !== null) push(match[1], "message");
  const direct = /(?<![\w./:])((?:[A-Za-z]:\\|\/|~\/)[^\s`"'<>]+\.(?:png|jpg|jpeg|webp|gif|bmp|svg|html?|md|txt|json|pdf|xlsx?|csv|zip))/gi;
  while ((match = direct.exec(text)) !== null) push(match[1], "message");
  return targets;
}

const MessageList = memo(function MessageList({
  messages,
  profileName,
  profileAvatar,
  personaName,
  personaAvatar,
  copiedMessageId,
  onCopy,
  previewCharLimit,
  onFirstStreamChar,
  animatedMessageIds,
  streamCharsPerSecond,
  onMessageAnimationDone,
  memoryStats,
  runStates,
  emojiPathIndexes
}: {
  messages: ChatMessage[];
  profileName: string;
  profileAvatar: string;
  personaName: string;
  personaAvatar: string;
  copiedMessageId: string | null;
  onCopy: (message: ChatMessage) => void;
  previewCharLimit: number;
  onFirstStreamChar?: () => void;
  animatedMessageIds: Set<string>;
  streamCharsPerSecond: number;
  onMessageAnimationDone: (messageId: string) => void;
  memoryStats: Map<string, ShortMemoryMessageStat>;
  runStates: Map<string, string>;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const renderItems = useMemo(() => {
    const sliced = messages;
    const selectedToolMessages = new Map<string, { index: number; event: ToolEvent; message: ChatMessage }>();
    const toolKeys = new Map<string, string>();
    const suppressedToolKeys = new Set<string>();
    for (let i = 0; i < sliced.length; i++) {
      const msg = sliced[i];
      if (msg.role !== "tool") continue;
      const evt = parseToolEvent(msg.content);
      if (!evt) continue;
      const materialized = materializeToolEvent(evt, evt.runId ? runStates.get(evt.runId) : null);
      const key = toolEventMessageKey(materialized);
      toolKeys.set(msg.id, key);
      if (isCanceledToolEvent(materialized)) {
        selectedToolMessages.delete(key);
        suppressedToolKeys.add(key);
        continue;
      }
      suppressedToolKeys.delete(key);
      const previous = selectedToolMessages.get(key);
      if (
        !previous
        || toolEventRank(materialized) > toolEventRank(previous.event)
        || (
          toolEventRank(materialized) === toolEventRank(previous.event)
          && i > previous.index
        )
      ) {
        selectedToolMessages.set(key, { index: i, event: materialized, message: msg });
      }
    }
    const deduped: typeof sliced = [];
    for (let i = 0; i < sliced.length; i++) {
      const msg = sliced[i];
      if (msg.role === "tool") {
        const key = toolKeys.get(msg.id);
        if (key && suppressedToolKeys.has(key)) continue;
        if (key) {
          const selected = selectedToolMessages.get(key);
          if (!selected || selected.message.id !== msg.id) continue;
        }
      }
      deduped.push(msg);
    }
    return materializeMessageRenderItems(deduped);
  }, [messages, runStates]);
  return (
    <>
      {renderItems.map((item) => (
        <MessageRow
          key={item.key}
          message={item.message}
          mode={item.mode}
          elementId={item.elementId}
          thinkingCardsOverride={item.cards}
          profileName={profileName}
          profileAvatar={profileAvatar}
          personaName={personaName}
          personaAvatar={personaAvatar}
          copied={item.mode !== "thinking" && copiedMessageId === item.message.id}
          onCopy={() => onCopy(item.message)}
          previewCharLimit={previewCharLimit}
          onFirstStreamChar={item.mode === "thinking" ? undefined : onFirstStreamChar}
          animateText={item.mode !== "thinking" && animatedMessageIds.has(item.message.id)}
          streamCharsPerSecond={streamCharsPerSecond}
          onAnimationDone={() => {
            if (item.mode !== "thinking") onMessageAnimationDone(item.message.id);
          }}
          memoryStat={item.mode === "thinking" ? null : memoryStats.get(item.message.id) ?? null}
          runStates={runStates}
          emojiPathIndexes={emojiPathIndexes}
        />
      ))}
    </>
  );
});

function providerModelOptions(providers: LlmProvider[]) {
  return providers
    .filter((provider) => provider.enabled)
    .map((provider) => ({
      key: `${provider.id}::${provider.model}`,
      providerId: provider.id,
      model: provider.model,
      label: provider.model || "未配置模型"
    }));
}

function recordValue(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function arrayValue(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function thinkingCardsFromProviderData(providerData: unknown): ThinkingCard[] {
  const root = recordValue(providerData);
  if (!root) return [];
  const candidates = [
    ...arrayValue(root.thinkingCards),
    ...arrayValue(recordValue(root.responses)?.thinkingCards),
    ...arrayValue(recordValue(root.anthropic)?.thinkingCards)
  ];
  return candidates
    .map((item, index) => {
      const card = recordValue(item);
      if (!card) return null;
      const summary = typeof card.summary === "string" ? card.summary.trim() : "";
      const redacted = card.redacted === true;
      const encrypted = card.encrypted === true || card.signature === true;
      const streaming = card.streaming === true;
      if (!summary && !redacted && !encrypted) return null;
      const provider = typeof card.provider === "string" && card.provider.trim() ? card.provider.trim() : "";
      const kind = typeof card.kind === "string" && card.kind.trim() ? card.kind.trim() : "thinking";
      const title = typeof card.title === "string" && card.title.trim() ? card.title.trim() : "模型思考";
      return {
        key: `${provider || "provider"}:${kind}:${index}`,
        provider,
        kind,
        title,
        summary,
        redacted,
        encrypted,
        streaming
      };
    })
    .filter((card): card is ThinkingCard => card !== null);
}

function messageThinkingCards(message: ChatMessage) {
  return thinkingCardsFromProviderData(message.providerData);
}

function stripThinkingCardsFromText(text: string, cards: ThinkingCard[]): string {
  let output = text;
  for (const card of cards) {
    const summary = card.summary.trim();
    if (summary.length < 8) continue;
    output = output.split(summary).join("");
  }
  return output
    .split(/\n/)
    .map((line) => line.trimEnd())
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

function visibleMessageText(message: ChatMessage): string {
  const base = message.content.trim();
  if (message.role === "user" || message.role === "tool" || message.role === "system") return base;
  const cards = messageThinkingCards(message);
  return cards.length > 0 ? stripThinkingCardsFromText(base, cards) : base;
}

function messageRenderItem(message: ChatMessage, mode: MessageRenderMode = "normal", cards?: ThinkingCard[]): MessageRenderItem {
  const suffix = mode === "normal" ? "" : `:${mode}`;
  return {
    key: `${message.id}${suffix}`,
    elementId: `${message.id}${suffix}`,
    message,
    mode,
    cards
  };
}

function materializeMessageRenderItem(message: ChatMessage): MessageRenderItem[] {
  if (message.role === "tool") {
    const cards = messageThinkingCards(message);
    return cards.length > 0
      ? [messageRenderItem(message, "thinking", cards), messageRenderItem(message)]
      : [messageRenderItem(message)];
  }
  if (message.role !== "assistant") return [messageRenderItem(message)];
  const cards = messageThinkingCards(message);
  if (cards.length === 0) return [messageRenderItem(message)];
  const items = [messageRenderItem(message, "thinking", cards)];
  if (visibleMessageText(message)) {
    items.push(messageRenderItem(message, "content"));
  }
  return items;
}

function thinkingCardsSignature(cards: ThinkingCard[]) {
  return cards
    .map((card) => [
      card.provider,
      card.kind,
      card.summary.trim(),
      card.redacted ? "redacted" : "",
      card.encrypted ? "encrypted" : ""
    ].filter(Boolean).join(":"))
    .filter(Boolean)
    .join("|");
}

function materializeMessageRenderItems(messages: ChatMessage[]): MessageRenderItem[] {
  const items: MessageRenderItem[] = [];
  let lastThinkingSignature = "";
  let previousItemWasThinking = false;
  for (const message of messages) {
    const nextItems = materializeMessageRenderItem(message);
    const first = nextItems[0];
    if (first?.mode === "thinking") {
      const signature = thinkingCardsSignature(first.cards ?? []);
      if (
        signature
        && (
          previousItemWasThinking
          || (first.message.role === "tool" && signature === lastThinkingSignature)
        )
      ) {
        nextItems.shift();
      }
    }
    for (const item of nextItems) {
      items.push(item);
      if (item.mode === "thinking") {
        lastThinkingSignature = thinkingCardsSignature(item.cards ?? []);
        previousItemWasThinking = true;
      } else {
        previousItemWasThinking = false;
        if (item.message.role !== "tool") {
          lastThinkingSignature = "";
        }
      }
    }
  }
  return items;
}

export const ChatExperience = memo(function ChatExperience() {
  const activeConversationId = useAppStore((state) => state.activeConversationId);
  const conversations = useAppStore((state) => state.conversations);
  const messages = useAppStore((state) => state.messages);
  const processingConversationIds = useAppStore((state) => state.processingConversationIds);
  const activeSection = useAppStore((state) => state.activeSection);
  const conversationUnreadCounts = useAppStore((state) => state.conversationUnreadCounts);
  const activeAgentRuns = useAppStore((state) => state.activeAgentRuns);
  const agentQueue = useAppStore((state) => state.agentQueue);
  const agentRuns = useAppStore((state) => state.agentRuns);
  const managedProcessEvents = useAppStore((state) => state.managedProcessEvents);
  const personas = useAppStore((state) => state.personas);
  const agents = useAppStore((state) => state.agents);
  const agentConfig = useAppStore((state) => state.agentConfig);
  const chatConfig = useAppStore((state) => state.config?.chat);
  const llmProviders = useAppStore((state) => state.llmProviders);
  const emojiGroups = useAppStore((state) => state.emojiGroups);
  const mcpServers = useAppStore((state) => state.mcpServers);
  const skills = useAppStore((state) => state.skills);
  const profile = useAppStore((state) => state.profile);
  const createConversation = useAppStore((state) => state.createConversation);
  const deleteConversation = useAppStore((state) => state.deleteConversation);
  const refreshMemories = useAppStore((state) => state.refreshMemories);
  const selectConversation = useAppStore((state) => state.selectConversation);
  const sendMessage = useAppStore((state) => state.sendMessage);
  const setConversationProcessing = useAppStore((state) => state.setConversationProcessing);
  const incrementConversationUnread = useAppStore((state) => state.incrementConversationUnread);
  const markConversationRead = useAppStore((state) => state.markConversationRead);
  const setSection = useAppStore((state) => state.setSection);
  const setFocusedAgentId = useAppStore((state) => state.setFocusedAgentId);
  const setSkillsPanelMode = useAppStore((state) => state.setSkillsPanelMode);
  const setMcpPanelMode = useAppStore((state) => state.setMcpPanelMode);
  const refreshChatData = useAppStore((state) => state.refreshChatData);
  const loadOlderMessages = useAppStore((state) => state.loadOlderMessages);
  const refreshAgents = useAppStore((state) => state.refreshAgents);
  const refreshSkills = useAppStore((state) => state.refreshSkills);
  const refreshMcpServers = useAppStore((state) => state.refreshMcpServers);
  const refreshAgentQueue = useAppStore((state) => state.refreshAgentQueue);
  const refreshAgentRuns = useAppStore((state) => state.refreshAgentRuns);
  const savePersona = useAppStore((state) => state.savePersona);
  const [draft, setDraft] = useState("");
  const [composerError, setComposerError] = useState<string | null>(null);
  const [controlCommands, setControlCommands] = useState<AgentControlCommand[]>([]);
  const [selectedSlashCommandIndex, setSelectedSlashCommandIndex] = useState(0);
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const [selectedPersonaId, setSelectedPersonaId] = useState("");
  const [attachments, setAttachments] = useState<ComposerAttachment[]>([]);
  const [emojiPickerOpen, setEmojiPickerOpen] = useState(false);
  const [pickerEmojiGroups, setPickerEmojiGroups] = useState(emojiGroups);
  const [dragActive, setDragActive] = useState(false);
  const [voiceInputState, setVoiceInputState] = useState<VoiceInputState>("idle");
  const [voiceSupported, setVoiceSupported] = useState(true);
  const [previewTarget, setPreviewTarget] = useState<ArtifactTarget | null>(null);
  const [copiedMessageId, setCopiedMessageId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const chatShellRef = useRef<HTMLElement>(null);
  const chatMainRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLElement>(null);
  const lastNativeDropRef = useRef<{ signature: string; at: number } | null>(null);
  const speechRecognitionRef = useRef<SpeechRecognitionLike | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const voiceChunksRef = useRef<Blob[]>([]);
  const voiceAudioRef = useRef<HTMLAudioElement | null>(null);
  const spokenAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const activeVoiceReplyRequestRef = useRef<string | null>(null);
  const sendingRef = useRef(false);
  const [isNearBottom, setIsNearBottom] = useState(true);
  const [unreadCount, setUnreadCount] = useState(0);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [historyExhausted, setHistoryExhausted] = useState(false);
  const seenMessageContentRef = useRef<Map<string, string>>(new Map());
  const [animatedMessageIds, setAnimatedMessageIds] = useState<Set<string>>(() => new Set());
  const [settlingConversationId, setSettlingConversationId] = useState<string | null>(null);
  const [executionPanelOpen, setExecutionPanelOpen] = useState(false);
  const [timelineCollapsed, setTimelineCollapsed] = useState(false);
  const [artifactsCollapsed, setArtifactsCollapsed] = useState(true);
  const [skillsCollapsed, setSkillsCollapsed] = useState(true);
  const [compactionTipVisible, setCompactionTipVisible] = useState(false);
  const [compactionRoundTokens, setCompactionRoundTokens] = useState(0);
  const [runtimeEvents, setRuntimeEvents] = useState<AgentRuntimeEvent[]>([]);
  const [runtimeCursor, setRuntimeCursor] = useState(0);
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);

  useEffect(() => {
    void Promise.all([refreshAgents(), refreshSkills(), refreshMcpServers(), refreshAgentRuns(), refreshAgentQueue()]);
  }, [refreshAgentQueue, refreshAgentRuns, refreshAgents, refreshMcpServers, refreshSkills]);

  useEffect(() => {
    let cancelled = false;
    void api.listAgentControlCommands().then((commands) => {
      if (!cancelled) setControlCommands(commands);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    setPickerEmojiGroups(emojiGroups);
  }, [emojiGroups]);

  useEffect(() => {
    setRuntimeEvents([]);
    setRuntimeCursor(0);
  }, [activeConversationId]);

  useEffect(() => {
    if (!emojiPickerOpen) return;
    let cancelled = false;
    void api.listEmojiGroups().then((groups) => {
      if (!cancelled) setPickerEmojiGroups(groups);
    });
    return () => {
      cancelled = true;
    };
  }, [emojiPickerOpen]);

  useEffect(() => {
    if (!selectedPersonaId && personas[0]) setSelectedPersonaId(personas[0].id);
  }, [personas, selectedPersonaId]);

  const activeConversation = useMemo(
    () => conversations.find((item) => item.id === activeConversationId) ?? null,
    [activeConversationId, conversations]
  );
  useEffect(() => {
    if (activeConversation?.personaId && activeConversation.personaId !== selectedPersonaId) {
      setSelectedPersonaId(activeConversation.personaId);
    }
  }, [activeConversation?.personaId, selectedPersonaId]);

  const personaById = useMemo(() => new Map(personas.map((persona) => [persona.id, persona])), [personas]);
  const visiblePersonas = personas;
  const selectedPersona = visiblePersonas.find((persona) => persona.id === selectedPersonaId) ?? visiblePersonas[0] ?? null;
  const activeConversationPersona = useMemo(
    () => (activeConversation?.personaId ? personaById.get(activeConversation.personaId) ?? null : null),
    [activeConversation?.personaId, personaById]
  );
  const toolbarPersona = selectedPersona ?? activeConversationPersona;
  useEffect(() => {
    if (!selectedPersonaId && visiblePersonas[0]) {
      setSelectedPersonaId(visiblePersonas[0].id);
      return;
    }
    if (selectedPersonaId && !visiblePersonas.some((persona) => persona.id === selectedPersonaId)) {
      setSelectedPersonaId(visiblePersonas[0]?.id ?? "");
    }
  }, [selectedPersonaId, visiblePersonas]);
  const defaultAgent = useMemo(() => agents.find((agent) => agent.isDefault) ?? agents[0] ?? null, [agents]);
  const renderLimit = clampCount(chatConfig?.uiMessageLimit, DEFAULT_RENDERED_MESSAGES, 40, 1000);
  const artifactScanLimit = clampCount(chatConfig?.artifactScanLimit, DEFAULT_ARTIFACT_SCAN_LIMIT, 20, renderLimit);
  const previewCharLimit = clampCount(chatConfig?.uiMessagePreviewChars, DEFAULT_MESSAGE_PREVIEW_CHARS, 2000, 100_000);
  const streamCharsPerSecond = clampCount(chatConfig?.uiStreamCharsPerSecond, DEFAULT_STREAM_CHARS_PER_SECOND, 8, 160);
  const thinkingMinVisibleMs = clampCount(chatConfig?.thinkingMinVisibleMs, DEFAULT_THINKING_MIN_VISIBLE_MS, 0, 8000);
  const bottomFollowThresholdPx = clampCount(chatConfig?.bottomFollowThresholdPx, DEFAULT_BOTTOM_FOLLOW_THRESHOLD_PX, 24, 600);
  const activePollIntervalMs = clampCount(chatConfig?.activePollIntervalMs, DEFAULT_ACTIVE_POLL_INTERVAL_MS, 300, 30_000);
  const idlePollIntervalMs = clampCount(chatConfig?.idlePollIntervalMs, DEFAULT_IDLE_POLL_INTERVAL_MS, 1000, 120_000);

  useEffect(() => {
    setHistoryLoading(false);
    setHistoryExhausted(false);
    loadingHistoryRef.current = false;
    preserveTopOnHistoryLoadRef.current = null;
  }, [activeConversationId, renderLimit]);
  // Round-aware compaction tip: only count tokens/messages after the last summary boundary
  useEffect(() => {
    if (!activeConversationId) return;
    const dialogueMessages = messages.filter((m) => m.role === "user" || m.role === "assistant");
    if (dialogueMessages.length === 0) return;
    const mode = chatConfig?.shortContextMode === "tokens" ? "tokens" : "messages";
    const budget = clampCount(chatConfig?.shortContextTokenBudget, 8000, 500, 500_000);
    const messageLimit = clampCount(chatConfig?.maxContextRounds, 10, 1, 500);
    let cancelled = false;
    api.getShortContextState(activeConversationId).then((state) => {
      if (cancelled) return;
      let startIndex = 0;
      const boundaryId = state?.boundaryId ?? null;
      if (boundaryId) {
        const idx = dialogueMessages.findIndex((m) => m.id === boundaryId);
        if (idx >= 0) startIndex = idx + 1;
      }
      const roundMessages = dialogueMessages.slice(startIndex);
      if (mode === "tokens") {
        const roundTokens = roundMessages.reduce((t, m) => t + estimateMessageTokens(visibleMessageText(m)), state?.summaryTokens ?? 0);
        if (roundTokens >= budget) {
          setCompactionTipVisible(true);
          setCompactionRoundTokens(roundTokens);
        } else {
          setCompactionTipVisible(false);
          setCompactionRoundTokens(0);
        }
      } else {
        const roundCount = roundMessages.length + (state?.summaryMessages ?? 0);
        if (roundCount >= messageLimit) {
          setCompactionTipVisible(true);
          setCompactionRoundTokens(roundCount);
        } else {
          setCompactionTipVisible(false);
          setCompactionRoundTokens(0);
        }
      }
    }).catch(() => {
      // fallback: full count
      if (cancelled) return;
      if (mode === "tokens") {
        const total = dialogueMessages.reduce((t, m) => t + estimateMessageTokens(visibleMessageText(m)), 0);
        if (total >= budget) {
          setCompactionTipVisible(true);
          setCompactionRoundTokens(total);
        }
      } else {
        if (dialogueMessages.length >= messageLimit) {
          setCompactionTipVisible(true);
          setCompactionRoundTokens(dialogueMessages.length);
        }
      }
    });
    return () => { cancelled = true; };
  }, [messages, activeConversationId, chatConfig?.shortContextMode, chatConfig?.shortContextTokenBudget, chatConfig?.maxContextRounds]);
  const shortContextNotice = useMemo(() => {
    if (!compactionTipVisible) return null;
    const mode = chatConfig?.shortContextMode === "tokens" ? "tokens" : "messages";
    if (mode === "tokens") {
      return `本轮短时记忆已达到 ${formatTokenK(compactionRoundTokens)} token 预算，旧片段已压缩为短时摘要。发送新消息后将开始新一轮对话。`;
    }
    return `本轮短时记忆已达到 ${compactionRoundTokens} 条消息窗口，旧片段已压缩为短时摘要。发送新消息后将开始新一轮对话。`;
  }, [compactionTipVisible, compactionRoundTokens, chatConfig?.shortContextMode]);
  const shortMemoryStats = useMemo(() => {
    const stats = new Map<string, ShortMemoryMessageStat>();
    const mode = chatConfig?.shortContextMode === "tokens" ? "tokens" : "messages";
    const messageLimit = clampCount(chatConfig?.maxContextRounds, 10, 1, 500);
    let dialogueCount = 0;
    for (const message of messages) {
      if (message.role !== "user" && message.role !== "assistant") continue;
      dialogueCount += 1;
      if (message.role !== "assistant" || message.source === "desktop-stream") continue;
      if (mode === "tokens") {
        stats.set(message.id, {
          label: `本轮回复约 ${estimateMessageTokens(message.content).toLocaleString()} tokens`,
          tone: "tokens"
        });
      } else {
        const remaining = Math.max(0, messageLimit - dialogueCount);
        stats.set(message.id, {
          label: `短时记忆重置前剩余 ${remaining} 条消息`,
          tone: "messages"
        });
      }
    }
    return stats;
  }, [chatConfig?.maxContextRounds, chatConfig?.shortContextMode, messages]);
  const activeAgent = useMemo(() => {
    return resolvePersonaBoundAgent(toolbarPersona, agents, activeConversation?.agentId) ?? defaultAgent;
  }, [activeConversation?.agentId, agents, defaultAgent, toolbarPersona]);
  const activeToolIterationBudget = toolbarPersona?.toolPolicy?.maxIterations
    ?? selectedPersona?.toolPolicy?.maxIterations
    ?? activeAgent?.maxToolIterations
    ?? agentConfig?.maxToolIterations
    ?? "-";
  const activeRun = useMemo(
    () => Object.values(activeAgentRuns).find((run) => run.conversationId === activeConversationId && !run.parentRunId),
    [activeAgentRuns, activeConversationId]
  );
  const activeQueueItems = useMemo(() => agentQueue
    .filter((item) => item.conversationId === activeConversationId)
    .filter((item) => item.status !== "completed")
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt)), [activeConversationId, agentQueue]);
  const availableMcpServers = useMemo(
    () => mcpServers.filter((server) => server.enabled),
    [mcpServers]
  );
  const activeMcpServerIdSet = useMemo(() => {
    if (!activeAgent?.mcpEnabled) return new Set<string>();
    const configured = activeAgent.enabledMcpServers
      .map((serverId) => serverId.trim())
      .filter(Boolean);
    return new Set(configured.length > 0 ? configured : availableMcpServers.map((server) => server.id));
  }, [activeAgent?.enabledMcpServers, activeAgent?.mcpEnabled, availableMcpServers]);
  const activeSkills = useMemo(() => {
    if (!activeAgent?.skillsEnabled) return [];
    const enabledIds = new Set(
      activeAgent.enabledSkills
        .map((skillId) => skillId.trim())
        .filter(Boolean)
    );
    return skills.filter((skill) => enabledIds.has(skill.id));
  }, [activeAgent?.enabledSkills, activeAgent?.skillsEnabled, skills]);

  useEffect(() => {
    if (activeSection !== "chat" || !activeAgent?.id) return;
    setFocusedAgentId(activeAgent.id);
  }, [activeAgent?.id, activeSection, setFocusedAgentId]);

  const slashCommandQuery = useMemo(() => {
    const value = draft.trimStart();
    if (!value.startsWith("/") && !value.startsWith("／")) return null;
    const body = value.slice(1);
    if (/\s/.test(body)) return null;
    return body.toLowerCase();
  }, [draft]);
  const slashCommandSuggestions = useMemo(() => {
    if (slashCommandQuery === null) return [];
    return controlCommands
      .filter((command) => {
        if (!slashCommandQuery) return true;
        return command.name.toLowerCase().startsWith(slashCommandQuery)
          || command.aliases.some((alias) => alias.toLowerCase().startsWith(slashCommandQuery));
      })
      .slice(0, 8);
  }, [controlCommands, slashCommandQuery]);

  useEffect(() => {
    setSelectedSlashCommandIndex(0);
  }, [slashCommandQuery]);

  useEffect(() => {
    if (selectedSlashCommandIndex >= slashCommandSuggestions.length) {
      setSelectedSlashCommandIndex(Math.max(0, slashCommandSuggestions.length - 1));
    }
  }, [selectedSlashCommandIndex, slashCommandSuggestions.length]);
  const storedRun = useMemo(
    () => agentRuns.find((run) => run.conversationId === activeConversationId && !run.parentRunId),
    [activeConversationId, agentRuns]
  );
  const runStates = useMemo(() => {
    const states = new Map<string, string>();
    for (const run of agentRuns) states.set(run.runId, run.state);
    for (const run of Object.values(activeAgentRuns)) states.set(run.runId, run.state);
    return states;
  }, [activeAgentRuns, agentRuns]);
  const runByQueueItemId = useMemo(() => {
    const entries = new Map<string, { runId: string; state: string }>();
    for (const run of agentRuns) {
      if (run.queueItemId) entries.set(run.queueItemId, { runId: run.runId, state: run.state });
    }
    for (const run of Object.values(activeAgentRuns)) {
      if (run.queueItemId) entries.set(run.queueItemId, { runId: run.runId, state: run.state });
    }
    return entries;
  }, [activeAgentRuns, agentRuns]);
  const visibleParentRunId = activeRun?.runId ?? storedRun?.runId ?? null;
  const activeChildRuns = useMemo(
    () => agentRuns
      .filter((run) => run.parentRunId === visibleParentRunId)
      .sort((a, b) => {
        const stateRank = Number(isTerminalRunState(a.state)) - Number(isTerminalRunState(b.state));
        if (stateRank !== 0) return stateRank;
        const indexRank = (a.subagentIndex ?? 0) - (b.subagentIndex ?? 0);
        if (indexRank !== 0) return indexRank;
        return new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime();
      })
      .slice(0, 8),
    [agentRuns, visibleParentRunId]
  );
  const activeChildRunCount = activeChildRuns.length;
  const runningChildRunCount = activeChildRuns.filter((run) => !isTerminalRunState(run.state)).length;
  const activeRunActivityAt = activeRun?.lastActivityAt ?? storedRun?.lastActivityAt ?? activeRun?.updatedAt ?? storedRun?.updatedAt ?? null;
  const activeRunActivityDesc = activeRun?.lastActivityDesc ?? storedRun?.lastActivityDesc ?? null;
  const stoppableRun = activeRun ?? (storedRun && !["completed", "failed", "aborted"].includes(storedRun.state) ? storedRun : null);
  const activeRunPhases = activeRun?.accumulatedPhases
    ?? (activeRun?.phase ? [{ phase: activeRun.phase, detail: activeRun.detail, updatedAt: activeRun.updatedAt }] : storedRun?.phaseEvents ?? []);
  const activeToolStartTimes = useMemo(
    () => runningToolStartTimesFromPhases(activeRunPhases),
    [activeRunPhases]
  );
  const activeToolEvents: ToolEvent[] = (activeRun?.accumulatedToolEvents?.length
    ? activeRun.accumulatedToolEvents
    : activeRun?.toolEvent
      ? [activeRun.toolEvent]
      : []
  )
    .map((event) => withToolEventStartedAt(event, activeToolStartTimes.get(toolEventStartKey(event)) ?? activeRunActivityAt ?? activeRun?.updatedAt ?? null))
    .map((event) => materializeToolEvent(event, event.runId ? runStates.get(event.runId) : null));
  const activeProcessEvents = useMemo(
    () => managedProcessEvents
      .filter((event) => event.conversationId === activeConversationId || Boolean(activeRun?.runId && event.runId === activeRun.runId))
      .slice(0, 6),
    [activeConversationId, activeRun?.runId, managedProcessEvents]
  );
  const recentMessages = useMemo(() => messages.slice(-renderLimit), [messages, renderLimit]);
  const artifactMessages = useMemo(() => recentMessages.slice(-artifactScanLimit), [artifactScanLimit, recentMessages]);
  const messageToolEvents = useMemo(() => selectVisibleToolEvents(recentMessages
    .map((message) => {
      const event = message.role === "tool" ? parseToolEvent(message.content) : null;
      return event ? materializeToolEvent(withToolEventStartedAt(event, message.createdAt), event.runId ? runStates.get(event.runId) : null) : null;
    })
    .filter((event): event is ToolEvent => event !== null)), [recentMessages, runStates]);
  const graphEvents = activeToolEvents.length > 0 ? selectVisibleToolEvents(activeToolEvents) : messageToolEvents;
  const providerBinding = useMemo(
    () => resolvePersonaAgentBinding(toolbarPersona, agents, llmProviders, activeConversation?.agentId),
    [activeConversation?.agentId, agents, llmProviders, toolbarPersona]
  );
  const currentProvider = useMemo(() => {
    const providerId = providerBinding.providerId;
    return llmProviders.find((provider) => provider.id === providerId && provider.enabled) ?? null;
  }, [llmProviders, providerBinding.providerId]);
  const effectiveModelValue = providerBinding.model;
  useEffect(() => {
    if (!currentProvider) {
      setCatalogModels([]);
      return;
    }
    let cancelled = false;
    api.detectProviderModels(currentProvider).then((result) => {
      if (!cancelled) setCatalogModels(result.models ?? []);
    }).catch(() => {
      if (!cancelled) setCatalogModels([]);
    });
    return () => {
      cancelled = true;
    };
  }, [currentProvider]);
  const modelOptions = useMemo(() => {
    if (catalogModels.length > 0 && currentProvider) {
      const options = catalogModels.map((model) => ({
        key: `${currentProvider.id}::${model.id}`,
        providerId: currentProvider.id,
        model: model.id,
        label: model.id
      }));
      const currentModel = effectiveModelValue.trim();
      if (currentModel && !options.some((option) => option.model === currentModel)) {
        options.unshift({
          key: `${currentProvider.id}::${currentModel}`,
          providerId: currentProvider.id,
          model: currentModel,
          label: `${currentModel}（当前）`
        });
      }
      const defaultModel = currentProvider.model.trim();
      if (defaultModel && !options.some((option) => option.model === defaultModel)) {
        options.unshift({
          key: `${currentProvider.id}::${defaultModel}`,
          providerId: currentProvider.id,
          model: defaultModel,
          label: `${defaultModel}（默认）`
        });
      }
      return options;
    }
    if (!currentProvider) return [];
    const options = providerModelOptions([currentProvider]);
    const currentModel = effectiveModelValue.trim();
    if (currentModel && !options.some((option) => option.model === currentModel)) {
      options.unshift({
        key: `${currentProvider.id}::${currentModel}`,
        providerId: currentProvider.id,
        model: currentModel,
        label: `${currentModel}（当前）`
      });
    }
    return options;
  }, [catalogModels, currentProvider, effectiveModelValue]);
  const selectedModelKey = currentProvider && effectiveModelValue
    ? `${currentProvider.id}::${effectiveModelValue}`
    : "";
  const emojiPathIndexes = useMemo(() => buildEmojiPathIndexes(emojiGroups), [emojiGroups]);
  const artifacts = useMemo(() => {
    const results: ArtifactTarget[] = [];
    const seen = new Set<string>();
    const push = (target: ArtifactTarget) => {
      if (!target.path || seen.has(target.path)) return;
      seen.add(target.path);
      results.push(target);
    };
    for (const event of messageToolEvents) {
      if (event.path && event.exists) {
        push({
          path: event.path,
          title: event.title || fileNameFromPath(event.path),
          kind: artifactKind(event.path, event.mimeType),
          source: `${event.serverId}.${event.toolName}`
        });
      }
    }
    for (const message of artifactMessages) {
      for (const target of extractArtifactPaths(message.content)) push(target);
    }
    for (const attachment of attachments) {
      push({
        path: attachment.path,
        title: attachment.fileName,
        kind: artifactKind(attachment.path, attachment.mimeType),
        source: "attachment"
      });
    }
    return results;
  }, [artifactMessages, attachments, messageToolEvents]);
  const canStopRun = Boolean(stoppableRun);
  const isProcessing = canStopRun;
  const [showThinking, setShowThinking] = useState(false);
  // Keep the thinking row mounted through its exit animation so the
  // transition can play instead of the node being removed instantly.
  const [thinkingMounted, setThinkingMounted] = useState(false);
  const thinkingLeaveTimerRef = useRef<number | null>(null);
  const hasStreamingContent = useMemo(
    () => messages.some((m) => m.source === "desktop-stream" && m.content.length > 0),
    [messages]
  );
  const [firstCharShown, setFirstCharShown] = useState(false);
  // Reset when streaming message disappears (new turn)
  useEffect(() => {
    if (!hasStreamingContent) setFirstCharShown(false);
  }, [hasStreamingContent]);
  const handleFirstStreamChar = useCallback(() => { setFirstCharShown(true); }, []);
  const processingEndedAtRef = useRef<number | null>(null);
  const wasHiddenRef = useRef(false);

  // Manage thinking animation visibility
  useEffect(() => {
    // While processing or streaming, keep thinking visible
    if (isProcessing || hasStreamingContent) {
      if (isProcessing) processingEndedAtRef.current = null;
      setShowThinking(true);
      return;
    }
    // Both ended — start hide timer respecting minimum visible time
    if (processingEndedAtRef.current === null) processingEndedAtRef.current = Date.now();
    const elapsed = Date.now() - processingEndedAtRef.current;
    const delay = Math.max(0, thinkingMinVisibleMs - elapsed);
    const timer = window.setTimeout(() => {
      processingEndedAtRef.current = null;
      setShowThinking(false);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [isProcessing, hasStreamingContent, thinkingMinVisibleMs, firstCharShown]);

  useEffect(() => {
    if (!activeConversationId) return;
    const state: PetThinkingState = {
      conversationId: activeConversationId,
      personaId: activeConversation?.personaId ?? selectedPersona?.id ?? null,
      source: "desktop-ui",
      thinking: showThinking,
      updatedAt: new Date().toISOString()
    };
    publishPetThinkingState(state);
    void emit(PET_THINKING_STATE_EVENT, state).catch(() => undefined);
    void emitTo("pet", PET_THINKING_STATE_EVENT, state).catch(() => undefined);
    void emitTo("pet", "synthchat-pet-event", {
      type: showThinking ? "thinking_started" : "thinking_finished",
      source: state.source,
      personaId: state.personaId,
      conversationId: activeConversationId,
      ok: !showThinking
    }).catch(() => undefined);
  }, [activeConversation?.personaId, activeConversationId, selectedPersona?.id, showThinking]);

  // Drive mount/unmount with an exit animation: mount immediately when
  // showThinking turns on; when it turns off keep the node mounted with the
  // leaving class long enough for the exit transition to finish, then unmount.
  const THINKING_LEAVE_MS = 200;
  useEffect(() => {
    if (showThinking) {
      if (thinkingLeaveTimerRef.current !== null) {
        window.clearTimeout(thinkingLeaveTimerRef.current);
        thinkingLeaveTimerRef.current = null;
      }
      setThinkingMounted(true);
      return;
    }
    if (!thinkingMounted) return;
    thinkingLeaveTimerRef.current = window.setTimeout(() => {
      setThinkingMounted(false);
      thinkingLeaveTimerRef.current = null;
    }, THINKING_LEAVE_MS);
    return () => {
      if (thinkingLeaveTimerRef.current !== null) {
        window.clearTimeout(thinkingLeaveTimerRef.current);
        thinkingLeaveTimerRef.current = null;
      }
    };
  }, [showThinking, thinkingMounted]);

  useEffect(() => {
    const isHidden = activeSection !== "chat";
    const previous = seenMessageContentRef.current;
    const next = new Map<string, string>();
    const changedAssistantIds: string[] = [];
    for (const message of messages) {
      const visibleContent = visibleMessageText(message);
      next.set(message.id, visibleContent);
      if (message.role !== "assistant" || !visibleContent.trim()) continue;
      if (previous.size > 0 && previous.get(message.id) !== visibleContent) {
        changedAssistantIds.push(message.id);
      }
    }
    if (isHidden) {
      seenMessageContentRef.current = next;
      wasHiddenRef.current = true;
      return;
    }
    if (wasHiddenRef.current) {
      wasHiddenRef.current = false;
      seenMessageContentRef.current = next;
      return;
    }
    seenMessageContentRef.current = next;
    if (changedAssistantIds.length === 0) return;
    setAnimatedMessageIds((current) => {
      const updated = new Set(current);
      for (const id of changedAssistantIds) updated.add(id);
      return updated;
    });
  }, [activeSection, messages]);

  useEffect(() => {
    if (activeSection === "chat") return;
    activeVoiceReplyRequestRef.current = null;
    for (const message of messages) {
      if (message.role === "assistant") {
        notifiedAssistantMessageIdsRef.current.add(message.id);
        spokenAssistantMessageIdsRef.current.add(message.id);
      }
    }
  }, [activeSection, messages]);

  const handleMessageAnimationDone = useCallback((messageId: string) => {
    setAnimatedMessageIds((current) => {
      if (!current.has(messageId)) return current;
      const updated = new Set(current);
      updated.delete(messageId);
      return updated;
    });
  }, []);

  const filteredConversations = useMemo(() => {
    const needle = deferredQuery.toLowerCase();
    return conversations.filter((item) =>
      `${item.title} ${item.lastMessage}`.toLowerCase().includes(needle)
    );
  }, [conversations, deferredQuery]);
  const enabledMcpCount = useMemo(
    () => availableMcpServers.filter((server) => activeMcpServerIdSet.has(server.id)).length,
    [activeMcpServerIdSet, availableMcpServers]
  );
  const enabledSkillCount = useMemo(() => activeSkills.length, [activeSkills]);
  const agentReady = Boolean(activeAgent?.enabled && (activeAgent.allowShell || activeAgent.mcpEnabled || activeAgent.skillsEnabled));

  const scrollToBottom = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const target = el.scrollHeight;
    if (target <= 0) return;
    el.scrollTop = target;
    // Double-RAF: wait for React commit + browser layout to settle
    window.requestAnimationFrame(() => {
      const el2 = scrollRef.current;
      if (!el2) return;
      const h = el2.scrollHeight;
      if (h > 0) el2.scrollTop = h;
    });
  }, []);

  // Ref-based scroll tracking (synchronous, not affected by React batching)
  const nearBottomRef = useRef(true);

  // Track the currently rendered conversation tail.
  const lastMessage = messages.length > 0 ? messages[messages.length - 1] : null;
  const latestMessageKey = messages.length > 0
    ? `${messages[messages.length - 1].id}:${messages[messages.length - 1].content.length}`
    : "";
  const prevConversationIdRef = useRef<string | null>(activeConversationId);
  const prevActiveSectionRef = useRef(activeSection);
  const scrollOnNextMessagesRef = useRef<"bottom" | "restore" | null>(null);
  const scrollRestoreTargetRef = useRef<{ conversationId: string; memory: ConversationScrollMemory } | null>(null);
  const conversationActivatedAtRef = useRef<number>(Date.now());
  const notifiedAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const loadingHistoryRef = useRef(false);
  const preserveTopOnHistoryLoadRef = useRef<{ scrollHeight: number; scrollTop: number } | null>(null);

  const stopVoicePlayback = useCallback(() => {
    if (isTauri()) {
      void api.stopChatAudio?.().catch((error: unknown) => {
        console.warn("chat native voice stop failed:", error);
      });
    }
    const audio = voiceAudioRef.current;
    if (audio) {
      audio.pause();
      audio.src = "";
      voiceAudioRef.current = null;
    }
  }, []);

  useEffect(() => () => stopVoicePlayback(), [stopVoicePlayback]);

  useEffect(() => {
    if (activeSection === "chat") return;
    activeVoiceReplyRequestRef.current = null;
    stopVoicePlayback();
  }, [activeSection, stopVoicePlayback]);

  const loadMoreHistory = useCallback(async () => {
    const element = scrollRef.current;
    if (!element || !activeConversationId || loadingHistoryRef.current || historyExhausted) return;
    loadingHistoryRef.current = true;
    setHistoryLoading(true);
    preserveTopOnHistoryLoadRef.current = {
      scrollHeight: element.scrollHeight,
      scrollTop: element.scrollTop
    };
    try {
      const beforeCount = messages.length;
      const result = await loadOlderMessages(activeConversationId, renderLimit);
      if (result.loadedCount <= beforeCount || !result.hasMore) {
        setHistoryExhausted(true);
      }
    } catch (error) {
      console.warn("load older messages failed", error);
      preserveTopOnHistoryLoadRef.current = null;
    } finally {
      loadingHistoryRef.current = false;
      setHistoryLoading(false);
    }
  }, [activeConversationId, historyExhausted, loadOlderMessages, messages.length, renderLimit]);

  useEffect(() => {
    if (activeConversationPersona?.voiceReply?.enabled) return;
    activeVoiceReplyRequestRef.current = null;
    stopVoicePlayback();
  }, [activeConversationPersona?.voiceReply?.enabled, stopVoicePlayback]);

  const getScrollAnchor = useCallback((element: HTMLDivElement): ConversationScrollMemory => {
    const base: ConversationScrollMemory = { top: element.scrollTop };
    const nodes = element.querySelectorAll<HTMLElement>("[data-message-id]");
    const containerTop = element.getBoundingClientRect().top;
    const containerBottom = containerTop + element.clientHeight;
    for (const node of Array.from(nodes)) {
      const messageId = node.dataset.messageId?.trim();
      if (!messageId) continue;
      const rect = node.getBoundingClientRect();
      if (rect.bottom <= containerTop) continue;
      if (rect.top >= containerBottom) break;
      return {
        top: element.scrollTop,
        anchorMessageId: messageId,
        anchorOffset: rect.top - containerTop
      };
    }
    return base;
  }, []);

  const canPersistScrollPosition = useCallback((element: HTMLDivElement | null) => {
    if (!element) return false;
    if (activeSection !== "chat") return false;
    return element.clientHeight > 0 && element.scrollHeight > 0;
  }, [activeSection]);

  const applyScrollMemory = useCallback((element: HTMLDivElement, memory: ConversationScrollMemory) => {
    let targetTop = memory.top;
    if (memory.anchorMessageId) {
      const anchor = Array.from(element.querySelectorAll<HTMLElement>("[data-message-id]"))
        .find((node) => node.dataset.messageId === memory.anchorMessageId);
      if (anchor) {
        targetTop = anchor.offsetTop - (memory.anchorOffset ?? 0);
      }
    }
    const maxTop = Math.max(0, element.scrollHeight - element.clientHeight);
    const nextTop = Math.min(Math.max(0, targetTop), maxTop);
    element.scrollTop = nextTop;
    return nextTop;
  }, []);

  const saveCurrentScrollPosition = useCallback((conversationId: string | null) => {
    const element = scrollRef.current;
    if (!element || !conversationId || !canPersistScrollPosition(element)) return;
    conversationScrollPositionCache.set(conversationId, getScrollAnchor(element));
  }, [canPersistScrollPosition, getScrollAnchor]);

  const restoreSavedScrollPosition = useCallback((conversationId: string | null) => {
    if (!conversationId) return () => {};
    const saved = conversationScrollPositionCache.get(conversationId);
    let cancelled = false;
    let attempts = 0;
    const apply = () => {
      if (cancelled) return;
      const element = scrollRef.current;
      if (!element) {
        if (attempts < 8) {
          attempts += 1;
          window.requestAnimationFrame(apply);
        }
        return;
      }
      if (saved === undefined) {
        nearBottomRef.current = true;
        setIsNearBottom(true);
        setUnreadCount(0);
        markConversationRead(conversationId);
        scrollToBottom();
        return;
      }
      const nextTop = applyScrollMemory(element, saved);
      if (canPersistScrollPosition(element)) {
        conversationScrollPositionCache.set(conversationId, getScrollAnchor(element));
      }
      const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
      const near = distanceFromBottom <= bottomFollowThresholdPx;
      nearBottomRef.current = near;
      setIsNearBottom(near);
      const unread = conversationUnreadCounts[conversationId] ?? 0;
      setUnreadCount(near ? 0 : unread);
      if (near) {
        markConversationRead(conversationId);
      }
      if (attempts < 6 && Math.abs(element.scrollTop - nextTop) > 2) {
        attempts += 1;
        window.requestAnimationFrame(apply);
      }
    };
    window.requestAnimationFrame(apply);
    return () => {
      cancelled = true;
    };
  }, [applyScrollMemory, bottomFollowThresholdPx, conversationUnreadCounts, getScrollAnchor, markConversationRead, scrollToBottom]);

  const selectConversationWithScrollMemory = useCallback((conversationId: string) => {
    saveCurrentScrollPosition(activeConversationId);
    void selectConversation(conversationId);
  }, [activeConversationId, saveCurrentScrollPosition, selectConversation]);

  const deleteConversationWithMemorySettling = useCallback(async (conversationId: string) => {
    if (settlingConversationId) return;
    setSettlingConversationId(conversationId);
    try {
      const result = await deleteConversation(conversationId);
      if (result.status === "failed") {
        console.warn("Conversation deleted, but memory settling failed:", result.reason);
      } else if (result.status === "scheduled") {
        window.setTimeout(() => void refreshMemories(), 1500);
      } else {
        void refreshMemories();
      }
    } finally {
      setSettlingConversationId((current) => current === conversationId ? null : current);
    }
  }, [deleteConversation, refreshMemories, settlingConversationId]);

  // Mark conversation switch for instant scroll
  useEffect(() => {
    if (activeConversationId !== prevConversationIdRef.current) {
      prevConversationIdRef.current = activeConversationId;
      conversationActivatedAtRef.current = Date.now();
      activeVoiceReplyRequestRef.current = null;
      stopVoicePlayback();
      setUnreadCount(0);
      setIsNearBottom(true);
      nearBottomRef.current = true;
      // Check if we have a saved position for this conversation
      const savedPosition = activeConversationId ? conversationScrollPositionCache.get(activeConversationId) : undefined;
      scrollOnNextMessagesRef.current = savedPosition ? "restore" : "bottom";
      scrollRestoreTargetRef.current = activeConversationId && savedPosition
        ? { conversationId: activeConversationId, memory: savedPosition }
        : null;
    }
  }, [activeConversationId, stopVoicePlayback]);

  useEffect(() => {
    const previousSection = prevActiveSectionRef.current;
    prevActiveSectionRef.current = activeSection;
    if (previousSection === "chat" && activeSection !== "chat") {
      saveCurrentScrollPosition(activeConversationId);
      return;
    }
    if (previousSection !== "chat" && activeSection === "chat") {
      return restoreSavedScrollPosition(activeConversationId);
    }
  }, [activeConversationId, activeSection, restoreSavedScrollPosition, saveCurrentScrollPosition]);

  // Instant scroll when messages load after conversation switch
  useEffect(() => {
    if (!scrollOnNextMessagesRef.current || messages.length === 0) return;
    const mode = scrollOnNextMessagesRef.current;
    const convId = activeConversationId;
    let cancelled = false;
    let attempts = 0;
    const attemptScroll = () => {
      if (cancelled) return true;
      const el = scrollRef.current;
      if (!el || el.scrollHeight <= 0) return false;
      if (mode === "restore" && convId) {
        const target = scrollRestoreTargetRef.current?.conversationId === convId
          ? scrollRestoreTargetRef.current.memory
          : conversationScrollPositionCache.get(convId);
        if (target) {
          const appliedTop = applyScrollMemory(el, target);
          const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
          nearBottomRef.current = dist <= bottomFollowThresholdPx;
          setIsNearBottom(nearBottomRef.current);
          attempts += 1;
          if (attempts >= 6 || Math.abs(el.scrollTop - appliedTop) <= 2) {
            scrollOnNextMessagesRef.current = null;
            scrollRestoreTargetRef.current = null;
            return true;
          }
          return false;
        }
      }
      el.scrollTop = el.scrollHeight;
      nearBottomRef.current = true;
      setIsNearBottom(true);
      scrollOnNextMessagesRef.current = null;
      scrollRestoreTargetRef.current = null;
      return true;
    };
    const retry = () => {
      if (!attemptScroll()) window.requestAnimationFrame(retry);
    };
    retry();
    return () => {
      cancelled = true;
    };
  }, [activeConversationId, applyScrollMemory, bottomFollowThresholdPx, messages]);

  useLayoutEffect(() => {
    const snapshot = preserveTopOnHistoryLoadRef.current;
    if (!snapshot) return;
    const element = scrollRef.current;
    preserveTopOnHistoryLoadRef.current = null;
    if (!element) return;
    const delta = element.scrollHeight - snapshot.scrollHeight;
    element.scrollTop = snapshot.scrollTop + Math.max(0, delta);
    if (activeConversationId && canPersistScrollPosition(element)) {
      conversationScrollPositionCache.set(activeConversationId, getScrollAnchor(element));
    }
  }, [activeConversationId, getScrollAnchor, messages]);

  const handleScroll = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
    const near = distanceFromBottom <= bottomFollowThresholdPx;
    nearBottomRef.current = near;
    setIsNearBottom(near);
    if (scrollOnNextMessagesRef.current) return;
    if (element.scrollTop <= 48 && messages.length >= renderLimit && !historyLoading && !historyExhausted) {
      void loadMoreHistory();
    }
    // Save scroll position for current conversation
    saveCurrentScrollPosition(activeConversationId);
    if (near) {
      setUnreadCount(0);
      markConversationRead(activeConversationId ?? "");
    }
  }, [activeConversationId, bottomFollowThresholdPx, historyExhausted, historyLoading, loadMoreHistory, markConversationRead, messages.length, renderLimit, saveCurrentScrollPosition]);

  const handleScrollToBottom = useCallback(() => {
    setUnreadCount(0);
    setIsNearBottom(true);
    nearBottomRef.current = true;
    markConversationRead(activeConversationId ?? "");
    scrollToBottom();
  }, [activeConversationId, markConversationRead, scrollToBottom]);

  useEffect(() => {
    if (!activeConversationId || !lastMessage) return;
    if (activeSection !== "chat") return;
    if (scrollOnNextMessagesRef.current) return;
    if (lastMessage.role !== "assistant") return;
    if (notifiedAssistantMessageIdsRef.current.has(lastMessage.id)) return;
    const createdAt = new Date(lastMessage.createdAt).getTime();
    if (!Number.isFinite(createdAt) || createdAt < conversationActivatedAtRef.current) return;
    notifiedAssistantMessageIdsRef.current.add(lastMessage.id);
    if (nearBottomRef.current) {
      if (scrollRef.current) {
        const el = scrollRef.current;
        const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
        if (dist <= bottomFollowThresholdPx) {
          markConversationRead(activeConversationId);
          scrollToBottom();
          return;
        }
      }
      incrementConversationUnread(activeConversationId);
      setUnreadCount((c) => c + 1);
    } else {
      incrementConversationUnread(activeConversationId);
      setUnreadCount((c) => c + 1);
    }
  }, [activeConversationId, activeSection, bottomFollowThresholdPx, incrementConversationUnread, lastMessage, markConversationRead, scrollToBottom]);

  useEffect(() => () => {
    saveCurrentScrollPosition(activeConversationId);
  }, [activeConversationId, saveCurrentScrollPosition]);

  useEffect(() => {
    if (activeSection !== "chat") return;
    if (!latestMessageKey) return;
    const element = scrollRef.current;
    if (!element) return;
    const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
    if (nearBottomRef.current || distanceFromBottom <= bottomFollowThresholdPx) {
      scrollToBottom();
    }
  }, [activeSection, bottomFollowThresholdPx, latestMessageKey, scrollToBottom]);

  useEffect(() => {
    if (activeSection !== "chat") return;
    const interval = isProcessing ? activePollIntervalMs : idlePollIntervalMs;
    const timer = window.setInterval(() => {
      void Promise.all([
        refreshChatData(activeConversationId, selectedPersona?.id),
        refreshAgentRuns(),
        activeConversationId
          ? api.listAgentRuntimeEvents({ conversationId: activeConversationId, since: runtimeCursor, limit: 80 })
              .then((stream) => {
                setRuntimeCursor(stream.cursor);
                if (stream.events.length > 0) {
                  setRuntimeEvents((current) => [...current, ...stream.events].slice(-80));
                }
              })
          : Promise.resolve()
      ]);
    }, interval);
    return () => window.clearInterval(timer);
  }, [activeConversationId, activePollIntervalMs, activeSection, idlePollIntervalMs, isProcessing, refreshAgentRuns, refreshChatData, runtimeCursor, selectedPersona?.id]);

  const stageFiles = useCallback(async (files: FileList | File[]) => {
    const list = Array.from(files);
    if (list.length === 0) return;
    for (const file of list) {
      const temporaryId = crypto.randomUUID();
      const preview = file.type.startsWith("image/") ? URL.createObjectURL(file) : null;
      setAttachments((current) => [...current, {
        id: temporaryId,
        fileName: file.name,
        mimeType: file.type || "application/octet-stream",
        fileSize: file.size,
        path: "",
        preview,
        status: "staging"
      }]);
      try {
        const buffer = await file.arrayBuffer();
        const saved = await api.uploadChatAttachment(file.name, file.type || "application/octet-stream", Array.from(new Uint8Array(buffer)));
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...saved, preview, status: "ready" } : item));
      } catch (error) {
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...item, status: "error", error: String(error) } : item));
      }
    }
  }, []);

  const stageFilePaths = useCallback(async (paths: string[]) => {
    const list = paths.map((path) => path.trim()).filter(Boolean);
    if (list.length === 0) return;
    for (const path of list) {
      const temporaryId = crypto.randomUUID();
      setAttachments((current) => [...current, {
        id: temporaryId,
        fileName: fileNameFromLocalPath(path),
        mimeType: "application/octet-stream",
        fileSize: 0,
        path,
        preview: null,
        status: "staging"
      }]);
      try {
        const saved = await api.uploadChatAttachmentFromPath(path);
        const preview = saved.mimeType.startsWith("image/") ? api.convertFileSrc(saved.path) : null;
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...saved, preview, status: "ready" } : item));
      } catch (error) {
        setAttachments((current) => current.map((item) => item.id === temporaryId ? { ...item, status: "error", error: String(error) } : item));
      }
    }
  }, []);

  const handleFileDragEnter = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
    setDragActive(true);
  }, []);

  const handleFileDragOver = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
    setDragActive(true);
  }, []);

  const handleFileDragLeave = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.stopPropagation();
    const nextTarget = event.relatedTarget as Node | null;
    if (!nextTarget || !event.currentTarget.contains(nextTarget)) {
      setDragActive(false);
    }
  }, []);

  const handleFileDrop = useCallback((event: ReactDragEvent<HTMLElement>) => {
    event.preventDefault();
    event.stopPropagation();
    setDragActive(false);
    if (event.dataTransfer.files.length > 0) {
      void stageFiles(event.dataTransfer.files);
    }
  }, [stageFiles]);

  const isPointInsideChatDropTarget = useCallback((x: number, y: number) => {
    return [chatShellRef.current, composerRef.current, chatMainRef.current].some((element) => {
      if (!element) return false;
      const rect = element.getBoundingClientRect();
      return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
    });
  }, []);

  const isNativeDropInsideChatTarget = useCallback((position: NativeFileDropPayload["position"]) => {
    if (!position) return true;
    const pixelRatio = window.devicePixelRatio || 1;
    return isPointInsideChatDropTarget(position.x / pixelRatio, position.y / pixelRatio);
  }, [isPointInsideChatDropTarget]);

  const rememberFileDropSignature = useCallback((signature: string, windowMs = 1000) => {
    const now = Date.now();
    const previous = lastNativeDropRef.current;
    if (previous?.signature === signature && now - previous.at < windowMs) return false;
    lastNativeDropRef.current = { signature, at: now };
    return true;
  }, []);

  const rememberPathDrop = useCallback((paths: string[]) => {
    return rememberFileDropSignature(`paths:${paths.slice().sort().join("\n")}`);
  }, [rememberFileDropSignature]);

  const rememberDomDrop = useCallback((files: FileList) => {
    const signature = Array.from(files)
      .map((file) => `${file.name}:${file.size}:${file.lastModified}`)
      .sort()
      .join("\n");
    return rememberFileDropSignature(`files:${signature}`, 500);
  }, [rememberFileDropSignature]);

  const handleNativeFileDrop = useCallback((payload: NativeFileDropPayload) => {
    if (activeSection !== "chat") {
      setDragActive(false);
      return;
    }
    if (payload.type === "leave") {
      setDragActive(false);
      return;
    }
    if (!isNativeDropInsideChatTarget(payload.position)) {
      setDragActive(false);
      return;
    }
    if (payload.type === "enter" || payload.type === "over") {
      setDragActive(true);
      return;
    }
    setDragActive(false);
    const paths = (payload.paths ?? []).map((path) => path.trim()).filter(Boolean);
    if (paths.length === 0) return;
    if (!rememberPathDrop(paths)) return;
    void stageFilePaths(paths);
  }, [activeSection, isNativeDropInsideChatTarget, rememberPathDrop, stageFilePaths]);

  useEffect(() => {
    if (activeSection !== "chat") {
      setDragActive(false);
      return;
    }
    const handleDrag = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
      const inside = isPointInsideChatDropTarget(event.clientX, event.clientY);
      if (!inside) {
        setDragActive(false);
        return;
      }
      setDragActive(true);
    };
    const handleDragLeave = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      const nextTarget = event.relatedTarget as Node | null;
      if (nextTarget && (chatMainRef.current?.contains(nextTarget) || composerRef.current?.contains(nextTarget))) return;
      setDragActive(false);
    };
    const handleDrop = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      event.preventDefault();
      event.stopPropagation();
      const inside = isPointInsideChatDropTarget(event.clientX, event.clientY);
      if (!inside) {
        setDragActive(false);
        return;
      }
      setDragActive(false);
      if (event.dataTransfer && event.dataTransfer.files.length > 0 && rememberDomDrop(event.dataTransfer.files)) {
        void stageFiles(event.dataTransfer.files);
      }
    };
    window.addEventListener("dragenter", handleDrag, true);
    window.addEventListener("dragover", handleDrag, true);
    window.addEventListener("dragleave", handleDragLeave, true);
    window.addEventListener("drop", handleDrop, true);
    return () => {
      window.removeEventListener("dragenter", handleDrag, true);
      window.removeEventListener("dragover", handleDrag, true);
      window.removeEventListener("dragleave", handleDragLeave, true);
      window.removeEventListener("drop", handleDrop, true);
    };
  }, [activeSection, isPointInsideChatDropTarget, rememberDomDrop, stageFiles]);

  useEffect(() => {
    if (!isTauri() || activeSection !== "chat") {
      setDragActive(false);
      return;
    }
    const unlisteners: Array<() => void> = [];
    let cancelled = false;
    const attach = (source: string, registration: Promise<() => void>) => {
      void registration.then((handler) => {
        if (cancelled) {
          handler();
        } else {
          unlisteners.push(handler);
        }
      }).catch((error) => {
        console.warn(`${source} file drop listener unavailable:`, error);
      });
    };
    attach("webview native", getCurrentWebview().onDragDropEvent((event) => handleNativeFileDrop(event.payload as NativeFileDropPayload)));
    attach("window native", getCurrentWindow().onDragDropEvent((event) => handleNativeFileDrop(event.payload as NativeFileDropPayload)));
    attach("window forwarded", listen<NativeFileDropPayload>("synthchat-file-drop-event", (event) => {
      if (event.payload.windowLabel && event.payload.windowLabel !== "main") return;
      handleNativeFileDrop(event.payload);
    }));
    return () => {
      cancelled = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [activeSection, handleNativeFileDrop]);

  const removeAttachment = (id: string) => {
    setAttachments((current) => current.filter((item) => item.id !== id));
  };

  const appendVoiceTranscript = useCallback((text: string) => {
    const transcript = text.trim();
    if (!transcript) return;
    setDraft((current) => {
      const prefix = current.trimEnd();
      return prefix ? `${prefix}\n${transcript}` : transcript;
    });
    setComposerError(null);
  }, []);

  const blobToDataUrl = useCallback((blob: Blob) => new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      if (typeof reader.result === "string") {
        resolve(reader.result);
      } else {
        reject(new Error("语音数据读取失败"));
      }
    };
    reader.onerror = () => reject(reader.error ?? new Error("语音数据读取失败"));
    reader.readAsDataURL(blob);
  }), []);

  const transcribeRecordedVoice = useCallback(async (blob: Blob) => {
    if (blob.size === 0) {
      setComposerError("没有录到语音内容。");
      return;
    }
    setVoiceInputState("transcribing");
    try {
      const dataUrl = await blobToDataUrl(blob);
      const result = await api.transcribeChatAudio(dataUrl, blob.type || "audio/webm");
      const transcript = String(result?.transcript ?? "").trim();
      if (transcript) {
        appendVoiceTranscript(transcript);
      } else {
        setComposerError("没有识别到语音内容。");
      }
    } catch (error) {
      setComposerError(composerErrorText(error));
    } finally {
      setVoiceInputState("idle");
    }
  }, [appendVoiceTranscript, blobToDataUrl]);

  const stopVoiceInput = useCallback(() => {
    const recognition = speechRecognitionRef.current;
    if (recognition) {
      recognition.stop();
      return;
    }
    const recorder = mediaRecorderRef.current;
    if (recorder && recorder.state !== "inactive") {
      recorder.stop();
    }
  }, []);

  const startRecordedVoiceInput = useCallback(async () => {
    if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
      setVoiceSupported(false);
      setComposerError("当前 WebView 不支持语音输入。");
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const preferredMimeType = [
        "audio/webm;codecs=opus",
        "audio/webm",
        "audio/ogg;codecs=opus"
      ].find((mimeType) => MediaRecorder.isTypeSupported?.(mimeType));
      const recorder = preferredMimeType
        ? new MediaRecorder(stream, { mimeType: preferredMimeType })
        : new MediaRecorder(stream);
      voiceChunksRef.current = [];
      mediaRecorderRef.current = recorder;
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) voiceChunksRef.current.push(event.data);
      };
      recorder.onerror = () => {
        stream.getTracks().forEach((track) => track.stop());
        mediaRecorderRef.current = null;
        setVoiceInputState("idle");
        setComposerError("语音录制失败。");
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((track) => track.stop());
        mediaRecorderRef.current = null;
        const blob = new Blob(voiceChunksRef.current, { type: recorder.mimeType || "audio/webm" });
        voiceChunksRef.current = [];
        void transcribeRecordedVoice(blob);
      };
      recorder.start();
      setVoiceInputState("recording");
      setComposerError(null);
    } catch (error) {
      setVoiceInputState("idle");
      setComposerError(composerErrorText(error));
    }
  }, [transcribeRecordedVoice]);

  const toggleVoiceInput = useCallback(() => {
    if (voiceInputState !== "idle") {
      stopVoiceInput();
      return;
    }
    const SpeechRecognitionCtor = (
      (window as unknown as { SpeechRecognition?: SpeechRecognitionConstructor }).SpeechRecognition
      ?? (window as unknown as { webkitSpeechRecognition?: SpeechRecognitionConstructor }).webkitSpeechRecognition
    );
    if (SpeechRecognitionCtor) {
      try {
        const recognition = new SpeechRecognitionCtor();
        speechRecognitionRef.current = recognition;
        recognition.lang = "zh-CN";
        recognition.continuous = false;
        recognition.interimResults = false;
        recognition.onresult = (event: unknown) => {
          const results = (event as { results?: ArrayLike<ArrayLike<{ transcript?: string }>> }).results;
          const transcript = results
            ? Array.from(results)
                .map((result) => result[0]?.transcript ?? "")
                .join("")
            : "";
          appendVoiceTranscript(transcript);
        };
        recognition.onerror = () => {
          speechRecognitionRef.current = null;
          setVoiceInputState("idle");
          void startRecordedVoiceInput();
        };
        recognition.onend = () => {
          speechRecognitionRef.current = null;
          setVoiceInputState((current) => current === "listening" ? "idle" : current);
        };
        recognition.start();
        setVoiceInputState("listening");
        setComposerError(null);
        setVoiceSupported(true);
        return;
      } catch {
        speechRecognitionRef.current = null;
      }
    }
    void startRecordedVoiceInput();
  }, [appendVoiceTranscript, startRecordedVoiceInput, stopVoiceInput, voiceInputState]);

  const switchAgentModel = async (key: string) => {
    if (!key) return;
    const option = modelOptions.find((item) => item.key === key);
    if (!option || !toolbarPersona || !currentProvider) return;
    const fixedProviderId = toolbarPersona.llmProvider.trim();
    if (!fixedProviderId || option.providerId !== fixedProviderId || currentProvider.id !== fixedProviderId) return;
    if (toolbarPersona.llmModel === option.model) return;
    const savedPersona = await savePersona({
      ...toolbarPersona,
      agentId: activeAgent?.id ?? toolbarPersona.agentId,
      llmProvider: fixedProviderId,
      llmModel: option.model
    });
    await refreshChatData(activeConversationId, savedPersona.id);
  };

  const switchConversationAgent = async (agentId: string) => {
    if (!agentId || activeAgent?.id === agentId) return;
    setFocusedAgentId(agentId);
    if (toolbarPersona) {
      const savedPersona = await savePersona({ ...toolbarPersona, agentId });
      await refreshChatData(activeConversationId, savedPersona.id);
      return;
    }
    if (!activeConversationId) return;
    const conversation = await api.setConversationAgent(activeConversationId, agentId);
    await refreshChatData((conversation as any)?.id || activeConversationId, (conversation as any)?.personaId ?? activeConversation?.personaId);
  };

  const submit = async () => {
    const content = draft.trim();
    const readyAttachments = attachments.filter((item) => item.status === "ready");
    if ((!content && readyAttachments.length === 0) || sendingRef.current) return;
    const submittedAttachments = attachments;
    sendingRef.current = true;
    setDraft("");
    setComposerError(null);
    setCompactionTipVisible(false);
    try {
      if (activeConversationPersona && activeAgent && activeConversationPersona.agentId !== activeAgent.id) {
        await savePersona({ ...activeConversationPersona, agentId: activeAgent.id });
      }
      const outboundPersonaId = activeConversation?.personaId ?? selectedPersona?.id;
      const attachmentContext = readyAttachments
        .map((file) => JSON.stringify({
          type: "attachment",
          id: file.id,
          fileName: file.fileName,
          mimeType: file.mimeType || "application/octet-stream",
          fileSize: file.fileSize,
          path: file.path,
          recommendedTool: file.mimeType?.startsWith("image/") ? "vision_analyze" : undefined
        }))
        .join("\n");
      const attachmentMarkers = readyAttachments
        .map((file) => `[media attached: "${file.path}" (${file.mimeType || "application/octet-stream"})] ${file.fileName}`)
        .join("\n");
      const outbound = [content, attachmentMarkers, attachmentContext].filter(Boolean).join("\n\n");
      setAttachments([]);
      await sendMessage(outbound, outboundPersonaId, activeAgent?.id);
      window.setTimeout(() => void refreshChatData(activeConversationId, outboundPersonaId), 500);
    } catch (error) {
      console.error("submit message failed", error);
      setDraft((current) => current.trim() ? current : content);
      setAttachments((current) => current.length > 0 ? current : submittedAttachments);
      setComposerError(composerErrorText(error));
    } finally {
      sendingRef.current = false;
      // Delay scroll to let React commit the new message to DOM first
      window.setTimeout(() => scrollToBottom(), 50);
    }
  };

  const stopActiveRun = async () => {
    if (!stoppableRun) return;
    await api.abortAgentRun(stoppableRun.runId, "Agent run stopped by user from chat.");
    setConversationProcessing(stoppableRun.conversationId, false);
    await Promise.all([
      refreshAgentRuns(),
      refreshAgentQueue(),
      refreshChatData(activeConversationId, selectedPersona?.id)
    ]);
  };

  const cancelQueuedItem = async (id: string) => {
    await api.cancelAgentQueueItem(id);
    await Promise.all([
      refreshAgentQueue(),
      refreshAgentRuns(),
      refreshChatData(activeConversationId, selectedPersona?.id)
    ]);
  };

  const copyMessage = async (message: ChatMessage) => {
    const text = displayTextForMessage(visibleMessageText(message));
    if (text) await navigator.clipboard?.writeText(text);
    setCopiedMessageId(message.id);
    window.setTimeout(() => setCopiedMessageId(null), 1200);
  };

  const insertSkill = (skillName: string) => {
    const token = `/${skillName}  `;
    setDraft((current) => current.includes(token) ? current : `${token}${current}`);
  };

  const insertControlCommand = (command: AgentControlCommand) => {
    setDraft(`/${command.name}${command.argsHint ? " " : ""}`);
  };

  const handleComposerKeyDown = (event: ReactKeyboardEvent<HTMLTextAreaElement>) => {
    if (slashCommandSuggestions.length > 0) {
      if (event.key === "ArrowDown") {
        event.preventDefault();
        setSelectedSlashCommandIndex((current) => (current + 1) % slashCommandSuggestions.length);
        return;
      }
      if (event.key === "ArrowUp") {
        event.preventDefault();
        setSelectedSlashCommandIndex((current) => (current - 1 + slashCommandSuggestions.length) % slashCommandSuggestions.length);
        return;
      }
      if (event.key === "Tab" || (event.key === "Enter" && !event.shiftKey)) {
        event.preventDefault();
        insertControlCommand(slashCommandSuggestions[selectedSlashCommandIndex] ?? slashCommandSuggestions[0]);
        return;
      }
    }

    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void submit();
    }
  };

  const sendEmojiImage = (path: string) => {
    const mime = imageMimeType(path);
    const marker = `[media attached: "${path}" (${mime})]`;
    setDraft((current) => [current.trim(), marker].filter(Boolean).join("\n\n"));
    setEmojiPickerOpen(false);
  };

  const insertEmoji = (emoji: string) => {
    setDraft((current) => `${current}${emoji}`);
  };

  return (
    <section className="claw-chat-shell">
      <aside className="claw-chat-sidebar">
        <div className="claw-side-head">
          <div>
            <span>Sessions</span>
            <strong>对话</strong>
          </div>
          <button onClick={() => void createConversation(selectedPersona?.id)} title="新建会话" type="button">
            <Plus size={16} />
          </button>
        </div>
        <label className="claw-search">
          <Search size={15} />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索会话" />
        </label>
        <div className="claw-session-list">
          {filteredConversations.map((conversation) => {
            const persona = personaById.get(conversation.personaId || "");
            return (
              <div className={[
                "claw-session",
                conversation.id === activeConversationId ? "active" : "",
                settlingConversationId === conversation.id ? "settling" : ""
              ].filter(Boolean).join(" ")} key={conversation.id}>
                <button disabled={settlingConversationId === conversation.id} onClick={() => selectConversationWithScrollMemory(conversation.id)} type="button">
                  <Avatar
                    name={persona?.name || conversation.title}
                    src={persona?.avatarPath ? api.assetUrl(persona.avatarPath) : ""}
                  />
                  <span>
                    <strong>{persona?.name || conversation.title}</strong>
                    <small>{settlingConversationId === conversation.id ? "删除中，记忆稍后整理..." : conversation.lastMessage || "暂无消息"}</small>
                  </span>
                  {(() => {
                    const count = conversation.id === activeConversationId
                      ? Math.max(conversationUnreadCounts[conversation.id] ?? 0, unreadCount)
                      : (conversationUnreadCounts[conversation.id] ?? 0);
                    return count > 0
                      ? <span aria-label={`${count} 条未读消息`} className="claw-unread-badge" title={`${count} 条未读消息`} />
                      : null;
                  })()}
                </button>
                <button
                  className="claw-session-delete"
                  disabled={Boolean(settlingConversationId)}
                  onClick={() => void deleteConversationWithMemorySettling(conversation.id)}
                  title="整理会话记忆后删除会话"
                  type="button"
                >
                  {settlingConversationId === conversation.id ? <Loader2 className="spin" size={14} /> : <Trash2 size={14} />}
                </button>
                {settlingConversationId === conversation.id ? (
                  <div className="claw-memory-settling">
                    <span />
                  </div>
                ) : null}
              </div>
            );
          })}
          {filteredConversations.length === 0 ? (
            <div className="claw-empty-small">
              <MessageSquareText size={28} />
              <span>还没有对话</span>
            </div>
          ) : null}
        </div>
      </aside>

      <article className="claw-chat-main" ref={chatShellRef}>
        <header className="claw-chat-toolbar">
          <div className="claw-toolbar-title">
            <Sparkles size={17} />
            <div>
              <span>{activeRun ? runStateLabel(activeRun.state) : agentReady ? "Agent runtime ready" : "Agent runtime disabled"}</span>
              <strong>{agentLabel(activeAgent)}</strong>
            </div>
          </div>
          <div className="claw-toolbar-actions">
            <label className="claw-select">
              <Bot size={14} />
              <select value={toolbarPersona?.id ?? selectedPersona?.id ?? ""} onChange={(event) => setSelectedPersonaId(event.target.value)}>
                {!toolbarPersona && visiblePersonas.length === 0 ? <option value="">无可用角色</option> : null}
                {visiblePersonas.map((persona) => <option key={persona.id} value={persona.id}>{persona.name}</option>)}
              </select>
            </label>
            <label className="claw-select">
              <Network size={14} />
              <select value={activeAgent?.id ?? ""} onChange={(event) => void switchConversationAgent(event.target.value)}>
                {agents.map((agent) => <option key={agent.id} value={agent.id}>{agent.name}</option>)}
              </select>
            </label>
            <label className="claw-select">
              <ChevronIcon />
              <select disabled={!currentProvider} value={selectedModelKey} onChange={(event) => void switchAgentModel(event.target.value)}>
                <option value="">{providerBinding.providerDisabled ? "服务商已停用" : currentProvider ? "选择模型" : "先在通讯录选择服务商"}</option>
                {modelOptions.map((option) => <option key={option.key} value={option.key}>{option.label}</option>)}
              </select>
            </label>
            <button onClick={() => void refreshChatData(activeConversationId, selectedPersona?.id)} title="刷新" type="button">
              <RefreshCw size={15} />
            </button>
            <button
              className={executionPanelOpen ? "claw-toolbar-btn-active" : ""}
              aria-pressed={executionPanelOpen}
              onClick={() => setExecutionPanelOpen((open) => !open)}
              title={executionPanelOpen ? "隐藏任务编排" : "显示任务编排"}
              type="button"
            >
              {executionPanelOpen ? <PanelRightClose size={15} /> : <PanelRightOpen size={15} />}
            </button>
          </div>
        </header>

        <div className="claw-runtime-strip">
          <button onClick={() => {
            if (activeAgent?.id) setFocusedAgentId(activeAgent.id);
            setSection("agents");
          }} type="button">
            <Bot size={14} />
            <span>Agents</span>
            <strong>{agents.length}</strong>
          </button>
          <button onClick={() => {
            if (activeAgent?.id) setFocusedAgentId(activeAgent.id);
            setMcpPanelMode("local");
            setSection("mcp");
          }} type="button">
            <Wrench size={14} />
            <span>MCP</span>
            <strong>{enabledMcpCount}/{availableMcpServers.length}</strong>
          </button>
          <button onClick={() => {
            if (activeAgent?.id) setFocusedAgentId(activeAgent.id);
            setSkillsPanelMode("local");
            setSection("skills");
          }} type="button">
            <Code2 size={14} />
            <span>Skills</span>
            <strong>{enabledSkillCount}/{skills.length}</strong>
          </button>
          <button onClick={() => setSection("personas")} type="button">
            <Settings2 size={14} />
            <span>Policy</span>
            <strong>{activeToolIterationBudget}</strong>
          </button>
          <button onClick={() => void refreshAgentQueue()} type="button" title="刷新队列">
            <Clock size={14} />
            <span>Queue</span>
            <strong>{activeQueueItems.length}</strong>
          </button>
        </div>

        <div
          className={[
            "claw-chat-body",
            dragActive ? "dragging" : "",
            executionPanelOpen ? "execution-open" : ""
          ].filter(Boolean).join(" ")}
          ref={chatMainRef}
          onDragEnter={handleFileDragEnter}
          onDragOver={handleFileDragOver}
          onDragLeave={handleFileDragLeave}
          onDrop={handleFileDrop}
        >
          <div className="claw-message-stream-wrap">
            <div className="claw-message-stream" ref={scrollRef} onScroll={handleScroll}>
              {messages.length === 0 ? (
                <WelcomePanel
                  disabled={!selectedPersona}
                  onPrompt={(text) => setDraft(text)}
                />
              ) : (
                <>
                  {messages.length >= renderLimit ? (
                    <div className={`claw-history-loader${historyLoading ? " is-loading" : ""}${historyExhausted ? " is-exhausted" : ""}`}>
                      {historyLoading ? (
                        <>
                          <Loader2 className="spin" size={14} />
                          <span>正在加载更早消息...</span>
                        </>
                      ) : historyExhausted ? (
                        <span>已到达当前会话最早消息</span>
                      ) : (
                        <span>继续向上滚动加载更早消息</span>
                      )}
                    </div>
                  ) : null}
                  <MessageList
                    messages={messages}
                    profileName={profile.name}
                    profileAvatar={profile.avatarPath ?? ""}
                    personaName={selectedPersona?.name ?? "assistant"}
                    personaAvatar={selectedPersona?.avatarPath ?? ""}
                    onFirstStreamChar={handleFirstStreamChar}
                    copiedMessageId={copiedMessageId}
                    onCopy={copyMessage}
                    previewCharLimit={previewCharLimit}
                    animatedMessageIds={animatedMessageIds}
                    streamCharsPerSecond={streamCharsPerSecond}
                    onMessageAnimationDone={handleMessageAnimationDone}
                    memoryStats={shortMemoryStats}
                    runStates={runStates}
                    emojiPathIndexes={emojiPathIndexes}
                  />
                </>
              )}
              {thinkingMounted ? (
                <div className={`claw-thinking-row${showThinking ? "" : " is-leaving"}`}>
                  <span className="claw-thinking-orbit" aria-hidden="true">
                    <i />
                    <i />
                    <i />
                  </span>
                  <span>{activeRun ? runStateLabel(activeRun.state) : "正在思考"}</span>
                </div>
              ) : null}
            </div>
            {unreadCount > 0 && !isNearBottom ? (
              <button className="claw-new-msg-bubble" onClick={handleScrollToBottom} type="button">
                <ChevronDown size={16} />
                <span>{unreadCount} 条新消息</span>
              </button>
            ) : null}
          </div>

          <aside className="claw-execution-panel" aria-hidden={!executionPanelOpen}>
            {activeQueueItems.length > 0 ? (
              <div className="claw-panel-card claw-panel-card--queue">
                <div className="claw-panel-head compact">
                  <div className="claw-panel-head-left">
                    <span className="claw-panel-icon claw-panel-icon--queue"><Clock size={14} /></span>
                    <div>
                      <span>Queue</span>
                      <strong>排队请求</strong>
                    </div>
                  </div>
                  <div className="claw-panel-head-right">
                    <small className="claw-count-badge">{activeQueueItems.length}</small>
                  </div>
                </div>
                <div className="claw-panel-body">
                  <div className="claw-agent-queue-list">
                    {activeQueueItems.slice(0, 6).map((item) => {
                      const linkedRun = runByQueueItemId.get(item.id);
                      return (
                      <div className={`claw-agent-queue-row is-${item.status}`} key={item.id}>
                        <div>
                          <span>{queueStatusLabel(item.status)}</span>
                          <small>
                            {formatTime(item.updatedAt || item.createdAt)}
                            {linkedRun ? ` · ${shortRuntimeId(linkedRun.runId)} · ${runStateLabel(linkedRun.state)}` : ` · ${shortRuntimeId(item.id)}`}
                          </small>
                        </div>
                        <p>{item.content}</p>
                        {item.error ? <em>{item.error}</em> : null}
                        {["pending", "running"].includes(item.status) ? (
                          <button onClick={() => void cancelQueuedItem(item.id)} title="取消排队请求" type="button">
                            <X size={12} />
                          </button>
                        ) : null}
                      </div>
                      );
                    })}
                  </div>
                </div>
              </div>
            ) : null}
            {/* ── Execution Graph Card ── */}
            <div className="claw-panel-card claw-panel-card--accent">
              <div className="claw-panel-head" onClick={() => setTimelineCollapsed((v) => !v)} role="button" tabIndex={0} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setTimelineCollapsed((v) => !v); } }}>
                <div className="claw-panel-head-left">
                  <span className="claw-panel-icon claw-panel-icon--primary"><Layers size={14} /></span>
                  <div>
                    <span>Execution Graph</span>
                    <strong>任务编排</strong>
                  </div>
                </div>
                <div className="claw-panel-head-right">
                  {activeRun ? <small className="claw-status-chip claw-status-chip--active">{runStateLabel(activeRun.state)}</small> : <small className="claw-status-chip">idle</small>}
                  <span className="claw-panel-chevron">{timelineCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${timelineCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                {activeRun?.error ? (
                  <div className="claw-run-error">
                    <AlertCircle size={15} />
                    <span>{activeRun.error}</span>
                  </div>
                ) : null}
                <div className="claw-timeline">
                  <div className="claw-tl-node claw-tl-node--done">
                    <div className="claw-tl-dot"><CheckCircle2 size={14} /></div>
                    <div className="claw-tl-content">
                      <div className="claw-tl-head">
                        <span className="claw-tl-title">接收用户目标</span>
                      </div>
                    </div>
                  </div>
                  {runtimeEvents.length > 0 ? (
                    <div className="claw-tl-node claw-tl-node--phase">
                      <div className="claw-tl-dot"><Network size={14} /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">Runtime Stream</span>
                          <small>{runtimeEvents.length} events · cursor {runtimeCursor}</small>
                        </div>
                        <div className="claw-acp-updates">
                          {runtimeEvents.slice(-5).map((event) => (
                            <span className="claw-acp-update" key={`${event.id}-${event.kind}-${runtimeEventTime(event)}`}>
                              {runtimeEventText(event)}
                            </span>
                          ))}
                        </div>
                      </div>
                    </div>
                  ) : null}
                  {activeRunPhases.length > 0 ? (
                    activeRunPhases.slice(-8).map((phase, index) => {
                      const acpUpdateLines = acpUpdateLinesFromDetail(phase.detail).slice(-4);
                      return (
                        <div className="claw-tl-node claw-tl-node--phase" key={`${phase.phase}-${phase.updatedAt}-${index}`}>
                          <div className="claw-tl-dot"><Brain size={14} /></div>
                          <div className="claw-tl-content">
                            <div className="claw-tl-head">
                              <span className="claw-tl-title">{runPhaseLabel(phase.phase)}</span>
                              <small>{formatTime(phase.updatedAt)}</small>
                            </div>
                            {phaseDetailText(phase.detail) ? <p>{phaseDetailText(phase.detail)}</p> : null}
                            {acpUpdateLines.length > 0 ? (
                              <div className="claw-acp-updates">
                                {acpUpdateLines.map((line) => <span className="claw-acp-update" key={line}>{line}</span>)}
                              </div>
                            ) : null}
                          </div>
                        </div>
                      );
                    })
                  ) : null}
                  {activeChildRunCount > 0 ? (
                    <div className="claw-tl-node claw-tl-node--subagents">
                      <div className="claw-tl-dot"><Bot size={14} /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">子智能体</span>
                          <small>{runningChildRunCount > 0 ? `${runningChildRunCount} 个运行中` : `${activeChildRunCount} 个已结束`}</small>
                        </div>
                        <div className="claw-subagent-list">
                          {activeChildRuns.map((run) => {
                            const latestPhase = run.phaseEvents?.[run.phaseEvents.length - 1];
                            const acpUpdateLines = acpUpdateLinesFromDetail(latestPhase?.detail).slice(-3);
                            const activity = run.lastActivityDesc
                              || (latestPhase ? runPhaseLabel(latestPhase.phase) : "")
                              || run.error
                              || run.userRequest
                              || "";
                            const title = subagentTitle(run);
                            return (
                              <div className={`claw-subagent-row is-${run.state}`} key={run.runId}>
                                <div className="claw-subagent-row-head">
                                  <span>{title}</span>
                                  <small>{runStateLabel(run.state)}</small>
                                </div>
                                {compactRunText(run.subagentTask || run.userRequest) ? <p>{compactRunText(run.subagentTask || run.userRequest)}</p> : null}
                                {compactRunText(activity, 100) ? <em>{compactRunText(activity, 100)}</em> : null}
                                <div className="claw-subagent-row-meta">
                                  {typeof run.subagentDepth === "number" ? <span>depth {run.subagentDepth}</span> : null}
                                  {typeof run.subagentMaxIterations === "number" ? <span>max {run.subagentMaxIterations}</span> : null}
                                  {(run.subagentToolsets ?? []).slice(0, 4).map((toolset) => <span key={toolset}>{toolset}</span>)}
                                  <span>{formatTime(run.lastActivityAt || run.updatedAt)}</span>
                                </div>
                                {acpUpdateLines.length > 0 ? (
                                  <div className="claw-acp-updates claw-acp-updates--compact">
                                    {acpUpdateLines.map((line) => <span className="claw-acp-update" key={line}>{line}</span>)}
                                  </div>
                                ) : null}
                                {run.error ? <strong>{run.error}</strong> : null}
                              </div>
                            );
                          })}
                        </div>
                      </div>
                    </div>
                  ) : null}
                  {activeProcessEvents.length > 0 ? (
                    activeProcessEvents.map((event) => (
                      <div className="claw-tl-node claw-tl-node--phase" key={`${event.processId}-${event.type}-${event.createdAt}`}>
                        <div className="claw-tl-dot"><Zap size={14} /></div>
                        <div className="claw-tl-content">
                          <div className="claw-tl-head">
                            <span className="claw-tl-title">{managedProcessEventLabel(event.type)}</span>
                            <small>{formatTime(event.createdAt)}</small>
                          </div>
                          <p>{managedProcessEventText(event)}</p>
                        </div>
                      </div>
                    ))
                  ) : null}
                  {graphEvents.length > 0 ? (
                    compactSteps(graphEvents).map((step, index, arr) => (
                      <TimelineStep step={step} key={step.key} isLast={index === arr.length - 1} />
                    ))
                  ) : null}
                  {activeRun && activeRun.state !== "completed" && activeRun.state !== "failed" && activeRun.state !== "aborted" ? (
                    <div className="claw-tl-node claw-tl-node--phase">
                      <div className="claw-tl-dot"><Brain size={14} className="claw-tl-icon-spin" /></div>
                      <div className="claw-tl-content">
                        <div className="claw-tl-head">
                          <span className="claw-tl-title">{runStateLabel(activeRun.state)}</span>
                          {activeRunActivityAt ? <small>{formatTime(activeRunActivityAt)}</small> : null}
                        </div>
                        {activeRunActivityDesc ? <p>最近活动：{activeRunActivityDesc}</p> : null}
                      </div>
                    </div>
                  ) : null}
                  {graphEvents.length === 0 && activeProcessEvents.length === 0 && !activeRun ? (
                    <div className="claw-panel-hint-box">
                      <Network size={18} />
                      <p>复杂任务会在这里显示规划、工具调用、MCP 返回与最终整理过程。</p>
                    </div>
                  ) : null}
                </div>
              </div>
            </div>

            {/* ── Artifacts Card ── */}
            <div className="claw-panel-card">
              <div className="claw-panel-head compact" onClick={() => setArtifactsCollapsed((v) => !v)} role="button" tabIndex={0} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setArtifactsCollapsed((v) => !v); } }}>
                <div className="claw-panel-head-left">
                  <span className="claw-panel-icon claw-panel-icon--orange"><FolderOpen size={14} /></span>
                  <div>
                    <span>Artifacts</span>
                    <strong>文件与预览</strong>
                  </div>
                </div>
                <div className="claw-panel-head-right">
                  {artifacts.length > 0 ? <small className="claw-count-badge">{artifacts.length}</small> : null}
                  <span className="claw-panel-chevron">{artifactsCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${artifactsCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                <div className="claw-artifact-list">
                  {artifacts.slice(0, 8).map((artifact) => (
                    <button key={artifact.path} onClick={() => setPreviewTarget(artifact)} type="button">
                      {artifact.kind === "image" ? <ImageIcon size={14} /> : <FileText size={14} />}
                      <span>{artifact.title}</span>
                      <small>{artifact.source}</small>
                    </button>
                  ))}
                  {artifacts.length === 0 ? <div className="claw-panel-hint-box"><FolderOpen size={18} /><p>工具生成的截图、文档和附件会显示在这里。</p></div> : null}
                </div>
              </div>
            </div>

            {/* ── Quick Skills Card ── */}
            <div className="claw-panel-card">
              <div className="claw-panel-head compact" onClick={() => setSkillsCollapsed((v) => !v)} role="button" tabIndex={0} onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setSkillsCollapsed((v) => !v); } }}>
                <div className="claw-panel-head-left">
                  <span className="claw-panel-icon claw-panel-icon--indigo"><Zap size={14} /></span>
                  <div>
                    <span>Quick Skills</span>
                    <strong>技能快捷调用</strong>
                  </div>
                </div>
                <div className="claw-panel-head-right">
                  {activeSkills.length > 0 ? <small className="claw-count-badge">{activeSkills.length}</small> : null}
                  <span className="claw-panel-chevron">{skillsCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${skillsCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                <div className="claw-skill-chips">
                  {activeSkills.slice(0, 8).map((skill) => (
                    <button key={skill.id} onClick={() => insertSkill(skill.name)} type="button" title={skill.description}>
                      /{skill.name}
                    </button>
                  ))}
                  {activeSkills.length === 0 ? <div className="claw-panel-hint-box"><Zap size={18} /><p>当前智能体暂无已启用技能，进入 Skills 或 Agents 配置。</p></div> : null}
                </div>
              </div>
            </div>
          </aside>
        </div>

        <footer
          className={`claw-composer${dragActive ? " dragging" : ""}`}
          ref={composerRef}
          onDragEnter={handleFileDragEnter}
          onDragOver={handleFileDragOver}
          onDragLeave={handleFileDragLeave}
          onDrop={handleFileDrop}
        >
          <input
            ref={fileInputRef}
            multiple
            type="file"
            onChange={(event) => {
              if (event.currentTarget.files) void stageFiles(event.currentTarget.files);
              event.currentTarget.value = "";
            }}
            hidden
          />
          <div className="claw-composer-main">
            {emojiPickerOpen ? (
              <EmojiPicker groups={pickerEmojiGroups} onEmoji={insertEmoji} onPick={sendEmojiImage} />
            ) : null}
            {shortContextNotice ? (
              <div className="claw-context-hint">
                <Sparkles size={14} />
                <span>{shortContextNotice}</span>
              </div>
            ) : null}
            {composerError ? (
              <div className="claw-composer-error">
                <AlertCircle size={14} />
                <span>{composerError}</span>
              </div>
            ) : null}
            {slashCommandSuggestions.length > 0 ? (
              <div className="claw-command-suggestions">
                {slashCommandSuggestions.map((command, index) => {
                  const primary = `/${command.name}${command.argsHint ? ` ${command.argsHint}` : ""}`;
                  const aliases = command.aliases.map((alias) => `/${alias}`).join(" ");
                  return (
                    <button
                      className={index === selectedSlashCommandIndex ? "selected" : ""}
                      key={command.name}
                      onClick={() => insertControlCommand(command)}
                      onMouseEnter={() => setSelectedSlashCommandIndex(index)}
                      type="button"
                    >
                      <span>{command.category}</span>
                      <strong>{primary}</strong>
                      <small>{command.description}</small>
                      {aliases ? <code>{aliases}</code> : null}
                    </button>
                  );
                })}
              </div>
            ) : null}
            {attachments.length > 0 ? (
              <div className="claw-attachment-row">
                {attachments.map((file) => (
                  <div className={`claw-attachment ${file.status}`} key={file.id}>
                    {file.preview ? <img src={file.preview} alt={file.fileName} /> : <FileText size={16} />}
                    <span>{file.fileName}</span>
                    {file.status === "staging" ? <Loader2 className="spin" size={13} /> : null}
                    {file.status === "error" ? <small>{file.error || "上传失败"}</small> : null}
                    <button onClick={() => removeAttachment(file.id)} title="移除附件" type="button"><X size={12} /></button>
                  </div>
                ))}
              </div>
            ) : null}
          <textarea
            rows={1}
            value={draft}
            onChange={(event) => {
              setDraft(event.target.value);
              if (composerError) setComposerError(null);
            }}
            onPaste={(event) => {
              if (event.clipboardData.files.length > 0) void stageFiles(event.clipboardData.files);
            }}
            onKeyDown={handleComposerKeyDown}
            placeholder={agentReady ? "描述任务，Enter 发送，Shift+Enter 换行..." : "请先在 Agents / MCP / Skills 中启用运行时配置..."}
          />
          </div>
          <button
            className={`claw-attach-button${voiceInputState !== "idle" ? " is-recording" : ""}`}
            disabled={voiceInputState === "transcribing" || (!voiceSupported && voiceInputState === "idle")}
            onClick={toggleVoiceInput}
            title={voiceInputState === "idle" ? "语音输入" : voiceInputState === "transcribing" ? "正在识别语音" : "停止语音输入"}
            type="button"
          >
            {voiceInputState === "idle" ? <Mic size={17} /> : voiceInputState === "transcribing" ? <Loader2 className="spin" size={17} /> : <MicOff size={17} />}
          </button>
          <button className="claw-attach-button" onClick={() => setEmojiPickerOpen((open) => !open)} title="表情" type="button">
            <Smile size={17} />
          </button>
          <button className="claw-attach-button" onClick={() => fileInputRef.current?.click()} title="发送文件" type="button">
            <Paperclip size={17} />
          </button>
          <button
            disabled={canStopRun ? false : ((!draft.trim() && attachments.every((item) => item.status !== "ready")) || isProcessing || attachments.some((item) => item.status === "staging"))}
            onClick={() => canStopRun ? void stopActiveRun() : void submit()}
            title={canStopRun ? "结束当前运行" : "发送"}
            type="button"
          >
            {canStopRun ? <Square size={15} fill="currentColor" /> : <SendHorizontal size={17} />}
          </button>
        </footer>
        {dragActive ? (
          <div
            className="claw-file-drop-overlay"
            onDragEnter={handleFileDragEnter}
            onDragOver={handleFileDragOver}
            onDragLeave={handleFileDragLeave}
            onDrop={handleFileDrop}
          >
            <div className="claw-file-drop-message">
              <Paperclip size={24} />
              <strong>松开即可添加</strong>
              <span>文件会作为本轮消息附件上传</span>
            </div>
          </div>
        ) : null}
        {previewTarget ? <ArtifactPreview target={previewTarget} onClose={() => setPreviewTarget(null)} /> : null}
      </article>
    </section>
  );
});

function ChevronIcon() {
  return <Eye size={14} />;
}

const WelcomePanel = memo(function WelcomePanel({ disabled, onPrompt }: { disabled: boolean; onPrompt: (text: string) => void }) {
  const prompts = [
    "打开 https://example.com，截图并总结页面内容",
    "联网搜索今天 AI 新闻，整理三条要点",
    "列出当前工作目录的文件，并解释项目结构"
  ];
  return (
    <div className="claw-welcome">
      <div className="claw-welcome-mark"><Sparkles size={28} /></div>
      <h2>今天要让 Agent 做什么？</h2>
      <p>支持 MCP 工具调用、Skills 注入、浏览器/文件任务和多步骤执行图。</p>
      <div>
        {prompts.map((prompt) => (
          <button disabled={disabled} key={prompt} onClick={() => onPrompt(prompt)} type="button">
            {prompt}
          </button>
        ))}
      </div>
    </div>
  );
});

const STANDARD_EMOJIS = [
  "😀","😃","😄","😁","😆","😅","😂","🤣","😊","😇",
  "🙂","🙃","😉","😌","😍","🥰","😘","😗","😙","😚",
  "😋","😛","😜","🤪","😝","🤑","🤗","🤭","🤫","🤔",
  "🤐","🤨","😐","😑","😶","😏","😒","🙄","😬","🤥",
  "😎","🤓","🥸","🧐","😕","😟","🙁","☹️","😮","😯",
  "😲","😳","🥺","🥹","😦","😧","😨","😰","😥","😢",
  "😭","😱","😖","😣","😞","😓","😩","😪","🤤","😴",
  "😷","🤒","🤕","🤢","🤮","🤧","🥵","🥶","🥴","😵",
  "😡","😠","🤬","😈","👿","💀","💩","🤡","👻","👽",
  "🤖","😺","😸","😹","😻","😼","😽","🙀","😿","😾",
  "👍","👎","👊","✊","🤛","🤜","👏","🙌","👐","🤲",
  "🤝","🙏","✌️","🤞","🤟","🤘","👌","🤌","👈","👉",
  "👆","👇","☝️","👋","🤙","💪","🦵","🦶","👂","👀",
  "❤️","🧡","💛","💚","💙","💜","🖤","🤍","🤎","💔",
  "💕","💞","💓","💗","💖","💘","💝","💌","💯","💢",
  "💥","💫","💦","💨","🔥","⭐","🌟","✨","🎉","🎈",
  "🎁","🎀","🏆","🏅","🥇","🥈","🥉","⚽","🎵","🎶",
  "🐶","🐱","🐭","🐹","🐰","🦊","🐻","🐼","🐨","🐯",
  "🦁","🐮","🐷","🐸","🐵","🐒","🐔","🐧","🐦","🦅",
  "🌹","🌻","🌷","🌸","🌺","🍀","🍃","🍁","🍂","🌴",
  "🍉","🍊","🍋","🍌","🍍","🍎","🍐","🍑","🍒","🍓",
  "☕","🍵","🍺","🍻","🥂","🍷","🍸","🍹","🍔","🍕"
];

const EMOJI_TAB_ID = "__emoji__";

const EmojiPicker = memo(function EmojiPicker({
  groups,
  onEmoji,
  onPick
}: {
  groups: { id: string; name: string; emotionImages?: Record<string, string[]>; images: string[] }[];
  onEmoji: (emoji: string) => void;
  onPick: (path: string) => void;
}) {
  const firstGroupId = groups[0]?.id ?? "";
  const [groupId, setGroupId] = useState(EMOJI_TAB_ID);
  useEffect(() => {
    if (groupId !== EMOJI_TAB_ID && !groups.some((group) => group.id === groupId)) setGroupId(firstGroupId || EMOJI_TAB_ID);
  }, [firstGroupId, groupId, groups]);
  const group = groups.find((item) => item.id === groupId) ?? groups[0];
  const emotionImages = group?.emotionImages && Object.keys(group.emotionImages).length > 0
    ? group.emotionImages
    : (group?.images ?? []).reduce<Record<string, string[]>>((acc, path) => {
        const parts = path.split(/[\\/]/);
        const emotion = parts.length > 1 ? parts[parts.length - 2] : "default";
        acc[emotion] = [...(acc[emotion] ?? []), path];
        return acc;
      }, {});
  return (
    <div className="claw-emoji-picker">
      <div className="claw-emoji-tabs">
        <button className={groupId === EMOJI_TAB_ID ? "active" : ""} onClick={() => setGroupId(EMOJI_TAB_ID)} type="button">
          Emoji
        </button>
        {groups.map((item) => (
          <button className={item.id === groupId ? "active" : ""} key={item.id} onClick={() => setGroupId(item.id)} type="button">
            {item.name}
          </button>
        ))}
      </div>
      <div className="claw-emoji-scroll">
        {groupId === EMOJI_TAB_ID ? (
          <div className="claw-standard-emoji-grid">
            {STANDARD_EMOJIS.map((emoji, index) => (
              <button key={`${emoji}-${index}`} onClick={() => onEmoji(emoji)} type="button">
                {emoji}
              </button>
            ))}
          </div>
        ) : group ? (
          Object.entries(emotionImages).map(([emotion, images]) => images.length > 0 ? (
            <div className="claw-emoji-section" key={emotion}>
              <strong>{emotion}</strong>
              <div className="claw-emoji-grid">
                {images.map((path) => (
                  <button key={path} onClick={() => onPick(path)} type="button" title={fileNameFromPath(path)}>
                    <img src={api.assetUrl(path)} alt={fileNameFromPath(path)} />
                  </button>
                ))}
              </div>
            </div>
          ) : null)
        ) : <small>暂无表情包</small>}
      </div>
    </div>
  );
});

const MessageRow = memo(function MessageRow({
  message,
  mode,
  elementId,
  thinkingCardsOverride,
  profileName,
  profileAvatar,
  personaName,
  personaAvatar,
  copied,
  onCopy,
  previewCharLimit,
  onFirstStreamChar,
  animateText,
  streamCharsPerSecond,
  onAnimationDone,
  memoryStat,
  runStates,
  emojiPathIndexes
}: {
  message: ChatMessage;
  mode: MessageRenderMode;
  elementId: string;
  thinkingCardsOverride?: ThinkingCard[];
  profileName: string;
  profileAvatar: string;
  personaName: string;
  personaAvatar: string;
  copied: boolean;
  onCopy: () => void;
  previewCharLimit: number;
  onFirstStreamChar?: () => void;
  animateText: boolean;
  streamCharsPerSecond: number;
  onAnimationDone: () => void;
  memoryStat: ShortMemoryMessageStat | null;
  runStates: Map<string, string>;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);
  const parsedToolEvent = mode !== "thinking" && message.role === "tool" ? parseToolEvent(message.content) : null;
  const toolEvent = parsedToolEvent
    ? materializeToolEvent(withToolEventStartedAt(parsedToolEvent, message.createdAt), parsedToolEvent.runId ? runStates.get(parsedToolEvent.runId) : null)
    : null;
  const processEvent = mode !== "thinking" && message.role === "tool" ? parseManagedProcessEvent(message.content) : null;
  const isUser = message.role === "user";
  const rawThinkingCards = thinkingCardsOverride ?? messageThinkingCards(message);
  const thinkingCards = mode !== "content" ? rawThinkingCards : [];
  const visibleText = !isUser && rawThinkingCards.length > 0
    ? stripThinkingCardsFromText(message.content.trim(), rawThinkingCards)
    : message.content.trim();
  const text = mode === "thinking" ? "" : previewText(renderTextForMessage(visibleText), previewCharLimit);
  const canRevealText = mode !== "thinking" && !isUser && !toolEvent && !processEvent;
  const isLiveStreaming = canRevealText && message.source === "desktop-stream";
  const [settlingAfterStream, setSettlingAfterStream] = useState(isLiveStreaming);
  useEffect(() => {
    if (isLiveStreaming) setSettlingAfterStream(true);
  }, [isLiveStreaming]);
  const revealText = canRevealText && (isLiveStreaming || animateText || settlingAfterStream);
  const handleRevealDone = useCallback(() => {
    if (!isLiveStreaming) setSettlingAfterStream(false);
    onAnimationDone();
  }, [isLiveStreaming, onAnimationDone]);
  const displayText = useRevealedText(text, revealText, streamCharsPerSecond, handleRevealDone);
  if (toolEvent && isCanceledToolEvent(toolEvent)) return null;
  if (toolEvent) return <ToolMessage event={toolEvent} />;
  if (processEvent) return <ManagedProcessMessage event={processEvent} />;
  if (!text && thinkingCards.length === 0) return null;
  return (
    <div className={isUser ? "claw-message-row user" : "claw-message-row assistant"} data-message-id={elementId}>
      <Avatar
        name={isUser ? profileName : personaName}
        src={isUser && profileAvatar ? api.assetUrl(profileAvatar) : !isUser && personaAvatar ? api.assetUrl(personaAvatar) : ""}
      />
      <div className="claw-message-content">
        <div className="claw-message-meta">
          <span>{isUser ? profileName : personaName}</span>
          <small>{formatTime(message.createdAt)}{message.source === "wechat" ? " · 微信" : ""}</small>
        </div>
        {thinkingCards.length > 0 ? <ThinkingCards cards={thinkingCards} /> : null}
        {text ? (
          <div className={isUser ? "claw-bubble user" : revealText ? "claw-bubble assistant streaming" : "claw-bubble assistant"}>
            <MarkdownLite
              text={displayText}
              onImageClick={setPreviewSrc}
              streaming={revealText}
              onFirstChar={onFirstStreamChar}
              emojiPathIndexes={emojiPathIndexes}
            />
          </div>
        ) : null}
        {!isUser && mode !== "thinking" ? (
          <div className="claw-message-actions">
            {memoryStat ? (
              <span className={`claw-memory-stat ${memoryStat.tone}`}>
                <Sparkles size={12} />
                {memoryStat.label}
              </span>
            ) : null}
            <button className="claw-copy" onClick={onCopy} type="button">
              {copied ? <CheckCircle2 size={13} /> : <Copy size={13} />}
              {copied ? "已复制" : "复制"}
            </button>
          </div>
        ) : null}
      </div>
      {previewSrc ? <ImagePreviewModal src={previewSrc} onClose={() => setPreviewSrc(null)} /> : null}
    </div>
  );
});

const ThinkingCards = memo(function ThinkingCards({ cards }: { cards: ThinkingCard[] }) {
  return (
    <div className="claw-thinking-card-stack">
      {cards.map((card) => (
        <ThinkingCardView card={card} key={card.key} />
      ))}
    </div>
  );
});

const ThinkingCardView = memo(function ThinkingCardView({ card }: { card: ThinkingCard }) {
  const [expanded, setExpanded] = useState(card.streaming);
  const providerLabel = card.provider === "anthropic"
    ? "Anthropic"
    : card.provider === "openai_responses"
      ? "Responses"
      : "Reasoning";
  const detail = card.summary || (card.redacted ? "服务商返回了受保护的思考内容，当前仅展示占位，不显示原始链路。" : "");
  return (
    <div className={`claw-thinking-card${expanded ? " claw-thinking-card--expanded" : ""}`}>
      <button className="claw-thinking-card-head" onClick={() => setExpanded((value) => !value)} type="button">
        <Brain size={15} />
        <strong>{card.title}</strong>
        <small>{[providerLabel, card.streaming ? "思考中" : card.redacted ? "已隐藏" : "摘要"].filter(Boolean).join(" · ")}</small>
        <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
          <ChevronRight size={14} />
        </span>
      </button>
      <div className={`claw-thinking-card-body${expanded ? " claw-thinking-card-body--open" : ""}`}>
        <div className="claw-thinking-card-inner">
          <p>{detail}</p>
        </div>
      </div>
    </div>
  );
});

type MediaSegment =
  | { kind: "text"; value: string }
  | { kind: "image"; path: string; mimeType: string }
  | { kind: "file"; path: string; mimeType: string };

const MEDIA_MARKER = /\[media attached:\s*(?:"([^"]+)"|`([^`]+)`|([^\]\(]+?))\s*(?:\(([^)]+)\))?\]/gi;
const MEDIA_TAG_MARKER = /`?MEDIA:\s*(?:"([^"\n]+)"|'([^'\n]+)'|`([^`\n]+)`|([A-Za-z]:[\\/][^\n]+|\/[^\n]+|~\/[^\n]+))`?/gi;

function parseMediaSegments(text: string): MediaSegment[] {
  const segments: MediaSegment[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  MEDIA_MARKER.lastIndex = 0;
  while ((match = MEDIA_MARKER.exec(text)) !== null) {
    if (match.index > lastIndex) {
      segments.push({ kind: "text", value: text.slice(lastIndex, match.index) });
    }
    const path = (match[1] || match[2] || match[3] || "").trim();
    const mimeType = (match[4] || (isImagePath(path) ? imageMimeType(path) : "application/octet-stream")).trim();
    if (path) segments.push({ kind: isImagePath(path) || mimeType.startsWith("image/") ? "image" : "file", path, mimeType });
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < text.length) segments.push({ kind: "text", value: text.slice(lastIndex) });
  return segments.flatMap((segment) => segment.kind === "text" ? parseMediaTagSegments(segment.value) : [segment]);
}

function parseMediaTagSegments(text: string): MediaSegment[] {
  const segments: MediaSegment[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  MEDIA_TAG_MARKER.lastIndex = 0;
  while ((match = MEDIA_TAG_MARKER.exec(text)) !== null) {
    if (match.index > lastIndex) {
      segments.push({ kind: "text", value: text.slice(lastIndex, match.index) });
    }
    const path = (match[1] || match[2] || match[3] || match[4] || "").trim();
    const mimeType = isImagePath(path) ? imageMimeType(path) : "application/octet-stream";
    if (path) segments.push({ kind: isImagePath(path) ? "image" : "file", path, mimeType });
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < text.length) segments.push({ kind: "text", value: text.slice(lastIndex) });
  return segments;
}

function isImagePath(path: string): boolean {
  return /\.(png|jpe?g|webp|gif|bmp|svg)$/i.test(path);
}

function imageMimeType(path: string): string {
  if (/\.gif$/i.test(path)) return "image/gif";
  if (/\.webp$/i.test(path)) return "image/webp";
  if (/\.jpe?g$/i.test(path)) return "image/jpeg";
  if (/\.bmp$/i.test(path)) return "image/bmp";
  if (/\.svg$/i.test(path)) return "image/svg+xml";
  return "image/png";
}

const InlineImage = memo(function InlineImage({
  path,
  onClick,
  emojiPathIndexes
}: {
  path: string;
  onClick: (path: string) => void;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const isEmojiAsset = isEmojiAssetPath(path);
  const repairedPath = isEmojiAsset ? repairEmojiAssetPath(path, emojiPathIndexes) : path;
  const repairedKnown = emojiPathIndexes.byPath.has(normalizeEmojiPathKey(repairedPath));
  const [failedPath, setFailedPath] = useState<string | null>(null);
  useEffect(() => {
    setFailedPath(null);
  }, [repairedPath]);
  if (isEmojiAsset && !repairedKnown) return null;
  if (failedPath === repairedPath) return null;
  return (
    <div className="claw-inline-image" onClick={() => onClick(repairedPath)} role="button" tabIndex={0}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") onClick(repairedPath); }}>
      <img
        src={api.assetUrl(repairedPath)}
        alt={fileNameFromPath(repairedPath)}
        loading="lazy"
        onError={() => setFailedPath(repairedPath)}
      />
    </div>
  );
});

const InlineFile = memo(function InlineFile({ path, mimeType }: { path: string; mimeType: string }) {
  return (
    <button className="claw-inline-file" onClick={() => void api.openLocalFile(path)} type="button">
      <span><FileText size={18} /></span>
      <strong>{fileNameFromPath(path)}</strong>
      <small>{mimeType || "application/octet-stream"}</small>
    </button>
  );
});

const MarkdownLite = memo(function MarkdownLite({
  text,
  onImageClick,
  streaming,
  onFirstChar,
  emojiPathIndexes
}: {
  text: string;
  onImageClick?: (path: string) => void;
  streaming?: boolean;
  onFirstChar?: () => void;
  emojiPathIndexes: EmojiPathIndexes;
}) {
  const firstCharFiredRef = useRef(false);

  useEffect(() => {
    if (!streaming) {
      firstCharFiredRef.current = false;
      return;
    }
    if (text.length > 0 && !firstCharFiredRef.current) {
      firstCharFiredRef.current = true;
      onFirstChar?.();
    }
  }, [onFirstChar, streaming, text.length]);

  const segments = parseMediaSegments(text);
  const handleClick = onImageClick ?? (() => {});
  return (
    <>
      {segments.map((seg, i) => {
        if (seg.kind === "image") {
          return <InlineImage key={i} path={seg.path} onClick={handleClick} emojiPathIndexes={emojiPathIndexes} />;
        }
        if (seg.kind === "file") {
          return <InlineFile key={i} path={seg.path} mimeType={seg.mimeType} />;
        }
        const raw = seg.value;
        const blocks = raw.split(/\n{2,}/);
        return blocks.map((block, j) => {
          const trimmed = block.trim();
          if (!trimmed) return null;
          if (trimmed.startsWith("```")) {
            return <pre key={`${i}-${j}`}>{trimmed.replace(/^```[a-zA-Z]*\n?/, "").replace(/```$/, "")}</pre>;
          }
          return <p key={`${i}-${j}`}>{trimmed}</p>;
        });
      })}
    </>
  );
});

const ImagePreviewModal = memo(function ImagePreviewModal({ src, onClose }: { src: string; onClose: () => void }) {
  useEffect(() => {
    const handler = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);
  return (
    <div className="image-preview-backdrop" onClick={onClose} role="presentation">
      <div className="image-preview-dialog" onClick={(e) => e.stopPropagation()} role="dialog" aria-modal="true">
        <div className="image-preview-head">
          <strong>{fileNameFromPath(src)}</strong>
          <div>
            <button onClick={() => void api.openLocalFile(src)} type="button">打开</button>
            <button onClick={onClose} title="关闭" type="button"><X size={15} /></button>
          </div>
        </div>
        <img src={api.assetUrl(src)} alt={fileNameFromPath(src)} />
      </div>
    </div>
  );
});

const ToolStep = memo(function ToolStep({ event }: { event: ToolEvent }) {
  const status = eventStatusLabel(event);
  const isRunning = event.status === "running";
  return (
    <div className={isRunning ? "claw-step active" : event.ok ? "claw-step done" : "claw-step failed"}>
      {isRunning ? <Loader2 size={15} /> : event.ok ? <CheckCircle2 size={15} /> : <AlertCircle size={15} />}
      <span>{event.title || `${event.serverId}.${event.toolName}`}</span>
      <small>{status} · {event.elapsedMs}ms</small>
    </div>
  );
});

interface CompactStep {
  key: string;
  title: string;
  count: number;
  allOk: boolean;
  anyRunning: boolean;
  anyFailed: boolean;
  totalMs: number;
  lastEvent: ToolEvent;
}

function compactSteps(events: ToolEvent[]): CompactStep[] {
  const result: CompactStep[] = [];
  for (const event of events.filter((item) => !isCanceledToolEvent(item))) {
    const title = event.title || `${event.serverId}.${event.toolName}`;
    const prev = result[result.length - 1];
    if (prev && prev.title === title && !prev.anyRunning && !event.status) {
      prev.count++;
      prev.allOk = prev.allOk && event.ok;
      prev.anyFailed = prev.anyFailed || (!event.ok && event.status !== "running");
      prev.totalMs += event.elapsedMs;
      prev.lastEvent = event;
    } else {
      result.push({
        key: `${event.serverId}:${event.toolName}:${event.elapsedMs}:${result.length}`,
        title,
        count: 1,
        allOk: event.ok,
        anyRunning: event.status === "running",
        anyFailed: !event.ok && event.status !== "running",
        totalMs: event.elapsedMs,
        lastEvent: event
      });
    }
  }
  return result;
}

const TimelineStep = memo(function TimelineStep({ step, isLast }: { step: CompactStep; isLast: boolean }) {
  const [expanded, setExpanded] = useState(step.anyRunning);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const statusClass = step.anyRunning ? "running" : step.anyFailed ? "failed" : "done";
  const statusIcon = step.anyRunning
    ? <Loader2 size={14} className="claw-tl-icon-spin" />
    : step.anyFailed
      ? <AlertCircle size={14} />
      : <CheckCircle2 size={14} />;
  const fallbackStartedAtMsRef = useRef(Date.now());
  const elapsedLabel = step.anyRunning ? toolEventElapsedLabel(step.lastEvent, nowMs, fallbackStartedAtMsRef.current) : formatDurationMs(step.totalMs);

  useEffect(() => {
    if (!step.anyRunning) return;
    const timer = window.setInterval(() => setNowMs(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [step.anyRunning]);

  return (
    <div className={`claw-tl-node claw-tl-node--${statusClass}${isLast ? " claw-tl-node--last" : ""}`}>
      <div className="claw-tl-dot">{statusIcon}</div>
      <div className="claw-tl-content">
        <div
          className="claw-tl-head"
          onClick={() => setExpanded((v) => !v)}
          role="button"
          tabIndex={0}
          onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          <span className="claw-tl-title">
            {step.title}
            {step.count > 1 ? <span className="claw-tl-count">x{step.count}</span> : null}
          </span>
          <span className="claw-tl-meta">
            <Clock size={11} />
            {elapsedLabel}
          </span>
        </div>
        {expanded ? (
          <div className="claw-tl-detail">
            {step.lastEvent.summary ? <p>{step.lastEvent.summary}</p> : null}
            {step.lastEvent.error ? <p className="claw-error-text">{step.lastEvent.error}</p> : null}
          </div>
        ) : null}
      </div>
    </div>
  );
});

function toolEventReauthInfo(event: ToolEvent): { state: string; cacheState: string; refreshRisk: string } | null {
  const raw = event.raw as Record<string, any> | null | undefined;
  const errorJson = raw?.errorJson as Record<string, any> | null | undefined;
  const needsReauth = raw?.needsReauth === true || errorJson?.needsReauth === true || errorJson?.needs_reauth === true;
  if (!needsReauth) return null;
  const oauthStatus = errorJson?.oauthStatus as Record<string, any> | null | undefined;
  const tokenStatus = oauthStatus?.tokenStatus as Record<string, any> | null | undefined;
  return {
    state: String(oauthStatus?.state ?? "needs_reauth"),
    cacheState: String(tokenStatus?.cacheState ?? "n/a"),
    refreshRisk: String(tokenStatus?.refreshRisk ?? "n/a")
  };
}

function toolEventElapsedLabel(event: ToolEvent, nowMs = Date.now(), fallbackStartedAtMs?: number): string {
  if (event.status === "running") {
    const startedAt = toolEventStartedAt(event);
    const startedMs = startedAt ? new Date(startedAt).getTime() : NaN;
    const fallbackMs = Number.isFinite(fallbackStartedAtMs) ? Number(fallbackStartedAtMs) : nowMs;
    const liveElapsedMs = Number.isFinite(startedMs)
      ? Math.max(0, nowMs - startedMs)
      : Math.max(1, nowMs - fallbackMs);
    const elapsedMs = Math.max(event.elapsedMs, liveElapsedMs);
    return formatDurationMs(Math.max(1, elapsedMs));
  }
  if (event.elapsedMs > 0) {
    return formatDurationMs(event.elapsedMs);
  }
  if (!event.ok || event.status === "failed") {
    return "即时返回";
  }
  return "0ms";
}

function toolEventPathBadge(event: ToolEvent): { label: string; tone: "neutral" | "success" | "warning" | "danger" } {
  const errorText = `${event.summary ?? ""} ${event.error ?? ""}`.toLowerCase();
  if (event.status === "running") {
    return { label: "检查中", tone: "warning" };
  }
  if (typeof event.exists === "boolean") {
    return event.exists
      ? { label: "存在", tone: "success" }
      : { label: "文件不存在", tone: "danger" };
  }
  if (errorText.includes("file registry stale check failed")) {
    return { label: "读状态已失效", tone: "warning" };
  }
  if (errorText.includes("cannot read current file") || errorText.includes("os error 2")) {
    return { label: "无法读取", tone: "danger" };
  }
  if (!event.ok || event.status === "failed") {
    return { label: "未校验", tone: "neutral" };
  }
  return { label: "未提供状态", tone: "neutral" };
}

type TerminalOutputParts = {
  cwd?: string;
  exitCode?: number;
  command?: string;
  stdout?: string;
  stderr?: string;
  raw?: string;
};

function rawObject(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : {};
}

function rawString(value: unknown) {
  return typeof value === "string" && value.trim() ? value.trim() : "";
}

function rawNumber(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value === "string" && /^-?\d+$/.test(value.trim())) return Number(value.trim());
  return undefined;
}

function parseTerminalOutput(value: string): TerminalOutputParts {
  const text = value.trim();
  if (!text) return {};
  const match = text.match(/(?:^|\n)cwd:\s*(.*?)\n(?:(?:transport|backend|target|sandbox|mode|sync|sessionCwd|image):.*?\n)*exitCode:\s*(-?\d+|unknown)\nstdout:\n([\s\S]*?)\nstderr:\n([\s\S]*)$/);
  if (!match) return {};
  const exitCode = match[2] === "unknown" ? undefined : Number(match[2]);
  return {
    cwd: match[1]?.trim() || undefined,
    exitCode: Number.isFinite(exitCode) ? exitCode : undefined,
    stdout: match[3]?.trimEnd() || "",
    stderr: match[4]?.trimEnd() || "",
    raw: text
  };
}

function toolEventPayload(event: ToolEvent): Record<string, unknown> {
  const raw = rawObject(event.raw);
  return rawObject(raw.payload);
}

function firstTerminalParts(...items: TerminalOutputParts[]): TerminalOutputParts {
  return items.find((item) => Boolean(item.raw)) ?? {};
}

function parseInlineTerminalCommand(value: string): TerminalOutputParts {
  const text = value.trim();
  if (!text) return {};
  const match = text.match(/^([\s\S]*?)\s+[·-]\s+exit\s+(-?\d+)\s*$/i);
  if (!match) return {};
  const command = match[1]?.trim() || "";
  if (!command) return {};
  return {
    command,
    exitCode: Number(match[2]),
    raw: command
  };
}

function terminalCommandLabel(command: string) {
  const first = command.trim().split(/\s+/)[0] || "terminal";
  if (/yt-dlp(?:\.exe)?$/i.test(first)) return "yt-dlp 下载";
  if (/npx(?:\.cmd|\.exe)?$/i.test(first)) return "npx";
  if (/powershell(?:\.exe)?$/i.test(first) || /pwsh(?:\.exe)?$/i.test(first)) return "PowerShell";
  if (/cmd(?:\.exe)?$/i.test(first)) return "cmd";
  return first;
}

const ToolMessage = memo(function ToolMessage({ event }: { event: ToolEvent }) {
  const [expanded, setExpanded] = useState(false);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const fallbackStartedAtMsRef = useRef(Date.now());
  const canOpen = Boolean(event.path && event.exists);
  const isToolImage = canOpen && (event.eventType === "screenshot" || event.eventType === "image" || Boolean(event.mimeType?.startsWith("image/")));
  const isRunning = event.status === "running";
  const reauthInfo = toolEventReauthInfo(event);
  const summaryText = event.summary?.trim() ?? "";
  const bodyText = event.text?.trim() ?? "";
  const errorText = event.error?.trim() ?? "";
  const payload = toolEventPayload(event);
  const bodyTerminalParts = parseTerminalOutput(bodyText);
  const summaryTerminalParts = parseTerminalOutput(summaryText);
  const errorTerminalParts = parseTerminalOutput(errorText);
  const inlineTerminalParts = firstTerminalParts(
    parseInlineTerminalCommand(bodyText),
    parseInlineTerminalCommand(summaryText),
    parseInlineTerminalCommand(errorText)
  );
  const terminalParts = firstTerminalParts(bodyTerminalParts, summaryTerminalParts, errorTerminalParts, inlineTerminalParts);
  const commandText = rawString(payload.command) || terminalParts.command || "";
  const payloadCwd = rawString(payload.cwd) || rawString(payload.workdir);
  const terminalExitCode = rawNumber(payload.exitCode) ?? terminalParts.exitCode;
  const terminalCwd = payloadCwd || terminalParts.cwd || "";
  const terminalStdout = terminalParts.stdout?.trim() ?? "";
  const terminalStderr = terminalParts.stderr?.trim() ?? "";
  const terminalFallbackOutput = terminalParts.raw && !terminalStdout && !terminalStderr && terminalParts.raw !== commandText ? terminalParts.raw : "";
  const isTerminalTool = event.toolName === "terminal" || Boolean(commandText || terminalParts.cwd || terminalParts.exitCode !== undefined);
  const isFailed = !isRunning && (!event.ok || Boolean(event.error) || (typeof terminalExitCode === "number" && terminalExitCode !== 0));
  const cardTitle = isTerminalTool
    ? `终端命令${commandText ? ` · ${terminalCommandLabel(commandText)}` : ""}`
    : event.title || `${event.serverId}.${event.toolName}`;
  const displaySummary = isTerminalTool && commandText
    ? (isFailed ? "命令执行失败，展开查看命令、工作目录和输出。" : "命令执行完成，展开查看命令、工作目录和输出。")
    : summaryText;
  const pathBadge = toolEventPathBadge(event);
  const elapsedLabel = toolEventElapsedLabel(event, nowMs, fallbackStartedAtMsRef.current);
  const duplicateBody = Boolean(displaySummary && bodyText && normalizeToolDetailText(displaySummary) === normalizeToolDetailText(bodyText));
  const duplicateError = Boolean(errorText && (normalizeToolDetailText(errorText) === normalizeToolDetailText(displaySummary) || normalizeToolDetailText(errorText) === normalizeToolDetailText(bodyText)));
  const hasTerminalDetails = Boolean(commandText || terminalCwd || terminalStdout || terminalStderr || terminalFallbackOutput || terminalExitCode !== undefined);
  const hasDetails = Boolean(displaySummary || event.path || isToolImage || canOpen || (bodyText && !duplicateBody) || (errorText && !duplicateError) || reauthInfo || hasTerminalDetails);
  const statusMeta = [
    isRunning ? "执行中..." : isFailed ? "失败" : "成功",
    elapsedLabel
  ].filter(Boolean).join(" · ");

  useEffect(() => {
    if (!isRunning) return;
    const timer = window.setInterval(() => setNowMs(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [isRunning]);

  return (
    <div className="claw-tool-message">
      <div className={`claw-tool-card${isRunning ? " claw-tool-card--running" : ""}${isFailed ? " claw-tool-card--failed" : ""}${expanded ? " claw-tool-card--expanded" : ""}`}>
        <div
          className="claw-tool-head"
          onClick={() => hasDetails && setExpanded((v) => !v)}
          role={hasDetails ? "button" : undefined}
          tabIndex={hasDetails ? 0 : undefined}
          onKeyDown={(e) => { if (hasDetails && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          {isTerminalTool ? <Terminal size={15} /> : <Wrench size={15} />}
          <strong>{cardTitle}</strong>
          <small>{statusMeta}</small>
          {hasDetails ? (
            <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
              <ChevronRight size={14} />
            </span>
          ) : null}
        </div>
        <div className={`claw-tool-body${expanded ? " claw-tool-body--open" : ""}`}>
          <div className="claw-tool-body-inner">
            {displaySummary ? <p>{displaySummary}</p> : null}
            {isTerminalTool && commandText ? (
              <div className="claw-tool-command">
                <div className="claw-tool-command-head">
                  <Code2 size={14} />
                  <span>command</span>
                  {typeof terminalExitCode === "number" ? (
                    <span className={`claw-tool-path-badge claw-tool-path-badge--${terminalExitCode === 0 ? "success" : "danger"}`}>{terminalExitCode === 0 ? "成功" : "失败"}</span>
                  ) : null}
                </div>
                <code>{commandText}</code>
              </div>
            ) : null}
            {isTerminalTool && terminalCwd ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{terminalCwd}</code>
                <span>cwd</span>
              </div>
            ) : null}
            {event.path ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{event.path}</code>
                <span className={`claw-tool-path-badge claw-tool-path-badge--${pathBadge.tone}`}>{pathBadge.label}</span>
              </div>
            ) : null}
            {isToolImage && event.path ? (
              <img className="claw-tool-image" src={api.assetUrl(event.path)} alt="tool output" />
            ) : null}
            {canOpen && event.path ? (
              <div className="claw-tool-actions">
                <button onClick={() => void api.openLocalFile(event.path || "")} type="button">打开</button>
                <button onClick={() => void api.revealLocalFile(event.path || "")} type="button"><FolderOpen size={13} />定位</button>
              </div>
            ) : null}
            {isTerminalTool && terminalStdout ? (
              <div className="claw-tool-output">
                <span>stdout</span>
                <pre>{previewText(terminalStdout, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre>
              </div>
            ) : null}
            {isTerminalTool && terminalStderr ? (
              <div className="claw-tool-output claw-tool-output--stderr">
                <span>stderr</span>
                <pre>{previewText(terminalStderr, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre>
              </div>
            ) : null}
            {isTerminalTool && terminalFallbackOutput ? (
              <div className={`claw-tool-output${isFailed ? " claw-tool-output--stderr" : ""}`}>
                <span>output</span>
                <pre>{previewText(terminalFallbackOutput, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre>
              </div>
            ) : null}
            {bodyText && !duplicateBody && !isTerminalTool ? <pre>{previewText(bodyText, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre> : null}
            {errorText && !duplicateError ? <p className="claw-error-text">{errorText}</p> : null}
            {reauthInfo ? (
              <div className="claw-tool-path">
                <AlertCircle size={14} />
                <code>OAuth {reauthInfo.state}</code>
                <span>{reauthInfo.cacheState} · {reauthInfo.refreshRisk}</span>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
});

const ManagedProcessMessage = memo(function ManagedProcessMessage({ event }: { event: ManagedProcessEvent }) {
  const detail = event.detail ?? {};
  const exitCode = typeof detail.exitCode === "number" ? detail.exitCode : null;
  const line = typeof detail.line === "string" ? detail.line : "";
  const reason = typeof detail.reason === "string" ? detail.reason : "";
  const hasDetails = Boolean(line || reason || event.command || event.cwd);
  const [expanded, setExpanded] = useState(false);

  return (
    <div className="claw-tool-message">
      <div className={`claw-tool-card${expanded ? " claw-tool-card--expanded" : ""}`}>
        <div
          className="claw-tool-head"
          onClick={() => hasDetails && setExpanded((v) => !v)}
          role={hasDetails ? "button" : undefined}
          tabIndex={hasDetails ? 0 : undefined}
          onKeyDown={(e) => { if (hasDetails && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          <Zap size={15} />
          <strong>{managedProcessEventLabel(event.type)}</strong>
          <small>{event.label || event.processId}{exitCode !== null ? ` · exit ${exitCode}` : ""}</small>
          {hasDetails ? (
            <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
              <ChevronRight size={14} />
            </span>
          ) : null}
        </div>
        <div className={`claw-tool-body${expanded ? " claw-tool-body--open" : ""}`}>
          <div className="claw-tool-body-inner">
            <p>{managedProcessEventText(event)}</p>
            {event.command ? <pre>{event.command}</pre> : null}
            {event.cwd ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{event.cwd}</code>
              </div>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
});

const ArtifactPreview = memo(function ArtifactPreview({ target, onClose }: { target: ArtifactTarget; onClose: () => void }) {
  const isImage = target.kind === "image";
  return (
    <div className="claw-artifact-backdrop" onClick={onClose} role="presentation">
      <div className="claw-artifact-dialog" onClick={(event) => event.stopPropagation()} role="dialog" aria-modal="true">
        <div className="claw-artifact-dialog-head">
          <div>
            <span>{target.source}</span>
            <strong>{target.title}</strong>
          </div>
          <div>
            <button onClick={() => void api.openLocalFile(target.path)} type="button">打开</button>
            <button onClick={() => void api.revealLocalFile(target.path)} type="button">定位</button>
            <button onClick={onClose} title="关闭" type="button"><X size={15} /></button>
          </div>
        </div>
        {isImage ? (
          <img src={api.assetUrl(target.path)} alt={target.title} />
        ) : (
          <div className="claw-artifact-file">
            <FileText size={42} />
            <code>{target.path}</code>
            <p>该文件可通过系统应用打开，或在文件管理器中定位。</p>
          </div>
        )}
      </div>
    </div>
  );
});
