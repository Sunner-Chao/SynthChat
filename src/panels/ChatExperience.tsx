import { memo, useCallback, useDeferredValue, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
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
  Trash2,
  Wrench,
  Zap,
  X
} from "lucide-react";
import { api } from "../lib/api";
import { useAppStore } from "../lib/store";
import type { AgentControlCommand, AgentDefinition, AgentRunRecord, AgentRuntimeEvent, ChatAttachment, ChatMessage, LlmProvider, ManagedProcessEvent, ToolEvent, ToolEventEnvelope } from "../lib/types";
import { Avatar } from "../components/common";

type ComposerAttachment = ChatAttachment & {
  preview: string | null;
  status: "ready" | "staging" | "error";
  error?: string;
};

type ArtifactTarget = {
  path: string;
  title: string;
  kind: "image" | "file";
  source: string;
};

type ShortMemoryMessageStat = {
  label: string;
  tone: "tokens" | "messages";
};

const DEFAULT_RENDERED_MESSAGES = 180;
const DEFAULT_ARTIFACT_SCAN_LIMIT = 80;
const DEFAULT_MESSAGE_PREVIEW_CHARS = 12_000;
const DEFAULT_STREAM_CHARS_PER_SECOND = 36;
const DEFAULT_THINKING_MIN_VISIBLE_MS = 1800;
const DEFAULT_BOTTOM_FOLLOW_THRESHOLD_PX = 180;
const DEFAULT_ACTIVE_POLL_INTERVAL_MS = 1500;
const DEFAULT_IDLE_POLL_INTERVAL_MS = 3000;

function clampCount(value: number | undefined, fallback: number, min: number, max: number) {
  if (!Number.isFinite(value)) return fallback;
  return Math.min(max, Math.max(min, Math.floor(value ?? fallback)));
}

function previewText(text: string, limit: number) {
  if (text.length <= limit) return text;
  return `${text.slice(0, limit)}\n\n[内容过长，界面仅预览前 ${limit} 个字符；复制按钮仍会复制完整消息。]`;
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

  useEffect(() => {
    if (!enabled) {
      targetTextRef.current = text;
      completedTextRef.current = text;
      setVisibleText(text);
      return;
    }
    targetTextRef.current = text;
    if (!text) {
      completedTextRef.current = "";
      setVisibleText("");
      onDoneRef.current?.();
      return;
    }
    setVisibleText((current) => text.startsWith(current) ? current : "");
  }, [enabled, text]);

  useEffect(() => {
    onDoneRef.current = onDone;
  }, [onDone]);

  useEffect(() => {
    if (!enabled) return;
    let visibleCount = 0;
    const stepMs = 48;
    const charsPerStep = Math.max(1, Math.round(charsPerSecond * (stepMs / 1000)));
    const timer = window.setInterval(() => {
      setVisibleText((current) => {
        const target = targetTextRef.current;
        visibleCount = Math.max(current.length, visibleCount);
        const nextCount = Math.min(target.length, visibleCount + charsPerStep);
        visibleCount = nextCount;
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

function parseManagedProcessEvent(content: string): ManagedProcessEvent | null {
  try {
    const parsed = JSON.parse(content) as { type?: string; event?: ManagedProcessEvent };
    if (parsed?.type === "managedProcessEvent" && parsed.event) return parsed.event;
  } catch {
    return null;
  }
  return null;
}

function plainText(content: string) {
  return content.trim();
}

function isAttachmentContextLine(line: string) {
  const trimmed = line.trim();
  if (!trimmed.startsWith("{") || !trimmed.includes("\"attachment\"")) return false;
  try {
    const parsed = JSON.parse(trimmed) as { type?: string };
    return parsed?.type === "attachment";
  } catch {
    return false;
  }
}

function displayTextForMessage(content: string) {
  return content
    .split(/\r?\n/)
    .filter((line) => !isAttachmentContextLine(line))
    .join("\n")
    .trim();
}

function formatTime(value?: string | number | null) {
  if (!value) return "";
  const date = typeof value === "number" ? new Date(value) : new Date(value);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

function runStateLabel(state: string) {
  const labels: Record<string, string> = {
    started: "任务已启动",
    planning: "正在规划",
    running_tool: "正在调用工具",
    tool_completed: "工具完成",
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
    tool_started: "工具启动",
    tool_message_recorded: "工具结果记录",
    tool_batch_started: "并行工具启动",
    tool_batch_completed: "并行工具完成",
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

function isTerminalRunState(state: string) {
  return ["completed", "failed", "aborted"].includes(state);
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
  if (event.status === "running") return "调用中";
  if (event.ok) return "成功";
  if (event.timedOut) return "超时";
  return "失败";
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

function agentLabel(agent: AgentDefinition | null | undefined) {
  if (!agent) return "Default Agent";
  return agent.name || agent.id || "Agent";
}

function fileNameFromPath(path: string) {
  return path.split(/[\\/]/).pop() || path;
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
  renderLimit,
  previewCharLimit,
  onFirstStreamChar,
  animatedMessageIds,
  streamCharsPerSecond,
  onMessageAnimationDone,
  memoryStats
}: {
  messages: ChatMessage[];
  profileName: string;
  profileAvatar: string;
  personaName: string;
  personaAvatar: string;
  copiedMessageId: string | null;
  onCopy: (message: ChatMessage) => void;
  renderLimit: number;
  previewCharLimit: number;
  onFirstStreamChar?: () => void;
  animatedMessageIds: Set<string>;
  streamCharsPerSecond: number;
  onMessageAnimationDone: (messageId: string) => void;
  memoryStats: Map<string, ShortMemoryMessageStat>;
}) {
  const visibleMessages = useMemo(() => {
    const sliced = messages.slice(-renderLimit);
    // Deduplicate tool messages by provider call id when available; fall back to legacy tool name.
    const seen = new Set<string>();
    const deduped: typeof sliced = [];
    for (let i = sliced.length - 1; i >= 0; i--) {
      const msg = sliced[i];
      if (msg.role === "tool") {
        const evt = parseToolEvent(msg.content);
        if (evt) {
          const key = toolEventMessageKey(evt);
          if (seen.has(key)) continue;
          seen.add(key);
        }
      }
      deduped.push(msg);
    }
    deduped.reverse();
    return deduped;
  }, [messages, renderLimit]);
  const hiddenCount = messages.length - visibleMessages.length;
  return (
    <>
      {hiddenCount > 0 ? (
        <div className="claw-history-trim">
          已折叠 {hiddenCount} 条更早消息，当前仅渲染最近 {renderLimit} 条以保持页面流畅。
        </div>
      ) : null}
      {visibleMessages.map((message) => (
        <MessageRow
          key={message.id}
          message={message}
          profileName={profileName}
          profileAvatar={profileAvatar}
          personaName={personaName}
          personaAvatar={personaAvatar}
          copied={copiedMessageId === message.id}
          onCopy={() => onCopy(message)}
          previewCharLimit={previewCharLimit}
          onFirstStreamChar={onFirstStreamChar}
          animateText={animatedMessageIds.has(message.id)}
          streamCharsPerSecond={streamCharsPerSecond}
          onAnimationDone={() => onMessageAnimationDone(message.id)}
          memoryStat={memoryStats.get(message.id) ?? null}
        />
      ))}
    </>
  );
});

function providerModelOptions(providers: LlmProvider[]) {
  return providers
    .filter((provider) => provider.enabled || provider.model)
    .map((provider) => ({
      key: `${provider.id}::${provider.model}`,
      providerId: provider.id,
      model: provider.model,
      label: `${provider.name || provider.id}${provider.model ? ` / ${provider.model}` : ""}`
    }));
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
  const selectConversation = useAppStore((state) => state.selectConversation);
  const sendMessage = useAppStore((state) => state.sendMessage);
  const setConversationProcessing = useAppStore((state) => state.setConversationProcessing);
  const setSection = useAppStore((state) => state.setSection);
  const refreshChatData = useAppStore((state) => state.refreshChatData);
  const refreshAgents = useAppStore((state) => state.refreshAgents);
  const refreshSkills = useAppStore((state) => state.refreshSkills);
  const refreshMcpServers = useAppStore((state) => state.refreshMcpServers);
  const refreshAgentQueue = useAppStore((state) => state.refreshAgentQueue);
  const refreshAgentRuns = useAppStore((state) => state.refreshAgentRuns);
  const savePersona = useAppStore((state) => state.savePersona);
  const [draft, setDraft] = useState("");
  const [controlCommands, setControlCommands] = useState<AgentControlCommand[]>([]);
  const [selectedSlashCommandIndex, setSelectedSlashCommandIndex] = useState(0);
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const [selectedPersonaId, setSelectedPersonaId] = useState("");
  const [selectedAgentId, setSelectedAgentId] = useState("");
  const [attachments, setAttachments] = useState<ComposerAttachment[]>([]);
  const [emojiPickerOpen, setEmojiPickerOpen] = useState(false);
  const [pickerEmojiGroups, setPickerEmojiGroups] = useState(emojiGroups);
  const [dragActive, setDragActive] = useState(false);
  const [previewTarget, setPreviewTarget] = useState<ArtifactTarget | null>(null);
  const [copiedMessageId, setCopiedMessageId] = useState<string | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const imageInputRef = useRef<HTMLInputElement>(null);
  const sendingRef = useRef(false);
  const [isNearBottom, setIsNearBottom] = useState(true);
  const [unreadCount, setUnreadCount] = useState(0);
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
  const selectedPersona = personaById.get(selectedPersonaId) ?? personas[0] ?? null;
  const defaultAgent = useMemo(() => agents.find((agent) => agent.isDefault) ?? agents[0] ?? null, [agents]);
  const renderLimit = clampCount(chatConfig?.uiMessageLimit, DEFAULT_RENDERED_MESSAGES, 40, 1000);
  const artifactScanLimit = clampCount(chatConfig?.artifactScanLimit, DEFAULT_ARTIFACT_SCAN_LIMIT, 20, renderLimit);
  const previewCharLimit = clampCount(chatConfig?.uiMessagePreviewChars, DEFAULT_MESSAGE_PREVIEW_CHARS, 2000, 100_000);
  const streamCharsPerSecond = clampCount(chatConfig?.uiStreamCharsPerSecond, DEFAULT_STREAM_CHARS_PER_SECOND, 8, 160);
  const thinkingMinVisibleMs = clampCount(chatConfig?.thinkingMinVisibleMs, DEFAULT_THINKING_MIN_VISIBLE_MS, 0, 8000);
  const bottomFollowThresholdPx = clampCount(chatConfig?.bottomFollowThresholdPx, DEFAULT_BOTTOM_FOLLOW_THRESHOLD_PX, 24, 600);
  const activePollIntervalMs = clampCount(chatConfig?.activePollIntervalMs, DEFAULT_ACTIVE_POLL_INTERVAL_MS, 300, 30_000);
  const idlePollIntervalMs = clampCount(chatConfig?.idlePollIntervalMs, DEFAULT_IDLE_POLL_INTERVAL_MS, 1000, 120_000);
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
        const roundTokens = roundMessages.reduce((t, m) => t + estimateMessageTokens(m.content), state?.summaryTokens ?? 0);
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
        const total = dialogueMessages.reduce((t, m) => t + estimateMessageTokens(m.content), 0);
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
    if (selectedAgentId) return agents.find((agent) => agent.id === selectedAgentId) ?? defaultAgent;
    if (selectedPersona?.agentId) return agents.find((agent) => agent.id === selectedPersona.agentId) ?? defaultAgent;
    return defaultAgent;
  }, [agents, defaultAgent, selectedAgentId, selectedPersona?.agentId]);
  const activeRun = useMemo(
    () => Object.values(activeAgentRuns).find((run) => run.conversationId === activeConversationId && !run.parentRunId),
    [activeAgentRuns, activeConversationId]
  );
  const activeQueueItems = useMemo(() => agentQueue
    .filter((item) => item.conversationId === activeConversationId)
    .filter((item) => item.status !== "completed")
    .sort((a, b) => a.createdAt.localeCompare(b.createdAt)), [activeConversationId, agentQueue]);
  const activePendingQueueCount = activeQueueItems.filter((item) => item.status === "pending").length;
  const activeRunningQueueCount = activeQueueItems.filter((item) => item.status === "running").length;
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
  const activeToolEvents: ToolEvent[] = activeRun?.accumulatedToolEvents?.length
    ? activeRun.accumulatedToolEvents
    : activeRun?.toolEvent
      ? [activeRun.toolEvent]
      : [];
  const activeRunPhases = activeRun?.accumulatedPhases
    ?? (activeRun?.phase ? [{ phase: activeRun.phase, detail: activeRun.detail, updatedAt: activeRun.updatedAt }] : storedRun?.phaseEvents ?? []);
  const activeProcessEvents = useMemo(
    () => managedProcessEvents
      .filter((event) => event.conversationId === activeConversationId || Boolean(activeRun?.runId && event.runId === activeRun.runId))
      .slice(0, 6),
    [activeConversationId, activeRun?.runId, managedProcessEvents]
  );
  const recentMessages = useMemo(() => messages.slice(-renderLimit), [messages, renderLimit]);
  const artifactMessages = useMemo(() => recentMessages.slice(-artifactScanLimit), [artifactScanLimit, recentMessages]);
  const messageToolEvents = useMemo(() => recentMessages
    .map((message) => (message.role === "tool" ? parseToolEvent(message.content) : null))
    .filter((event): event is ToolEvent => Boolean(event)), [recentMessages]);
  const graphEvents = activeToolEvents.length > 0 ? activeToolEvents : messageToolEvents;
  const modelOptions = useMemo(() => providerModelOptions(llmProviders), [llmProviders]);
  const selectedModelKey = selectedPersona?.llmProvider && selectedPersona?.llmModel
    ? `${selectedPersona.llmProvider}::${selectedPersona.llmModel}`
    : "";
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
  const isProcessing = Boolean(activeConversationId && processingConversationIds.includes(activeConversationId));
  const canStopRun = Boolean(stoppableRun);
  const [showThinking, setShowThinking] = useState(false);
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
    const isHidden = activeSection !== "chat";
    const previous = seenMessageContentRef.current;
    const next = new Map<string, string>();
    const changedAssistantIds: string[] = [];
    for (const message of messages) {
      next.set(message.id, message.content);
      if (message.role !== "assistant" || !message.content.trim()) continue;
      if (previous.size > 0 && previous.get(message.id) !== message.content) {
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
  const enabledMcpCount = useMemo(() => mcpServers.filter((server) => server.enabled).length, [mcpServers]);
  const enabledSkillCount = useMemo(() => skills.filter((skill) => skill.enabled).length, [skills]);
  const agentReady = Boolean(agentConfig?.enabled && (agentConfig.mcpEnabled || agentConfig.skillsEnabled));

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
  const scrollRestoreTargetRef = useRef<{ conversationId: string; top: number } | null>(null);
  const scrollPositionMapRef = useRef<Map<string, number>>(new Map());
  const conversationActivatedAtRef = useRef<number>(Date.now());
  const notifiedAssistantMessageIdsRef = useRef<Set<string>>(new Set());

  const saveCurrentScrollPosition = useCallback((conversationId: string | null) => {
    const element = scrollRef.current;
    if (!element || !conversationId) return;
    scrollPositionMapRef.current.set(conversationId, element.scrollTop);
  }, []);

  const selectConversationWithScrollMemory = useCallback((conversationId: string) => {
    saveCurrentScrollPosition(activeConversationId);
    void selectConversation(conversationId);
  }, [activeConversationId, saveCurrentScrollPosition, selectConversation]);

  const deleteConversationWithMemorySettling = useCallback(async (conversationId: string) => {
    if (settlingConversationId) return;
    setSettlingConversationId(conversationId);
    try {
      await deleteConversation(conversationId);
    } finally {
      setSettlingConversationId((current) => current === conversationId ? null : current);
    }
  }, [deleteConversation, settlingConversationId]);

  // Mark conversation switch for instant scroll
  useEffect(() => {
    if (activeConversationId !== prevConversationIdRef.current) {
      prevConversationIdRef.current = activeConversationId;
      conversationActivatedAtRef.current = Date.now();
      setUnreadCount(0);
      setIsNearBottom(true);
      nearBottomRef.current = true;
      // Check if we have a saved position for this conversation
      const savedPosition = activeConversationId ? scrollPositionMapRef.current.get(activeConversationId) : undefined;
      scrollOnNextMessagesRef.current = savedPosition !== undefined ? "restore" : "bottom";
      scrollRestoreTargetRef.current = activeConversationId && savedPosition !== undefined
        ? { conversationId: activeConversationId, top: savedPosition }
        : null;
    }
  }, [activeConversationId]);

  useEffect(() => {
    const previousSection = prevActiveSectionRef.current;
    prevActiveSectionRef.current = activeSection;
    if (previousSection === "chat" && activeSection !== "chat") {
      saveCurrentScrollPosition(activeConversationId);
      return;
    }
    if (previousSection !== "chat" && activeSection === "chat") {
      window.requestAnimationFrame(() => {
        const element = scrollRef.current;
        if (!element || !activeConversationId) return;
        const saved = scrollPositionMapRef.current.get(activeConversationId);
        if (saved === undefined) {
          scrollToBottom();
          return;
        }
        element.scrollTop = saved;
        const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
        const near = distanceFromBottom <= bottomFollowThresholdPx;
        nearBottomRef.current = near;
        setIsNearBottom(near);
      });
    }
  }, [activeConversationId, activeSection, bottomFollowThresholdPx, saveCurrentScrollPosition, scrollToBottom]);

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
          ? scrollRestoreTargetRef.current.top
          : scrollPositionMapRef.current.get(convId);
        if (target !== undefined) {
          const maxTop = Math.max(0, el.scrollHeight - el.clientHeight);
          el.scrollTop = Math.min(target, maxTop);
          const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
          nearBottomRef.current = dist <= bottomFollowThresholdPx;
          setIsNearBottom(nearBottomRef.current);
          attempts += 1;
          if (attempts >= 6 || Math.abs(el.scrollTop - Math.min(target, maxTop)) <= 2) {
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
  }, [activeConversationId, bottomFollowThresholdPx, messages]);

  const handleScroll = useCallback(() => {
    const element = scrollRef.current;
    if (!element) return;
    const distanceFromBottom = element.scrollHeight - element.scrollTop - element.clientHeight;
    const near = distanceFromBottom <= bottomFollowThresholdPx;
    nearBottomRef.current = near;
    setIsNearBottom(near);
    if (scrollOnNextMessagesRef.current) return;
    // Save scroll position for current conversation
    saveCurrentScrollPosition(activeConversationId);
    if (near) {
      setUnreadCount(0);
    }
  }, [activeConversationId, bottomFollowThresholdPx, saveCurrentScrollPosition]);

  const handleScrollToBottom = useCallback(() => {
    setUnreadCount(0);
    setIsNearBottom(true);
    nearBottomRef.current = true;
    scrollToBottom();
  }, [scrollToBottom]);

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
          scrollToBottom();
          return;
        }
      }
      setUnreadCount((c) => c + 1);
    } else {
      setUnreadCount((c) => c + 1);
    }
  }, [activeConversationId, activeSection, bottomFollowThresholdPx, lastMessage, scrollToBottom]);

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

  const removeAttachment = (id: string) => {
    setAttachments((current) => current.filter((item) => item.id !== id));
  };

  const switchPersonaModel = async (key: string) => {
    if (!selectedPersona || !key) return;
    const option = modelOptions.find((item) => item.key === key);
    if (!option) return;
    await savePersona({
      ...selectedPersona,
      llmProvider: option.providerId,
      llmModel: option.model
    });
  };

  const submit = async () => {
    const content = draft.trim();
    const readyAttachments = attachments.filter((item) => item.status === "ready");
    if ((!content && readyAttachments.length === 0) || sendingRef.current) return;
    sendingRef.current = true;
    setDraft("");
    setCompactionTipVisible(false);
    try {
      if (selectedPersona && activeAgent && selectedPersona.agentId !== activeAgent.id) {
        await savePersona({ ...selectedPersona, agentId: activeAgent.id });
      }
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
      await sendMessage(outbound, selectedPersona?.id, activeAgent?.id);
      window.setTimeout(() => void refreshChatData(activeConversationId, selectedPersona?.id), 500);
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
    const content = await api.getMessageContent(message.id).catch(() => message.content);
    await navigator.clipboard?.writeText(content);
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
    const marker = `[media attached: ${path} (${mime})]`;
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
                    <small>{settlingConversationId === conversation.id ? "正在沉淀长期记忆..." : conversation.lastMessage || "暂无消息"}</small>
                  </span>
                  {(() => {
                    const count = conversationUnreadCounts[conversation.id] ?? 0;
                    return count > 0 ? <span className="claw-unread-badge">{count > 99 ? "99+" : count}</span> : null;
                  })()}
                </button>
                <button
                  className="claw-session-delete"
                  disabled={Boolean(settlingConversationId)}
                  onClick={() => void deleteConversationWithMemorySettling(conversation.id)}
                  title="删除会话并沉淀长期记忆"
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

      <article className="claw-chat-main">
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
              <select value={selectedPersona?.id ?? ""} onChange={(event) => setSelectedPersonaId(event.target.value)}>
                {personas.map((persona) => <option key={persona.id} value={persona.id}>{persona.name}</option>)}
              </select>
            </label>
            <label className="claw-select">
              <Network size={14} />
              <select value={activeAgent?.id ?? ""} onChange={(event) => setSelectedAgentId(event.target.value)}>
                {agents.map((agent) => <option key={agent.id} value={agent.id}>{agent.name}</option>)}
              </select>
            </label>
            <label className="claw-select">
              <ChevronIcon />
              <select value={selectedModelKey} onChange={(event) => void switchPersonaModel(event.target.value)}>
                <option value="">选择模型</option>
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
          <button onClick={() => setSection("agents")} type="button">
            <Bot size={14} />
            <span>Agents</span>
            <strong>{agents.length}</strong>
          </button>
          <button onClick={() => setSection("mcp")} type="button">
            <Wrench size={14} />
            <span>MCP</span>
            <strong>{enabledMcpCount}/{mcpServers.length}</strong>
          </button>
          <button onClick={() => setSection("skills")} type="button">
            <Code2 size={14} />
            <span>Skills</span>
            <strong>{enabledSkillCount}/{skills.length}</strong>
          </button>
          <button onClick={() => setSection("settings")} type="button">
            <Settings2 size={14} />
            <span>Config</span>
            <strong>{agentConfig?.maxToolIterations ?? "-"}</strong>
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
          onDragEnter={(event) => {
            event.preventDefault();
            setDragActive(true);
          }}
          onDragOver={(event) => event.preventDefault()}
          onDragLeave={(event) => {
            if (event.currentTarget === event.target) setDragActive(false);
          }}
          onDrop={(event) => {
            event.preventDefault();
            setDragActive(false);
            void stageFiles(event.dataTransfer.files);
          }}
        >
          <div className="claw-message-stream-wrap">
            <div className="claw-message-stream" ref={scrollRef} onScroll={handleScroll}>
              {activeQueueItems.length > 0 ? (
                <div className="claw-queue-banner">
                  <div className="claw-queue-banner-head">
                    <Clock size={15} />
                    <strong>当前会话队列</strong>
                    <span>{activeRunningQueueCount > 0 ? `${activeRunningQueueCount} 个执行中` : `${activePendingQueueCount} 个等待中`}</span>
                  </div>
                  <div className="claw-queue-banner-list">
                    {activeQueueItems.slice(0, 3).map((item) => {
                      const linkedRun = runByQueueItemId.get(item.id);
                      return (
                      <div className={`claw-queue-item is-${item.status}`} key={item.id}>
                        <span>{queueStatusLabel(item.status)}</span>
                        <p>{item.content}</p>
                        <small>
                          {formatTime(item.updatedAt || item.createdAt)}
                          {linkedRun ? ` · ${shortRuntimeId(linkedRun.runId)} · ${runStateLabel(linkedRun.state)}` : ` · ${shortRuntimeId(item.id)}`}
                        </small>
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
              ) : null}
              {messages.length === 0 ? (
                <WelcomePanel
                  disabled={!selectedPersona}
                  onPrompt={(text) => setDraft(text)}
                />
              ) : (
                <MessageList
                  messages={messages}
                  profileName={profile.name}
                  profileAvatar={profile.avatarPath ?? ""}
                  personaName={selectedPersona?.name ?? "assistant"}
                  personaAvatar={selectedPersona?.avatarPath ?? ""}
                  onFirstStreamChar={handleFirstStreamChar}
                  copiedMessageId={copiedMessageId}
                  onCopy={copyMessage}
                  renderLimit={renderLimit}
                  previewCharLimit={previewCharLimit}
                  animatedMessageIds={animatedMessageIds}
                  streamCharsPerSecond={streamCharsPerSecond}
                  onMessageAnimationDone={handleMessageAnimationDone}
                  memoryStats={shortMemoryStats}
                />
              )}
              {showThinking ? (
                <div className="claw-thinking-row">
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
                  {skills.length > 0 ? <small className="claw-count-badge">{skills.length}</small> : null}
                  <span className="claw-panel-chevron">{skillsCollapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}</span>
                </div>
              </div>
              <div className={`claw-panel-body${skillsCollapsed ? " claw-panel-body--collapsed" : ""}`}>
                <div className="claw-skill-chips">
                  {skills.slice(0, 8).map((skill) => (
                    <button key={skill.id} onClick={() => insertSkill(skill.name)} type="button" title={skill.description}>
                      /{skill.name}
                    </button>
                  ))}
                  {skills.length === 0 ? <div className="claw-panel-hint-box"><Zap size={18} /><p>暂无技能，进入 Skills 安装内置技能。</p></div> : null}
                </div>
              </div>
            </div>
          </aside>
        </div>

        <footer className="claw-composer">
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
          <input
            ref={imageInputRef}
            multiple
            accept="image/*"
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
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onPaste={(event) => {
              if (event.clipboardData.files.length > 0) void stageFiles(event.clipboardData.files);
            }}
            onKeyDown={handleComposerKeyDown}
            placeholder={agentReady ? "描述任务，Enter 发送，Shift+Enter 换行..." : "请先在 Agents / MCP / Skills 中启用运行时配置..."}
          />
          </div>
          <button className="claw-attach-button" onClick={() => setEmojiPickerOpen((open) => !open)} title="表情" type="button">
            <Smile size={17} />
          </button>
          <button className="claw-attach-button" onClick={() => imageInputRef.current?.click()} title="发送图片" type="button">
            <ImageIcon size={17} />
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
  memoryStat
}: {
  message: ChatMessage;
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
}) {
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);
  const toolEvent = message.role === "tool" ? parseToolEvent(message.content) : null;
  const processEvent = message.role === "tool" ? parseManagedProcessEvent(message.content) : null;
  const isUser = message.role === "user";
  const text = previewText(displayTextForMessage(plainText(message.content)), previewCharLimit);
  const isStreaming = !isUser && !toolEvent && !processEvent && (message.source === "desktop-stream" || animateText);
  const displayText = useRevealedText(text, isStreaming, streamCharsPerSecond, onAnimationDone);
  if (toolEvent) return <ToolMessage event={toolEvent} />;
  if (processEvent) return <ManagedProcessMessage event={processEvent} />;
  if (!text) return null;
  return (
    <div className={isUser ? "claw-message-row user" : "claw-message-row assistant"}>
      <Avatar
        name={isUser ? profileName : personaName}
        src={isUser && profileAvatar ? api.assetUrl(profileAvatar) : !isUser && personaAvatar ? api.assetUrl(personaAvatar) : ""}
      />
      <div className="claw-message-content">
        <div className="claw-message-meta">
          <span>{isUser ? profileName : personaName}</span>
          <small>{formatTime(message.createdAt)}{message.source === "wechat" ? " · 微信" : ""}</small>
        </div>
        <div className={isUser ? "claw-bubble user" : isStreaming ? "claw-bubble assistant streaming" : "claw-bubble assistant"}>
          <MarkdownLite text={displayText} onImageClick={setPreviewSrc} streaming={isStreaming} onFirstChar={onFirstStreamChar} />
        </div>
        {!isUser ? (
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

type MediaSegment =
  | { kind: "text"; value: string }
  | { kind: "image"; path: string; mimeType: string }
  | { kind: "file"; path: string; mimeType: string };

const MEDIA_MARKER = /\[media attached:\s*(?:"([^"]+)"|`([^`]+)`|([^\]\(]+?))\s*(?:\(([^)]+)\))?\]/gi;

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

const InlineImage = memo(function InlineImage({ path, onClick }: { path: string; onClick: (path: string) => void }) {
  return (
    <div className="claw-inline-image" onClick={() => onClick(path)} role="button" tabIndex={0}
      onKeyDown={(e) => { if (e.key === "Enter" || e.key === " ") onClick(path); }}>
      <img src={api.assetUrl(path)} alt={fileNameFromPath(path)} loading="lazy" />
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

const MarkdownLite = memo(function MarkdownLite({ text, onImageClick, streaming, onFirstChar }: { text: string; onImageClick?: (path: string) => void; streaming?: boolean; onFirstChar?: () => void }) {
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
        if (seg.kind === "image" && isImagePath(seg.path)) {
          return <InlineImage key={i} path={seg.path} onClick={handleClick} />;
        }
        if (seg.kind === "file") {
          return <InlineFile key={i} path={seg.path} mimeType={seg.mimeType} />;
        }
        const raw = seg.kind === "image"
          ? `[media attached: ${seg.path} (${seg.mimeType})]`
          : seg.value;
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
  return (
    <div className={event.status === "running" ? "claw-step active" : event.ok ? "claw-step done" : "claw-step failed"}>
      {event.status === "running" ? <Loader2 size={15} /> : event.ok ? <CheckCircle2 size={15} /> : <AlertCircle size={15} />}
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
  for (const event of events) {
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
  const statusClass = step.anyRunning ? "running" : step.anyFailed ? "failed" : "done";
  const statusIcon = step.anyRunning
    ? <Loader2 size={14} className="claw-tl-icon-spin" />
    : step.anyFailed
      ? <AlertCircle size={14} />
      : <CheckCircle2 size={14} />;
  const elapsedLabel = step.anyRunning ? "执行中..." : step.totalMs >= 1000 ? `${(step.totalMs / 1000).toFixed(1)}s` : `${step.totalMs}ms`;

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

const ToolMessage = memo(function ToolMessage({ event }: { event: ToolEvent }) {
  const [expanded, setExpanded] = useState(event.status === "running");
  const canOpen = Boolean(event.path && event.exists);
  const isToolImage = canOpen && (event.eventType === "screenshot" || event.eventType === "image" || Boolean(event.mimeType?.startsWith("image/")));
  const isRunning = event.status === "running";
  const reauthInfo = toolEventReauthInfo(event);
  const hasDetails = Boolean(event.summary || event.path || isToolImage || canOpen || event.text || event.error || reauthInfo);

  return (
    <div className="claw-tool-message">
      <div className={`claw-tool-card${isRunning ? " claw-tool-card--running" : ""}${expanded ? " claw-tool-card--expanded" : ""}`}>
        <div
          className="claw-tool-head"
          onClick={() => hasDetails && setExpanded((v) => !v)}
          role={hasDetails ? "button" : undefined}
          tabIndex={hasDetails ? 0 : undefined}
          onKeyDown={(e) => { if (hasDetails && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); setExpanded((v) => !v); } }}
        >
          <Wrench size={15} />
          <strong>{event.title || `${event.serverId}.${event.toolName}`}</strong>
          <small>{eventStatusLabel(event)} · {event.elapsedMs}ms</small>
          {hasDetails ? (
            <span className={`claw-tool-chevron${expanded ? " claw-tool-chevron--open" : ""}`}>
              <ChevronRight size={14} />
            </span>
          ) : null}
        </div>
        <div className={`claw-tool-body${expanded ? " claw-tool-body--open" : ""}`}>
          <div className="claw-tool-body-inner">
            {event.summary ? <p>{event.summary}</p> : null}
            {event.path ? (
              <div className="claw-tool-path">
                <FileText size={14} />
                <code>{event.path}</code>
                <span>{event.exists ? "存在" : "不存在"}</span>
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
            {event.text ? <pre>{previewText(event.text, DEFAULT_MESSAGE_PREVIEW_CHARS)}</pre> : null}
            {event.error ? <p className="claw-error-text">{event.error}</p> : null}
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
  const [expanded, setExpanded] = useState(event.type !== "completed");

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
