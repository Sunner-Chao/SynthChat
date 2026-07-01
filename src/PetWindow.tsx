import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type DragEvent as ReactDragEvent, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Eye, EyeOff, FileText, Loader2, Menu, Palette, SendHorizontal, Volume2, VolumeX, X } from "lucide-react";
import { api, convertFileSrc, isTauri } from "./lib/api";
import { isAttachmentContextLine, isMediaDirectiveLine, sanitizeSpeechText, stripToolDirectiveBlocks } from "./lib/messageText";
import { PetStartupAwakening } from "./components/PetStartupAwakening";
import type { AgentRunEvent, ChatAttachment, ChatMessage, Conversation, EmojiGroup, Persona } from "./lib/types";
import {
  PET_ACTIVE_CONTEXT_EVENT,
  PET_ACTIVE_CONTEXT_STORAGE_KEY,
  PET_THINKING_STATE_EVENT,
  PET_THINKING_STATE_STORAGE_KEY,
  parsePetActiveContext,
  parsePetThinkingState,
  readStoredPetActiveContext,
  readStoredPetThinkingState,
  subscribePetThinkingState,
  writeStoredPetActiveContext,
  type PetActiveContext
} from "./lib/petContext";

const HOST_MESSAGE_SOURCE = "synthchat-pet-host";
const FRAME_MESSAGE_SOURCE = "synthchat-pet-frame";
const PET_ACTIVE_CONTEXT_SOURCE = "pet";
const PET_HISTORY_LIMIT = 40;
const PET_PREVIEW_CHARS = 1200;
const PET_MESSAGE_MIRROR_INTERVAL_MS = 3200;
const PET_GLOBAL_LOOK_INTERVAL_MS = 32;
const PET_GLOBAL_LOOK_IDLE_MS = 3000;
const DEFAULT_PET_ASSISTANT_CLOUD_DURATION_SECONDS = 10;
const MIN_PET_ASSISTANT_CLOUD_DURATION_SECONDS = 1;
const MAX_PET_ASSISTANT_CLOUD_DURATION_SECONDS = 120;
const PET_EDGE_SNAP_THRESHOLD_PX = 64;
const PET_EDGE_POINTER_THRESHOLD_PX = 96;
const PET_ORB_CLICK_MOVE_TOLERANCE_PX = 5;
const PET_MODEL_INPUT_WAKE_PADDING_PX = 28;
const PET_INPUT_HOT_ZONE_X_PADDING_PX = 24;
const PET_INPUT_HOT_ZONE_TOP_PADDING_PX = 34;
const PET_INPUT_HOT_ZONE_BOTTOM_PADDING_PX = 26;
const PET_INPUT_FALLBACK_HOT_ZONE_HEIGHT_PX = 116;
const PET_DEFAULT_MODEL_STORAGE_KEY = "synthchat.pet.defaultModelId";
const PET_VISION_INTERVAL_STORAGE_KEY = "synthchat.pet.visionIntervalSeconds";
const PET_VOICE_REPLY_ENABLED_STORAGE_KEY = "synthchat.pet.voiceReplyEnabled";
const DEFAULT_PET_VISION_INTERVAL_SECONDS = 60;
const MIN_PET_VISION_INTERVAL_SECONDS = 30;
const PET_VISION_BUSY_STATES = new Set(["started", "running", "pendingApproval", "needsClarification"]);
const PET_MODEL_INERTIA_MIN_SPEED = 0.1;
const PET_MODEL_INERTIA_MAX_DISTANCE = 180;
const PET_MODEL_INERTIA_DURATION_MS = 680;
const PET_MODEL_INERTIA_DISTANCE_MULTIPLIER = 300;
const PET_MODEL_INERTIA_REBOUND_PX = 14;
const PET_STARTUP_MIN_VISIBLE_MS = 5600;
const PET_STARTUP_MAX_VISIBLE_MS = 8600;
const PET_STARTUP_EXIT_MS = 920;
const PET_STARTUP_REVEAL_AFTER_EXIT_MS = 180;
const PET_CLOUD_STREAM_INTERVAL_MS = 34;
const PET_THINKING_CLOUD_TEXT = "正在思考...";

const AVAILABLE_MODELS = [
  { id: "tororo", name: "Tororo", path: "/pet/model/Tororo/tororo.model3.json", greeting: "Tororo 到啦。", headX: 0.5, headY: 0.24, tailGap: 28 },
  { id: "hijiki", name: "Hijiki", path: "/pet/model/Hijiki/hijiki.model3.json", greeting: "Hijiki 换好了。", headX: 0.5, headY: 0.23, tailGap: 30 },
  { id: "mao", name: "Mao", path: "/pet/model/Mao/Mao.model3.json", greeting: "Mao 在这里。", headX: 0.51, headY: 0.22, tailGap: 32 },
  { id: "wanko", name: "Wanko", path: "/pet/model/Wanko/Wanko.model3.json", greeting: "汪，我换好啦。", headX: 0.5, headY: 0.2, tailGap: 30 },
  { id: "hiyori", name: "Hiyori", path: "/pet/model/Hiyori/Hiyori.model3.json", greeting: "Hiyori 来了。", headX: 0.5, headY: 0.19, tailGap: 34 },
  { id: "natori", name: "Natori", path: "/pet/model/Natori/Natori.model3.json", greeting: "夏鸟已经就位。", headX: 0.49, headY: 0.2, tailGap: 34 },
  { id: "mark", name: "Mark", path: "/pet/model/Mark/Mark.model3.json", greeting: "Mark is ready.", headX: 0.5, headY: 0.22, tailGap: 32 }
];

type PetModel = (typeof AVAILABLE_MODELS)[number];

function fallbackPetModel() {
  return AVAILABLE_MODELS.find((model) => model.id === "hiyori") ?? AVAILABLE_MODELS[0];
}

function readStoredPetModel() {
  if (typeof window === "undefined") return fallbackPetModel();
  try {
    const storedId = window.localStorage.getItem(PET_DEFAULT_MODEL_STORAGE_KEY);
    return AVAILABLE_MODELS.find((model) => model.id === storedId) ?? fallbackPetModel();
  } catch {
    return fallbackPetModel();
  }
}

function writeStoredPetModel(model: PetModel) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(PET_DEFAULT_MODEL_STORAGE_KEY, model.id);
  } catch {
    // ignore preference persistence failures
  }
}

type PetSendContext = {
  conversationId: string;
  personaId: string | null;
  personaName: string | null;
  agentId: string | null;
};

type PetMessage = {
  source?: string;
  type?: string;
  text?: string;
  message?: string;
  area?: string;
  areas?: string[];
  url?: string;
  screenX?: number;
  screenY?: number;
  x?: number;
  y?: number;
  width?: number;
  height?: number;
  group?: string;
  index?: number;
  name?: string;
  duration?: number;
  playedMotion?: boolean;
  parameterCount?: number;
  expressionCount?: number;
  motionGroups?: unknown;
  expressions?: unknown;
  sampleParams?: unknown;
};

type NativeFileDropPayload = {
  type: "enter" | "over" | "drop" | "leave";
  paths?: string[];
  position?: { x: number; y: number };
  windowLabel?: string;
};

type PetModelBounds = {
  x: number;
  y: number;
  width: number;
  height: number;
};

type PetCloudBubble = {
  id: string;
  text: string;
  tone: "soft" | "happy" | "active" | "error";
  attachments?: Array<{fileName: string; path: string; mimeType?: string}>;
};

type PetAttachment = {
  fileName: string;
  path: string;
  mimeType?: string;
};

type PetComposerAttachment = ChatAttachment & {
  preview: string | null;
  status: "ready" | "staging" | "error";
  error?: string;
};

type PetAttachmentRender = PetAttachment & {
  resolvedPath: string;
  isEmojiAsset: boolean;
  hidden: boolean;
  imageFailed?: boolean;
};

type PetCloudStyle = CSSProperties & {
  "--pet-cloud-tail-start-x"?: string;
  "--pet-cloud-tail-start-y"?: string;
  "--pet-cloud-tail-x"?: string;
  "--pet-cloud-tail-y"?: string;
  "--pet-cloud-tail-length"?: string;
  "--pet-cloud-tail-angle"?: string;
  "--pet-cloud-dot-1-x"?: string;
  "--pet-cloud-dot-1-y"?: string;
  "--pet-cloud-dot-2-x"?: string;
  "--pet-cloud-dot-2-y"?: string;
  "--pet-cloud-dot-3-x"?: string;
  "--pet-cloud-dot-3-y"?: string;
};

type PetCursorPosition = {
  x?: number;
  y?: number;
  screenX?: number;
  screenY?: number;
  screenWidth?: number;
  screenHeight?: number;
  screenXOrigin?: number;
  screenYOrigin?: number;
  clientX?: number;
  clientY?: number;
  windowWidth?: number;
  windowHeight?: number;
  windowScreenX?: number;
  windowScreenY?: number;
};

type PetDragPoint = {
  screenX: number;
  screenY: number;
};

type PetDragInertiaSnapshot = {
  moved: boolean;
  velocity: { x: number; y: number };
  from: PetDragPoint | null;
};

type PetDockEdge = "left" | "right";
type PetWindowMode = "model" | "orb";
type PetBehaviorName = "idle" | "thinking" | "happy" | "stretch" | "error" | "curious" | "shy" | "sleepy" | "wave" | "nod" | "proud" | "listening" | "speaking" | "surprise";

type PetAssistantMirrorState = {
  messageId: string;
  signature: string;
};

type PetAssistantStreamRuntime = {
  conversationId: string;
  messageId: string;
  requestKey: string;
  bubbleId: string;
  tone: PetCloudBubble["tone"];
  attachments?: PetCloudBubble["attachments"];
  text: string;
  finalized: boolean;
};

type PetThinkingCloudRuntime = {
  conversationId: string;
  bubbleId: string;
};

function formatCloudText(text: string) {
  const normalized = text
    .split(/\r?\n/)
    .map((line) => line.trim().replace(/[ \t]+/g, " "))
    .filter(Boolean)
    .join(" ")
    .trim();
  if (!normalized) return "";
  return normalized.length > 360 ? `${normalized.slice(0, 360)}...` : normalized;
}

function petVoiceReplySpeechOptions(voiceReply: NonNullable<Persona["voiceReply"]>) {
  return {
    format: "wav",
    engine: voiceReply.engine || undefined,
    language: voiceReply.language || undefined,
    voice: voiceReply.voice || undefined,
    volume: voiceReply.volume || undefined,
    pitch: voiceReply.pitch || undefined,
    speedScale: "chattts",
    speed: voiceReply.speed,
    modelDir: voiceReply.modelDir || undefined,
    pythonPath: voiceReply.pythonPath || undefined,
    sampleRate: voiceReply.sampleRate,
    oral: voiceReply.oral,
    laugh: voiceReply.laugh,
    breakLevel: voiceReply.breakLevel,
    speakerSeed: voiceReply.speakerSeed,
    speakerEmbedding: voiceReply.speakerEmbedding || undefined,
    temperature: voiceReply.temperature,
    topP: voiceReply.topP,
    topK: voiceReply.topK,
    refineTextEnabled: voiceReply.refineTextEnabled,
    refinePrompt: voiceReply.refinePrompt || undefined,
    refineTemperature: voiceReply.refineTemperature
  };
}

function clampPetCloudDurationSeconds(value: unknown) {
  const numeric = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(numeric)) return DEFAULT_PET_ASSISTANT_CLOUD_DURATION_SECONDS;
  return Math.max(
    MIN_PET_ASSISTANT_CLOUD_DURATION_SECONDS,
    Math.min(MAX_PET_ASSISTANT_CLOUD_DURATION_SECONDS, Math.round(numeric))
  );
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function textValue(value: unknown) {
  return typeof value === "string" && value.trim() ? value.trim() : "";
}

function petShouldFollowThinkingTurn(source: string) {
  const normalized = source.trim();
  if (!normalized) return true;
  if (normalized === "pet-vision") return false;
  if (normalized === "proactive-internal") return false;
  if (normalized === "desktop-control") return false;
  if (normalized === "desktop-diagnosis") return false;
  if (normalized === "desktop-agent-error") return false;
  if (normalized === "desktop-agent-tool") return false;
  return !normalized.startsWith("desktop-local-");
}

function petShouldShowThinkingCloud(source: string) {
  const normalized = source.trim();
  return normalized !== "pet-vision" && normalized !== "proactive-internal";
}

function attachmentName(path: string, fileName?: string) {
  return textValue(fileName) || path.split("/").pop()?.split("\\").pop()?.trim() || "附件";
}

function normalizeAttachmentRecord(value: unknown): PetAttachment | null {
  const record = asRecord(value);
  if (!record) return null;
  const path = textValue(record.path) || textValue(record.mediaPath) || textValue(record.visiblePath);
  if (!path) return null;
  const fileName = attachmentName(path, textValue(record.fileName) || textValue(record.name));
  const mimeType = textValue(record.mimeType) || textValue(record.mime_type) || undefined;
  return { fileName, path, mimeType };
}

function structuredMessageAttachments(message: ChatMessage | null | undefined): PetAttachment[] {
  const providerData = asRecord(message?.providerData);
  if (!providerData) return [];
  const attachments: PetAttachment[] = [];
  const push = (value: unknown) => {
    const attachment = normalizeAttachmentRecord(value);
    if (!attachment) return;
    if (attachments.some((item) => item.path === attachment.path && item.fileName === attachment.fileName)) {
      return;
    }
    attachments.push(attachment);
  };
  push(providerData);
  for (const key of ["attachments", "attachmentContexts", "attachment_contexts", "mediaFiles", "media_files"]) {
    const items = providerData[key];
    if (Array.isArray(items)) {
      for (const item of items) push(item);
    }
  }
  return attachments;
}

function assistantMessageVisibleInCloud(message: ChatMessage | null | undefined) {
  if (!message || message.role !== "assistant") return false;
  if (message.source === "desktop-agent-error") return false;
  if (message.source === "desktop-control") return false;
  if (message.source === "desktop-diagnosis") return false;
  if (message.source?.startsWith("desktop-local-")) return false;
  return Boolean(assistantCloudPayload(message));
}

function latestAssistantMessage(messages: ChatMessage[]) {
  return [...messages]
    .reverse()
    .find((message) => assistantMessageVisibleInCloud(message));
}

// Keep bubble text and attachments on separate paths so marker lines never
// leak into the visible cloud text.
function messageToCloudText(message: ChatMessage | null | undefined) {
  if (!message) return "";
  const textLines = stripToolDirectiveBlocks(message.content)
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .filter((line) => !isAttachmentContextLine(line) && !isMediaDirectiveLine(line));
  return formatCloudText(textLines.join("\n"));
}

function extractCloudAttachments(message: ChatMessage | null | undefined): PetAttachment[] {
  const results = structuredMessageAttachments(message);
  for (const item of extractCloudAttachmentsFromContent(message?.content ?? "")) {
    if (!results.some((existing) => existing.path === item.path && existing.fileName === item.fileName)) {
      results.push(item);
    }
  }
  return results;
}

function extractCloudAttachmentsFromContent(rawContent: string): PetAttachment[] {
  const results: PetAttachment[] = [];
  for (const line of stripToolDirectiveBlocks(rawContent).split("\n")) {
    const trimmed = line.trim();
    if (isAttachmentContextLine(trimmed)) {
      try {
        const parsed = JSON.parse(trimmed) as { fileName?: string; path?: string; mimeType?: string };
        if (parsed.path) {
          results.push({
            fileName: parsed.fileName || parsed.path.split("/").pop()?.split("\\").pop() || "附件",
            path: parsed.path,
            mimeType: parsed.mimeType
          });
        }
      } catch { /* ignore */ }
    } else if (isMediaDirectiveLine(trimmed)) {
      const m = trimmed.match(/\[media attached:\s*"([^"]+)"(?:\s*\(([^)]+)\))?\]/i);
      if (m) {
        results.push({
          fileName: m[1].split("/").pop()?.split("\\").pop() || "附件",
          path: m[1],
          mimeType: m[2]
        });
      }
    }
  }
  return results;
}

function attachmentIdentity(attachment: PetAttachment) {
  return `${attachment.path}::${attachment.fileName}::${attachment.mimeType ?? ""}`;
}

function isImageAttachment(attachment: Pick<PetAttachment, "path" | "mimeType" | "fileName">) {
  const mime = attachment.mimeType?.toLowerCase() ?? "";
  if (mime.startsWith("image/")) return true;
  const target = `${attachment.path} ${attachment.fileName}`.toLowerCase();
  return /\.(png|jpe?g|gif|webp|bmp|svg)$/.test(target);
}

function normalizeEmojiPathKey(path: string) {
  return path.replace(/\//g, "\\").toLowerCase();
}

function isEmojiAssetPath(path: string) {
  return normalizeEmojiPathKey(path).includes("\\emoji\\");
}

function tryRepairEmojiAttachmentPath(
  path: string,
  emojiPathIndex: Map<string, string>,
  emojiFileFallbackIndex: Map<string, string>
) {
  const normalized = normalizeEmojiPathKey(path);
  if (emojiPathIndex.has(normalized)) {
    return emojiPathIndex.get(normalized) ?? path;
  }
  const marker = "\\emoji\\";
  const markerIndex = normalized.indexOf(marker);
  if (markerIndex < 0) return path;
  const relative = normalized.slice(markerIndex + marker.length);
  const segments = relative.split("\\").filter(Boolean);
  if (segments.length < 3) return path;
  const [groupId, emotionId, fileName] = segments;
  const canonical = emojiFileFallbackIndex.get(`${groupId}::${emotionId}::${fileName}`);
  return canonical ?? path;
}

function assistantCloudPayload(message: ChatMessage | null | undefined) {
  if (!message || message.role !== "assistant") return null;
  const text = messageToCloudText(message);
  const attachments = extractCloudAttachments(message);
  if (!text && attachments.length === 0) return null;
  const signature = [
    message.id,
    text,
    ...attachments.map(attachmentIdentity).sort((a, b) => a.localeCompare(b, "zh-CN"))
  ].join("\n");
  return { text, attachments, signature };
}

function touchAreaCloudText(area: string | undefined, count: number) {
  const normalized = (area ?? "").toLowerCase();
  const variants = normalized === "head"
    ? ["摸摸头收到。", "这样会把头发弄乱啦。", "嗯，我在认真听。"]
    : normalized === "body" || normalized === "belly"
      ? ["哎呀，戳到我了。", "我会站稳一点。", "别闹，我还在看着当前对话呢。"]
      : ["我在哦。", "有什么想问的，直接在下面说就好。", "我会在这里看着当前对话。"];
  const text = variants[Math.max(0, count - 1) % variants.length];
  return `${text} 悄悄说一句，双击我就能打开主窗口啦。`;
}

function fileNameFromLocalPath(path: string) {
  return path.split(/[\\/]/).pop() || "attachment";
}

function decodePetVisionDataUrl(dataUrl: string) {
  const match = /^data:([^;,]+);base64,(.+)$/i.exec(dataUrl.trim());
  if (!match) throw new Error("invalid screen capture data url");
  const mimeType = match[1] || "image/jpeg";
  const binary = atob(match[2]);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return { mimeType, bytes };
}

function petVisionFileName() {
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  return `pet-screen-${stamp}.jpg`;
}

function clampPetVisionIntervalSeconds(value: unknown) {
  const numeric = typeof value === "number" ? value : Number(value);
  if (!Number.isFinite(numeric)) return DEFAULT_PET_VISION_INTERVAL_SECONDS;
  return Math.max(MIN_PET_VISION_INTERVAL_SECONDS, Math.round(numeric));
}

function readStoredPetVisionIntervalSeconds() {
  if (typeof window === "undefined") return DEFAULT_PET_VISION_INTERVAL_SECONDS;
  try {
    return clampPetVisionIntervalSeconds(window.localStorage.getItem(PET_VISION_INTERVAL_STORAGE_KEY));
  } catch {
    return DEFAULT_PET_VISION_INTERVAL_SECONDS;
  }
}

function readStoredPetVoiceReplyEnabled() {
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem(PET_VOICE_REPLY_ENABLED_STORAGE_KEY) === "true";
  } catch {
    return false;
  }
}

function writeStoredPetVoiceReplyEnabled(enabled: boolean) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(PET_VOICE_REPLY_ENABLED_STORAGE_KEY, String(enabled));
  } catch {
    // ignore preference persistence failures
  }
}

function defaultPetVoiceReplyConfig(): NonNullable<Persona["voiceReply"]> {
    return {
      enabled: false,
      engine: "chattts",
      language: "zh-CN",
      voice: "zh-CN-XiaoxiaoNeural",
      volume: "+0%",
      pitch: "+0Hz",
      pythonPath: "",
    modelDir: "",
    sampleRate: 16000,
    speed: 5,
    oral: 2,
    laugh: 0,
    breakLevel: 4,
    speakerSeed: 20240,
    speakerEmbedding: "",
    temperature: 0.3,
    topP: 0.7,
    topK: 20,
    refineTextEnabled: true,
    refinePrompt: "[oral_2][laugh_0][break_4]",
    refineTemperature: 0.7
  };
}

function normalizePetVoiceReplyConfig(config?: Persona["voiceReply"] | null): NonNullable<Persona["voiceReply"]> {
  return { ...defaultPetVoiceReplyConfig(), ...(config ?? {}) };
}

function buildPetVisionContent(attachment: { fileName: string; path: string; mimeType?: string }) {
  const prompt = "【视觉感知：这是一张刚刚截取的用户当前屏幕截图。请直接根据图片内容，用中文简短评价用户可能正在做什么，像桌宠一样给出一两句话的关心或吐槽；不要描述你无法看到图片，也不要展开长篇说明。】";
  const marker = `[media attached: "${attachment.path}" (${attachment.mimeType || "image/jpeg"})] ${attachment.fileName}`;
  const context = JSON.stringify({
    type: "attachment",
    id: attachment.fileName,
    fileName: attachment.fileName,
    mimeType: attachment.mimeType || "image/jpeg",
    path: attachment.path,
    recommendedTool: "vision_analyze",
    source: "pet-vision"
  });
  return [prompt, marker, context].join("\n\n");
}

function hasFileDragData(dataTransfer: DataTransfer | null) {
  if (!dataTransfer) return false;
  if (dataTransfer.files.length > 0) return true;
  return Array.from(dataTransfer.types).includes("Files")
    || Array.from(dataTransfer.items).some((item) => item.kind === "file");
}

export function PetWindow() {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const inputShellRef = useRef<HTMLElement>(null);
  const modelMenuRef = useRef<HTMLDivElement>(null);
  const activeContextRef = useRef<PetActiveContext | null>(readStoredPetActiveContext());
  const frameReadyRef = useRef(false);
  const selectedModelRef = useRef<PetModel>(readStoredPetModel());
  const pendingModelLoadRef = useRef<{ model: PetModel; force: boolean } | null>(null);
  const modelBoundsRef = useRef<PetModelBounds | null>(null);
  const modelDragActiveRef = useRef(false);
  const modelDragMovedRef = useRef(false);
  const modelDragTokenRef = useRef(0);
  const modelDragStartReadyRef = useRef(false);
  const modelDragLatestPointRef = useRef<PetDragPoint | null>(null);
  const modelDragLastMovePointRef = useRef<PetDragPoint | null>(null);
  const modelDragVelocityRef = useRef({ x: 0, y: 0 });
  const modelDragLastSampleRef = useRef<{ point: PetDragPoint; at: number } | null>(null);
  const modelDragInertiaFrameRef = useRef<number | null>(null);
  const modelDragMoveFrameRef = useRef<number | null>(null);
  const orbDragActiveRef = useRef(false);
  const orbDragMovedRef = useRef(false);
  const orbDragStartPointRef = useRef<PetDragPoint | null>(null);
  const dockEdgeRef = useRef<PetDockEdge>("right");
  const petWindowModeRef = useRef<PetWindowMode>("model");
  const modelLoadedRef = useRef(false);
  const petStartupStartedAtRef = useRef(Date.now());
  const petStartupClosingRef = useRef(false);
  const petStartupTimersRef = useRef<number[]>([]);
  const ignoreCursorEventsRef = useRef(false);
  const sendingRef = useRef(false);
  const cloudTimerRef = useRef<number | null>(null);
  const cloudStreamTimerRef = useRef<number | null>(null);
  const cloudTextDraftsRef = useRef<Map<string, string>>(new Map());
  const voiceAudioRef = useRef<HTMLAudioElement | null>(null);
  const activeVoiceReplyRequestRef = useRef<string | null>(null);
  const assistantStreamRef = useRef<PetAssistantStreamRuntime | null>(null);
  const thinkingCloudRef = useRef<PetThinkingCloudRuntime | null>(null);
  const desktopUiThinkingRef = useRef<Map<string, boolean>>(new Map());
  const streamedAssistantMessageIdsRef = useRef<Set<string>>(new Set());
  const petVoiceReplyEnabledRef = useRef(false);
  const petVoiceReplyConfigRef = useRef<NonNullable<Persona["voiceReply"]>>(defaultPetVoiceReplyConfig());
  const globalLookTimerRef = useRef<number | null>(null);
  const globalLookInFlightRef = useRef(false);
  const lastLookMoveAtRef = useRef(Date.now());
  const lastLookPointRef = useRef<{ x: number; y: number } | null>(null);
  // Keep the latest rendered assistant watermark per conversation so the
  // poll fallback cannot overwrite a newer event from another context.
  const assistantMirrorStateRef = useRef<Map<string, PetAssistantMirrorState>>(new Map());
  const pokeCountRef = useRef(0);
  const lastPokeAtRef = useRef(0);
  const initialGreetingShownRef = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const hideTimeoutRef = useRef<number | null>(null);
  const lastNativeDropRef = useRef<{ signature: string; at: number } | null>(null);
  const assistantCloudDurationMsRef = useRef(DEFAULT_PET_ASSISTANT_CLOUD_DURATION_SECONDS * 1000);
  const isNearModelRef = useRef(false);
  const modelMenuOpenRef = useRef(false);
  const showInputRef = useRef(true);
  const inputShellHoverRef = useRef(false);
  const inputDraftActiveRef = useRef(false);
  const [brokenCloudImages, setBrokenCloudImages] = useState<Record<string, true>>({});
  const [emojiGroups, setEmojiGroups] = useState<EmojiGroup[]>([]);
  const [visionEnabled, setVisionEnabled] = useState(false);
  const [visionIntervalSeconds, setVisionIntervalSeconds] = useState(readStoredPetVisionIntervalSeconds);
  const [petVoiceReplyEnabled, setPetVoiceReplyEnabled] = useState(readStoredPetVoiceReplyEnabled);
  const [petVoiceReplySaving, setPetVoiceReplySaving] = useState(false);
  const [petVoicePlaybackActive, setPetVoicePlaybackActive] = useState(false);
  const [petVoicePersonaName, setPetVoicePersonaName] = useState("");
  const [petVoiceReplyConfig, setPetVoiceReplyConfig] = useState<NonNullable<Persona["voiceReply"]>>(defaultPetVoiceReplyConfig());
  const visionIntervalMs = clampPetVisionIntervalSeconds(visionIntervalSeconds) * 1000;
  const visionTickInFlightRef = useRef(false);
  const visionLastStartedAtRef = useRef(0);

  useEffect(() => {
    petVoiceReplyEnabledRef.current = petVoiceReplyEnabled;
    petVoiceReplyConfigRef.current = petVoiceReplyConfig;
  }, [petVoiceReplyConfig, petVoiceReplyEnabled]);

  useEffect(() => {
    void invoke("set_pet_vision_active", { active: visionEnabled }).catch((error) => {
      console.warn("pet vision active state sync failed:", error);
    });
    if (!visionEnabled) return;
    let intervalId: number | null = null;
    let stopped = false;

    async function tick() {
      const now = Date.now();
      if (visionTickInFlightRef.current || now - visionLastStartedAtRef.current < visionIntervalMs - 500) {
        return;
      }
      visionTickInFlightRef.current = true;
      visionLastStartedAtRef.current = now;
      try {
        const context = await resolvePetSendContext();
        if (stopped || !context.conversationId || !context.personaId) return;
        if (await petVisionShouldSkipTurn(context.conversationId)) return;

        const dataUrl = await invoke<string>("capture_screen_base64");
        if (stopped) return;
        const { mimeType, bytes } = decodePetVisionDataUrl(dataUrl);
        const saved = await api.uploadChatAttachment(
          petVisionFileName(),
          mimeType,
          Array.from(bytes)
        );
        if (stopped) return;
        const attachment = {
          fileName: saved.fileName,
          path: saved.path,
          mimeType: saved.mimeType
        };
        const previousAssistantState = assistantMirrorState(context.conversationId);
        const messages = await api.sendChatMessage({
          conversationId: context.conversationId,
          personaId: context.personaId,
          agentId: context.agentId,
          content: buildPetVisionContent(attachment),
          providerData: {
            source: "pet-vision",
            attachments: [attachment],
            silent: true
          }
        });
        if (stopped || !messages.length) return;
        const assistant = latestAssistantMessage(messages)
          ?? await waitForAssistantReply(context.conversationId, previousAssistantState);
        if (!stopped && assistant) {
          if (streamedAssistantMessageIdsRef.current.has(assistant.id)) {
            finalizeAssistantStream(context.conversationId, assistant);
          } else {
            showAssistantCloud(assistant, context.conversationId);
          }
        }
      } catch (err) {
        console.error("vision error:", err);
        if (!stopped) showCloud("视觉感知暂时看不到屏幕。", "error", 3200);
      } finally {
        visionTickInFlightRef.current = false;
      }
    }

    void tick();
    intervalId = window.setInterval(tick, visionIntervalMs);
    return () => {
      stopped = true;
      if (intervalId !== null) window.clearInterval(intervalId);
      void invoke("set_pet_vision_active", { active: false }).catch((error) => {
        console.warn("pet vision active state cleanup failed:", error);
      });
    };
  }, [visionEnabled, visionIntervalMs]);

  useEffect(() => {
    const clamped = clampPetVisionIntervalSeconds(visionIntervalSeconds);
    try {
      window.localStorage.setItem(PET_VISION_INTERVAL_STORAGE_KEY, String(clamped));
    } catch {
      // ignore preference persistence failures
    }
  }, [visionIntervalSeconds]);


  const [input, setInput] = useState("");
  const [activeContext, setActiveContext] = useState<PetActiveContext | null>(activeContextRef.current);
  const [selectedModel, setSelectedModel] = useState<PetModel>(selectedModelRef.current);
  const [modelLoaded, setModelLoaded] = useState(false);
  const [petAvatarRevealed, setPetAvatarRevealed] = useState(false);
  const [petStartupVisible, setPetStartupVisible] = useState(true);
  const [petStartupExiting, setPetStartupExiting] = useState(false);
  const [sending, setSending] = useState(false);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [cloudBubble, setCloudBubble] = useState<PetCloudBubble | null>(null);
  const [showInput, setShowInput] = useState(true);
  const [petWindowMode, setPetWindowMode] = useState<PetWindowMode>("model");
  const [dockEdge, setDockEdge] = useState<PetDockEdge>("right");
  const [composerAttachments, setComposerAttachments] = useState<PetComposerAttachment[]>([]);
  const [inputDragActive, setInputDragActive] = useState(false);

  useEffect(() => {
    inputDraftActiveRef.current = input.trim().length > 0 || composerAttachments.length > 0 || sending;
  }, [composerAttachments.length, input, sending]);

  const renderedCloudAttachments = useMemo(() => {
    const attachments = cloudBubble?.attachments ?? [];
    if (attachments.length === 0) return [] as PetAttachmentRender[];
    const emojiPathIndex = new Map<string, string>();
    const emojiFileFallbackIndex = new Map<string, string>();
    for (const group of emojiGroups) {
      const imagePaths = Object.values(group.emotionImages ?? {}).flat();
      for (const imagePath of imagePaths) {
        emojiPathIndex.set(normalizeEmojiPathKey(imagePath), imagePath);
        const normalized = normalizeEmojiPathKey(imagePath);
        const markerIndex = normalized.indexOf("\\emoji\\");
        if (markerIndex < 0) continue;
        const segments = normalized.slice(markerIndex + "\\emoji\\".length).split("\\").filter(Boolean);
        if (segments.length < 3) continue;
        const [groupId, emotionId, fileName] = segments;
        emojiFileFallbackIndex.set(`${groupId}::${emotionId}::${fileName}`, imagePath);
      }
    }
    return attachments.map((attachment) => {
      const isEmojiAsset = isEmojiAssetPath(attachment.path);
      const resolvedPath = tryRepairEmojiAttachmentPath(attachment.path, emojiPathIndex, emojiFileFallbackIndex);
      const resolvedKnown = emojiPathIndex.has(normalizeEmojiPathKey(resolvedPath));
      const imageFailed = Boolean(brokenCloudImages[resolvedPath]);
      return {
        ...attachment,
        resolvedPath,
        isEmojiAsset,
        imageFailed,
        hidden: isEmojiAsset && (!resolvedKnown || imageFailed)
      };
    });
  }, [brokenCloudImages, cloudBubble?.attachments, emojiGroups]);

  const queuePetStartupTimer = useCallback((handler: () => void, delayMs: number) => {
    const timer = window.setTimeout(() => {
      petStartupTimersRef.current = petStartupTimersRef.current.filter((item) => item !== timer);
      handler();
    }, delayMs);
    petStartupTimersRef.current.push(timer);
    return timer;
  }, []);

  const clearPetStartupTimers = useCallback(() => {
    for (const timer of petStartupTimersRef.current) {
      window.clearTimeout(timer);
    }
    petStartupTimersRef.current = [];
  }, []);

  const finishPetStartup = useCallback((delayMs = 0) => {
    if (petStartupClosingRef.current) return;
    petStartupClosingRef.current = true;
    queuePetStartupTimer(() => {
      setPetStartupExiting(true);
      queuePetStartupTimer(() => {
        setPetStartupVisible(false);
        queuePetStartupTimer(() => setPetAvatarRevealed(true), PET_STARTUP_REVEAL_AFTER_EXIT_MS);
      }, PET_STARTUP_EXIT_MS);
    }, delayMs);
  }, [queuePetStartupTimer]);

  useEffect(() => {
    document.body.classList.add("pet-window-body");
    document.documentElement.classList.add("pet-window-html");
    void setPetWindowModeState("model");
    return () => {
      document.body.classList.remove("pet-window-body");
      document.documentElement.classList.remove("pet-window-html");
      clearPetStartupTimers();
      clearCloudTimer();
      clearCloudStreamTimer();
      clearGlobalLookTimer();
      void syncPetPointerPassthrough(false);
      stopModelDrag();
    };
  }, [clearPetStartupTimers]);

  useEffect(() => {
    queuePetStartupTimer(() => {
      finishPetStartup();
    }, PET_STARTUP_MAX_VISIBLE_MS);
  }, [finishPetStartup, queuePetStartupTimer]);

  useEffect(() => {
    if (!modelLoaded || !petStartupVisible) return;
    const elapsedMs = Date.now() - petStartupStartedAtRef.current;
    const finishDelayMs = Math.max(0, PET_STARTUP_MIN_VISIBLE_MS - elapsedMs);
    finishPetStartup(finishDelayMs);
  }, [finishPetStartup, modelLoaded, petStartupVisible]);

  useEffect(() => {
    activeContextRef.current = activeContext;
  }, [activeContext]);

  useEffect(() => {
    const conversationId = activeContext?.conversationId;
    if (!conversationId) return;
    void refreshLatestAssistant(conversationId, false);
  }, [activeContext?.conversationId]);

  useEffect(() => {
    void refreshPetVoiceReplyState();
  }, [activeContext?.personaId, activeContext?.conversationId]);

  useEffect(() => {
    if (modelMenuOpen) void refreshPetVoiceReplyState();
  }, [modelMenuOpen]);

  useEffect(() => () => stopPetVoicePlayback(), []);

  useEffect(() => {
    selectedModelRef.current = selectedModel;
  }, [selectedModel]);

  useEffect(() => {
    modelLoadedRef.current = modelLoaded;
  }, [modelLoaded]);

  useEffect(() => {
    sendingRef.current = sending;
  }, [sending]);

  useEffect(() => {
    showInputRef.current = showInput;
  }, [showInput]);

  useEffect(() => {
    if (petWindowMode !== "model") {
      setInputDragActive(false);
      return;
    }
    const handleDrag = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      event.preventDefault();
      event.stopPropagation();
      if (event.dataTransfer) event.dataTransfer.dropEffect = "copy";
      if (isPointInsidePetInput(event.clientX, event.clientY)) {
        revealInput();
        setInputDragActive(true);
      } else {
        setInputDragActive(false);
      }
    };
    const handleDragLeave = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      const nextTarget = event.relatedTarget as Node | null;
      if (nextTarget && inputShellRef.current?.contains(nextTarget)) return;
      setInputDragActive(false);
    };
    const handleDrop = (event: DragEvent) => {
      if (!hasFileDragData(event.dataTransfer)) return;
      event.preventDefault();
      event.stopPropagation();
      setInputDragActive(false);
      if (!isPointInsidePetInput(event.clientX, event.clientY)) return;
      if (event.dataTransfer && event.dataTransfer.files.length > 0 && rememberPetDomDrop(event.dataTransfer.files)) {
        void stagePetFiles(event.dataTransfer.files);
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
  }, [petWindowMode]);

  useEffect(() => {
    if (!isTauri() || petWindowMode !== "model") {
      setInputDragActive(false);
      return;
    }
    const unlisteners: Array<() => void> = [];
    let cancelled = false;
    const attach = (source: string, registration: Promise<() => void>) => {
      void registration.then((unlisten) => {
        if (cancelled) {
          unlisten();
        } else {
          unlisteners.push(unlisten);
        }
      }).catch((error) => {
        console.warn(`${source} pet file drop listener unavailable:`, error);
      });
    };
    attach("pet webview native", getCurrentWebview().onDragDropEvent((event) => {
      handleNativePetFileDrop(event.payload as NativeFileDropPayload);
    }));
    attach("pet window native", getCurrentWindow().onDragDropEvent((event) => {
      handleNativePetFileDrop(event.payload as NativeFileDropPayload);
    }));
    attach("pet forwarded", listen<NativeFileDropPayload>("synthchat-file-drop-event", (event) => {
      if (event.payload.windowLabel && event.payload.windowLabel !== "pet") return;
      handleNativePetFileDrop(event.payload);
    }));
    return () => {
      cancelled = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [petWindowMode]);

  useEffect(() => {
    let cancelled = false;
    const loadEmojiGroups = async () => {
      try {
        const groups = await api.listEmojiGroups();
        if (!cancelled) setEmojiGroups(groups);
      } catch (error) {
        if (!cancelled) {
          console.warn("pet emoji group load failed:", error);
          setEmojiGroups([]);
        }
      }
    };
    void loadEmojiGroups();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    modelMenuOpenRef.current = modelMenuOpen;
  }, [modelMenuOpen]);

  useEffect(() => {
    let cancelled = false;
    const syncAssistantCloudDuration = async () => {
      try {
        const config = await api.getConfig();
        if (cancelled) return;
        assistantCloudDurationMsRef.current = clampPetCloudDurationSeconds(config.chat.petCloudDurationSeconds) * 1000;
      } catch {
        if (!cancelled) {
          assistantCloudDurationMsRef.current = DEFAULT_PET_ASSISTANT_CLOUD_DURATION_SECONDS * 1000;
        }
      }
    };

    void syncAssistantCloudDuration();
    const timer = window.setInterval(() => {
      void syncAssistantCloudDuration();
    }, 5000);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as HTMLElement | null;
      if (!target?.closest(".pet-input-shell")) {
        modelMenuOpenRef.current = false;
        setModelMenuOpen(false);
      }
    };
    window.addEventListener("pointerdown", onPointerDown);
    return () => window.removeEventListener("pointerdown", onPointerDown);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<PetActiveContext>(PET_ACTIVE_CONTEXT_EVENT, (event) => {
      const context = parsePetActiveContext(event.payload);
      if (!context) return;
      setPetContext(context);
    }).then((handler) => {
      unlisten = handler;
    });

    const onStorage = (event: StorageEvent) => {
      if (event.key !== PET_ACTIVE_CONTEXT_STORAGE_KEY || !event.newValue) return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.newValue);
      } catch {
        return;
      }
      const context = parsePetActiveContext(parsed);
      if (!context) return;
      setPetContext(context, false);
    };
    window.addEventListener("storage", onStorage);
    return () => {
      if (unlisten) unlisten();
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  useEffect(() => {
    const applyThinkingState = (value: unknown, persistContext = true) => {
      const state = parsePetThinkingState(value);
      if (!state?.conversationId) return;
      if (!petShouldShowThinkingCloud(state.source)) return;
      if (state.source === "desktop-ui") {
        desktopUiThinkingRef.current.set(state.conversationId, state.thinking);
      } else if (state.thinking && state.source === "desktop") {
        desktopUiThinkingRef.current.set(state.conversationId, true);
      }
      if (state.thinking) {
        const current = activeContextRef.current ?? readStoredPetActiveContext();
        if (current?.conversationId !== state.conversationId) {
          setPetContext({
            conversationId: state.conversationId,
            conversationTitle: null,
            personaId: state.personaId,
            personaName: null,
            agentId: null,
            updatedAt: state.updatedAt,
            source: state.source || "desktop"
          }, persistContext);
        }
        showThinkingCloud(state.conversationId);
        return;
      }
      const currentThinking = thinkingCloudRef.current;
      const keptBubbleId = currentThinking?.conversationId === state.conversationId
        ? currentThinking.bubbleId
        : null;
      const keptThinkingBubble = clearThinkingCloud(state.conversationId, { clearBubble: false });
      void refreshLatestAssistant(state.conversationId, true).finally(() => {
        if (!keptThinkingBubble || !keptBubbleId) return;
        cloudTextDraftsRef.current.delete(keptBubbleId);
        setCloudBubble((bubble) => (bubble?.id === keptBubbleId ? null : bubble));
      });
    };

    const stored = readStoredPetThinkingState();
    if (stored?.thinking) applyThinkingState(stored, false);
    const unsubscribeSharedState = subscribePetThinkingState((state) => {
      applyThinkingState(state);
    });

    let unlisten: (() => void) | null = null;
    void listen(PET_THINKING_STATE_EVENT, (event) => {
      applyThinkingState(event.payload);
    }).then((handler) => {
      unlisten = handler;
    });

    const onStorage = (event: StorageEvent) => {
      if (event.key !== PET_THINKING_STATE_STORAGE_KEY || !event.newValue) return;
      let parsed: unknown;
      try {
        parsed = JSON.parse(event.newValue);
      } catch {
        return;
      }
      applyThinkingState(parsed, false);
    };
    window.addEventListener("storage", onStorage);
    let lastStoredSignature = "";
    const pollStoredThinkingState = () => {
      const state = readStoredPetThinkingState();
      if (!state) return;
      const signature = `${state.conversationId ?? ""}:${state.thinking ? "1" : "0"}:${state.updatedAt}`;
      if (signature === lastStoredSignature) return;
      lastStoredSignature = signature;
      applyThinkingState(state, false);
    };
    const pollTimer = window.setInterval(pollStoredThinkingState, 700);
    return () => {
      unsubscribeSharedState();
      if (unlisten) unlisten();
      window.removeEventListener("storage", onStorage);
      window.clearInterval(pollTimer);
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type?: string;
      personaId?: string;
      source?: string;
      persona?: Persona;
    }>("synthchat-persona-event", (event) => {
      const payload = event.payload;
      if (payload.type !== "persona_updated") return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      const updatedPersonaId = payload.persona?.id ?? payload.personaId ?? null;
      const matchesActivePersona =
        !updatedPersonaId
        || !context?.personaId
        || updatedPersonaId === context.personaId;
      if (payload.source === "desktop-local" && payload.persona && applyPetVoiceReplyPersona(payload.persona)) return;
      if (matchesActivePersona) {
        void refreshPetVoiceReplyState();
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    const refreshMirror = async () => {
      const conversationId = activeContextRef.current?.conversationId;
      if (!conversationId) return;
      await refreshLatestAssistant(conversationId, true);
    };
    void refreshMirror();
    const timer = window.setInterval(refreshMirror, PET_MESSAGE_MIRROR_INTERVAL_MS);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      conversationId?: string;
      message?: ChatMessage;
      source?: string;
      personaId?: string;
      delta?: string;
      isLast?: boolean;
      ok?: boolean;
    }>("synthchat-pet-event", (event) => {
      const payload = event.payload;
      if (
        payload.type !== "thinking_started"
        && payload.type !== "thinking_finished"
        && payload.type !== "assistant_stream_delta"
        && payload.type !== "assistant_stream_done"
        && payload.type !== "assistant_final"
        && payload.type !== "proactive_message"
      ) return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      const hasContext = Boolean(context?.conversationId);
      const isCurrentConversation = context?.conversationId === payload.conversationId;
      const isWechat = payload.message?.source === "wechat" || (payload as { source?: string }).source === "wechat";
      if (!payload.conversationId) {
        if (payload.message && assistantMessageVisibleInCloud(payload.message)) showAssistantCloud(payload.message);
        return;
      }
      const eventSource = payload.source ?? payload.message?.source ?? "";
      const isThinkingStart = payload.type === "thinking_started";
      const shouldShowThinking =
        isThinkingStart
        && petShouldFollowThinkingTurn(eventSource)
        && petShouldShowThinkingCloud(eventSource);
      const shouldAdoptConversation = !isCurrentConversation && (isWechat || shouldShowThinking || !hasContext);
      if (shouldAdoptConversation) {
        setPetContext({
          conversationId: payload.conversationId,
          conversationTitle: null,
          personaId: payload.personaId ?? null,
          personaName: null,
          agentId: null,
          updatedAt: new Date().toISOString(),
          source: isWechat ? "wechat" : (eventSource || "desktop")
        });
      }
      if (payload.type === "thinking_started") {
        if (shouldShowThinking) {
          showThinkingCloud(payload.conversationId);
        }
        return;
      }
      if (!isCurrentConversation && !shouldAdoptConversation) return;
      if (payload.type === "thinking_finished") {
        if (desktopUiThinkingRef.current.get(payload.conversationId) === true) {
          return;
        }
        if (payload.message && assistantMessageVisibleInCloud(payload.message)) {
          showAssistantCloud(payload.message, payload.conversationId);
          return;
        }
        clearThinkingCloud(payload.conversationId);
        if (payload.ok === false) {
          showCloud("思考中断了。", "error", 3200);
          playPetBehavior("error");
        }
        return;
      }
      if (payload.type === "assistant_stream_delta" && payload.message) {
        appendAssistantStreamDelta(payload.conversationId, payload.message, payload.delta ?? "");
        return;
      }
      if (payload.type === "assistant_stream_done" && payload.message) {
        const runtime = assistantStreamRef.current;
        if (
          runtime
          && runtime.messageId === payload.message.id
          && runtime.conversationId === payload.conversationId
        ) {
          finalizeAssistantStream(payload.conversationId, payload.message);
        } else {
          showAssistantCloud(payload.message, payload.conversationId);
        }
        return;
      }
      if (!payload.message) return;
      if (
        payload.type === "assistant_final"
        && payload.message.id
        && streamedAssistantMessageIdsRef.current.has(payload.message.id)
      ) {
        finalizeAssistantStream(payload.conversationId, payload.message);
        return;
      }
      if (assistantMessageVisibleInCloud(payload.message)) {
        showAssistantCloud(payload.message, payload.conversationId);
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      source?: string;
      personaId?: string;
      conversationId?: string;
      message?: ChatMessage;
      ok?: boolean;
    }>("synthchat-chat-event", (event) => {
      // The chat stream keeps the pet's target/context in sync and owns the
      // pre-reply thinking cloud. Reply text still flows through pet events.
      const payload = event.payload;
      const relevantTypes = ["turn_started", "turn_finished", "processing", "new_message", "assistant_message", "conversation_updated"];
      if (!relevantTypes.includes(payload.type) || !payload.conversationId) return;

      const context = activeContextRef.current ?? readStoredPetActiveContext();
      const isCurrentConversation = context?.conversationId === payload.conversationId;
      const eventSource = payload.source ?? payload.message?.source ?? "";
      const hasContext = Boolean(context?.conversationId);
      // Follow rules:
      // - WeChat-originated messages always follow (locked or not).
      // - Desktop-originated thinking turns follow before the first reply delta
      //   so the pet can show the same pre-reply state as the main chat.
      // - When the pet has no locked context yet, follow the desktop-active
      //   conversation so the input target stays intuitive.
      const isThinkingStart = payload.type === "turn_started" || payload.type === "processing";
      const shouldShowThinking =
        isThinkingStart
        && petShouldFollowThinkingTurn(eventSource)
        && petShouldShowThinkingCloud(eventSource);
      const shouldFollowIncomingWechat = eventSource === "wechat" && (!hasContext || !isCurrentConversation);
      const shouldFollowWhenUnbound = !hasContext;
      const shouldFollow = shouldShowThinking || shouldFollowIncomingWechat || shouldFollowWhenUnbound;

      if (shouldFollow && !isCurrentConversation) {
        const nextContext: PetActiveContext = {
          conversationId: payload.conversationId,
          conversationTitle: null,
          personaId: payload.personaId ?? null,
          personaName: null,
          agentId: null,
          updatedAt: new Date().toISOString(),
          source: shouldFollowIncomingWechat ? "wechat" : (eventSource || "desktop")
        };
        setPetContext(nextContext);
      }
      const currentAfterFollow = activeContextRef.current ?? readStoredPetActiveContext();
      const isActiveConversation = currentAfterFollow?.conversationId === payload.conversationId;
      if (payload.type === "turn_started" || payload.type === "processing") {
        const canShowActiveThinking =
          isActiveConversation
          && petShouldFollowThinkingTurn(eventSource)
          && petShouldShowThinkingCloud(eventSource);
        if (shouldShowThinking || canShowActiveThinking) {
          showThinkingCloud(payload.conversationId);
        }
        return;
      }
      if (!isActiveConversation) return;
      if (payload.type === "turn_finished") {
        if (desktopUiThinkingRef.current.get(payload.conversationId) === true) {
          return;
        }
        if (payload.message && assistantMessageVisibleInCloud(payload.message)) {
          showAssistantCloud(payload.message, payload.conversationId);
          return;
        }
        clearThinkingCloud(payload.conversationId);
        if (payload.ok === false) {
          showCloud("思考中断了。", "error", 3200);
          playPetBehavior("error");
        }
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<AgentRunEvent>("synthchat-agent-run-event", (event) => {
      const payload = event.payload;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      if (
        context?.conversationId
        && context.conversationId === payload.conversationId
        && (payload.state === "failed" || payload.state === "aborted")
      ) {
        showCloud("任务没有完成。", "error", 3200);
        playPetBehavior("error");
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (!modelLoadedRef.current) return;
      if (petWindowModeRef.current === "orb") {
        void syncPetPointerPassthrough(false);
        return;
      }
      void invoke<PetCursorPosition>("cursor_position").then((position) => {
        const point = normalizeCursorPosition(position);
        if (!point) return;
        const { clientX, clientY } = point;
        const overModel = pointNearModel(clientX, clientY);
        const onModelSurface = pointInsideModelSurface(clientX, clientY);
        const inPetUi = isPointerInPetUi(clientX, clientY);
        const inInputHotZone = isPointInsidePetInputHotZone(clientX, clientY);
        const inputFocused = document.activeElement === inputRef.current;
        const focusShouldKeepInputVisible = inputFocused && inputDraftActiveRef.current;
        const pointerInInputShell = onModelSurface || inInputHotZone || inPetUi || modelMenuOpenRef.current;
        if (!pointerInInputShell && inputShellHoverRef.current) {
          inputShellHoverRef.current = false;
        }
        const keepWindowInteractive = inputFocused || overModel || inPetUi || inInputHotZone || modelMenuOpenRef.current;
        const keepInputVisible = onModelSurface || focusShouldKeepInputVisible || inputShellHoverRef.current || inPetUi || modelMenuOpenRef.current;

        void syncPetPointerPassthrough(!keepWindowInteractive);

        if (keepInputVisible) {
          clearInputHideTimer();
          if (!isNearModelRef.current || !showInputRef.current) {
            isNearModelRef.current = true;
            showInputRef.current = true;
            setShowInput(true);
          }
        } else {
          isNearModelRef.current = false;
          queueInputHide();
        }
      }).catch(() => {
        void syncPetPointerPassthrough(false);
      });
    }, 150);
    return () => {
      window.clearInterval(timer);
      if (hideTimeoutRef.current !== null) {
        window.clearTimeout(hideTimeoutRef.current);
      }
    };
  }, []);

  useEffect(() => {
    clearGlobalLookTimer();
    if (!modelLoaded) return;
    lastLookPointRef.current = null;
    lastLookMoveAtRef.current = Date.now();
    globalLookTimerRef.current = window.setInterval(() => {
      void updateGlobalLook();
    }, PET_GLOBAL_LOOK_INTERVAL_MS);
    return clearGlobalLookTimer;
  }, [modelLoaded]);

  useEffect(() => {
    const onMessage = (event: MessageEvent<PetMessage>) => {
      const message = event.data;
      if (!message || typeof message !== "object" || message.source !== FRAME_MESSAGE_SOURCE) return;
      if (message.type === "ready") {
        frameReadyRef.current = true;
        flushPendingModelLoad();
        loadModel(selectedModelRef.current);
        return;
      }
      if (message.type === "loaded") {
        setModelLoaded(true);
        if (!initialGreetingShownRef.current) {
          initialGreetingShownRef.current = true;
          const elapsedMs = Date.now() - petStartupStartedAtRef.current;
          const exitDelayMs = Math.max(0, PET_STARTUP_MIN_VISIBLE_MS - elapsedMs);
          const greetingDelayMs = exitDelayMs + PET_STARTUP_EXIT_MS + PET_STARTUP_REVEAL_AFTER_EXIT_MS + 1700;
          window.setTimeout(() => showCloud(selectedModel.greeting, "happy", 2600), greetingDelayMs);
        }
        return;
      }
      if (message.type === "model_capabilities") {
        console.info("[SynthChat Pet] Live2D capabilities", message);
        return;
      }
      if (message.type === "behavior_debug" || message.type === "motion_debug") {
        console.info("[SynthChat Pet] Live2D behavior", message);
        return;
      }
      if (message.type === "model_hover" || message.type === "model_bounds") {
        if (
          typeof message.x === "number"
          && typeof message.y === "number"
          && typeof message.width === "number"
          && typeof message.height === "number"
        ) {
          modelBoundsRef.current = {
            x: message.x,
            y: message.y,
            width: message.width,
            height: message.height
          };
        }
        if (message.type === "model_hover" && message.hovering) {
          revealPetInputShell();
        }
        return;
      }
      if (message.type === "model_drag_start") {
        if (petWindowModeRef.current === "orb") return;
        showCloud("带我换个位置吗？我会跟上。", "active", 2200);
        void startModelDrag(message.screenX, message.screenY);
        return;
      }
      if (message.type === "model_drag_move") {
        if (petWindowModeRef.current === "orb") return;
        void moveModelDrag(message.screenX, message.screenY);
        return;
      }
      if (message.type === "model_drag_end") {
        void finishModelDrag(message.screenX, message.screenY);
        return;
      }
      if (message.type === "toggle_main_window") {
        void toggleMainWindow();
        return;
      }
      if (message.type === "tap") {
        const now = Date.now();
        pokeCountRef.current = now - lastPokeAtRef.current < 2500 ? pokeCountRef.current + 1 : 1;
        lastPokeAtRef.current = now;
        showCloud(touchAreaCloudText(message.area, pokeCountRef.current), "soft", 2600);
        inputRef.current?.focus();
        return;
      }
      if (message.type === "poke") {
        showCloud(touchAreaCloudText(message.area ?? message.areas?.[0], 1), "active", 3000);
        inputRef.current?.focus();
        return;
      }
      if (message.type === "error") {
        showCloud(message.message ?? "模型加载失败。", "error", 3600);
        playPetBehavior("error");
      }
    };
    window.addEventListener("message", onMessage as EventListener);
    return () => window.removeEventListener("message", onMessage as EventListener);
  }, [selectedModel.greeting, selectedModel.path]);

  function postToPet(message: unknown) {
    const target = frameRef.current?.contentWindow;
    if (!target) return false;
    target.postMessage(
      { source: HOST_MESSAGE_SOURCE, ...(message as Record<string, unknown>) },
      "*"
    );
    return true;
  }

  function playPetBehavior(name: PetBehaviorName, options?: Record<string, unknown>) {
    if (!modelLoadedRef.current) return false;
    return postToPet({ type: "behavior", name, options });
  }

  function behaviorForAssistantText(text: string): PetBehaviorName {
    const value = text.trim();
    const lower = value.toLowerCase();
    if (/失败|错误|抱歉|不能|无法|异常|报错|bad request|error|failed|sorry/.test(lower)) return "error";
    if (/真的吗|为什么|怎么|如何|是否|\?|？/.test(value)) return "curious";
    if (/完成|成功|好了|可以了|没问题|太好了|nice|done|success/.test(lower)) return "proud";
    if (/谢谢|感谢|喜欢|开心|哈哈|嘿嘿|嘻嘻|thank/.test(lower)) return "happy";
    if (/你好|早上好|晚上好|hello|hi\b/.test(lower)) return "wave";
    if (value.length > 260) return "stretch";
    if (value.length < 18) return "shy";
    return Math.random() < 0.18 ? "curious" : "happy";
  }

  function flushPendingModelLoad() {
    if (!frameReadyRef.current || !pendingModelLoadRef.current) return;
    const pending = pendingModelLoadRef.current;
    pendingModelLoadRef.current = null;
    postToPet({ type: "load", url: pending.model.path, force: pending.force });
  }

  function loadModel(model = selectedModelRef.current, force = false) {
    pendingModelLoadRef.current = { model, force };
    flushPendingModelLoad();
  }

  function clearCloudTimer() {
    if (cloudTimerRef.current !== null) {
      window.clearTimeout(cloudTimerRef.current);
      cloudTimerRef.current = null;
    }
  }

  function clearCloudStreamTimer() {
    if (cloudStreamTimerRef.current !== null) {
      window.clearInterval(cloudStreamTimerRef.current);
      cloudStreamTimerRef.current = null;
    }
  }

  function clearGlobalLookTimer() {
    if (globalLookTimerRef.current !== null) {
      window.clearInterval(globalLookTimerRef.current);
      globalLookTimerRef.current = null;
    }
    globalLookInFlightRef.current = false;
  }

  function clearInputHideTimer() {
    if (hideTimeoutRef.current !== null) {
      window.clearTimeout(hideTimeoutRef.current);
      hideTimeoutRef.current = null;
    }
  }

  function revealInput() {
    clearInputHideTimer();
    isNearModelRef.current = true;
    showInputRef.current = true;
    setShowInput(true);
  }

  function scheduleInputHide() {
    inputShellHoverRef.current = false;
    isNearModelRef.current = false;
    clearInputHideTimer();
    queueInputHide();
  }

  function queueInputHide() {
    if (modelMenuOpenRef.current || !showInputRef.current || hideTimeoutRef.current !== null) return;
    hideTimeoutRef.current = window.setTimeout(() => {
      const focusedWithDraft = document.activeElement === inputRef.current && inputDraftActiveRef.current;
      if (!modelMenuOpenRef.current && !focusedWithDraft) {
        inputRef.current?.blur();
        showInputRef.current = false;
        setShowInput(false);
      }
      hideTimeoutRef.current = null;
    }, 800);
  }

  function revealPetInputSurface() {
    revealInput();
    void syncPetPointerPassthrough(false);
  }

  function revealPetInputShell() {
    inputShellHoverRef.current = true;
    revealPetInputSurface();
  }

  function activatePetInputHotZone(event: ReactPointerEvent<HTMLDivElement>) {
    if (event.button !== 0) return;
    event.preventDefault();
    revealPetInputShell();
    window.requestAnimationFrame(() => inputRef.current?.focus());
  }

  function isPointInsidePetInputHotZone(clientX: number, clientY: number) {
    const rect = inputShellRef.current?.getBoundingClientRect();
    if (!rect || rect.width <= 0 || rect.height <= 0) {
      return clientX >= PET_INPUT_HOT_ZONE_X_PADDING_PX
        && clientX <= window.innerWidth - PET_INPUT_HOT_ZONE_X_PADDING_PX
        && clientY >= window.innerHeight - PET_INPUT_FALLBACK_HOT_ZONE_HEIGHT_PX
        && clientY <= window.innerHeight;
    }
    const top = showInputRef.current
      ? rect.top - PET_INPUT_HOT_ZONE_TOP_PADDING_PX
      : Math.min(rect.top - PET_INPUT_HOT_ZONE_TOP_PADDING_PX, window.innerHeight - PET_INPUT_FALLBACK_HOT_ZONE_HEIGHT_PX);
    return clientX >= rect.left - PET_INPUT_HOT_ZONE_X_PADDING_PX
      && clientX <= rect.right + PET_INPUT_HOT_ZONE_X_PADDING_PX
      && clientY >= top
      && clientY <= rect.bottom + PET_INPUT_HOT_ZONE_BOTTOM_PADDING_PX;
  }

  function isPointInsidePetInput(clientX: number, clientY: number) {
    const rect = inputShellRef.current?.getBoundingClientRect();
    if (!rect) return true;
    return isPointInsidePetInputHotZone(clientX, clientY);
  }

  function isNativePetDropInsideInput(position: NativeFileDropPayload["position"]) {
    if (!position) return true;
    const pixelRatio = window.devicePixelRatio || 1;
    return isPointInsidePetInput(position.x / pixelRatio, position.y / pixelRatio);
  }

  function rememberPetFileDropSignature(signature: string, windowMs = 1000) {
    const now = Date.now();
    const previous = lastNativeDropRef.current;
    if (previous?.signature === signature && now - previous.at < windowMs) return false;
    lastNativeDropRef.current = { signature, at: now };
    return true;
  }

  function rememberPetPathDrop(paths: string[]) {
    return rememberPetFileDropSignature(`paths:${paths.slice().sort().join("\n")}`);
  }

  function rememberPetDomDrop(files: FileList) {
    const signature = Array.from(files)
      .map((file) => `${file.name}:${file.size}:${file.lastModified}`)
      .sort()
      .join("\n");
    return rememberPetFileDropSignature(`files:${signature}`, 500);
  }

  function handleNativePetFileDrop(payload: NativeFileDropPayload) {
    if (petWindowModeRef.current !== "model") {
      setInputDragActive(false);
      return;
    }
    if (payload.type === "leave") {
      setInputDragActive(false);
      return;
    }
    if (!isNativePetDropInsideInput(payload.position)) {
      setInputDragActive(false);
      return;
    }
    if (payload.type === "enter" || payload.type === "over") {
      revealInput();
      setInputDragActive(true);
      return;
    }
    setInputDragActive(false);
    const paths = (payload.paths ?? []).map((path) => path.trim()).filter(Boolean);
    if (paths.length === 0) return;
    if (!rememberPetPathDrop(paths)) return;
    void stagePetFilePaths(paths);
  }

  function handlePetFileDragEnter(event: ReactDragEvent<HTMLElement>) {
    if (!hasFileDragData(event.dataTransfer)) return;
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
    revealInput();
    setInputDragActive(true);
  }

  function handlePetFileDragOver(event: ReactDragEvent<HTMLElement>) {
    if (!hasFileDragData(event.dataTransfer)) return;
    event.preventDefault();
    event.stopPropagation();
    event.dataTransfer.dropEffect = "copy";
    setInputDragActive(true);
  }

  function handlePetFileDragLeave(event: ReactDragEvent<HTMLElement>) {
    if (!hasFileDragData(event.dataTransfer)) return;
    event.stopPropagation();
    const nextTarget = event.relatedTarget as Node | null;
    if (!nextTarget || !event.currentTarget.contains(nextTarget)) {
      setInputDragActive(false);
    }
  }

  function handlePetFileDrop(event: ReactDragEvent<HTMLElement>) {
    if (!hasFileDragData(event.dataTransfer)) return;
    event.preventDefault();
    event.stopPropagation();
    setInputDragActive(false);
    if (event.dataTransfer.files.length > 0 && rememberPetDomDrop(event.dataTransfer.files)) {
      void stagePetFiles(event.dataTransfer.files);
    }
  }

  function toggleModelMenu() {
    revealInput();
    void syncPetPointerPassthrough(false);
    setModelMenuOpen((open) => {
      const next = !open;
      modelMenuOpenRef.current = next;
      return next;
    });
  }

  function showCloud(text: string, tone: PetCloudBubble["tone"] = "soft", durationMs = 4200, attachments?: PetCloudBubble["attachments"]) {
    const formatted = formatCloudText(text);
    if (!formatted && !attachments?.length) return;
    assistantStreamRef.current = null;
    thinkingCloudRef.current = null;
    clearCloudTimer();
    clearCloudStreamTimer();
    setBrokenCloudImages({});
    const bubbleId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    cloudTextDraftsRef.current.set(bubbleId, formatted || "");
    setCloudBubble({
      id: bubbleId,
      text: formatted || "",
      tone,
      attachments
    });
    cloudTimerRef.current = window.setTimeout(() => {
      setCloudBubble(null);
      cloudTextDraftsRef.current.delete(bubbleId);
      cloudTimerRef.current = null;
    }, durationMs);
  }

  function showThinkingCloud(conversationId: string) {
    if (!conversationId) return;
    const current = thinkingCloudRef.current;
    if (current?.conversationId === conversationId) {
      clearCloudTimer();
      setCloudBubble((bubble) => {
        if (bubble?.id === current.bubbleId) return bubble;
        const bubbleId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
        thinkingCloudRef.current = { conversationId, bubbleId };
        cloudTextDraftsRef.current.set(bubbleId, PET_THINKING_CLOUD_TEXT);
        return {
          id: bubbleId,
          text: PET_THINKING_CLOUD_TEXT,
          tone: "active"
        };
      });
      return;
    }
    stopPetVoicePlayback({ clearCloudStream: true, clearAssistantStream: false });
    clearCloudTimer();
    clearCloudStreamTimer();
    setBrokenCloudImages({});
    const bubbleId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    thinkingCloudRef.current = { conversationId, bubbleId };
    cloudTextDraftsRef.current.set(bubbleId, PET_THINKING_CLOUD_TEXT);
    setCloudBubble({
      id: bubbleId,
      text: PET_THINKING_CLOUD_TEXT,
      tone: "active"
    });
    playPetBehavior("thinking", { durationMs: 1800 });
  }

  function clearThinkingCloud(
    conversationId?: string | null,
    options: { clearBubble?: boolean } = {}
  ) {
    const current = thinkingCloudRef.current;
    if (!current) return false;
    if (conversationId && current.conversationId !== conversationId) return false;
    thinkingCloudRef.current = null;
    cloudTextDraftsRef.current.delete(current.bubbleId);
    if (options.clearBubble === false) return true;
    setCloudBubble((bubble) => (bubble?.id === current.bubbleId ? null : bubble));
    return true;
  }

  function scheduleCloudDismiss(bubbleId: string, durationMs: number) {
    clearCloudTimer();
    cloudTimerRef.current = window.setTimeout(() => {
      setCloudBubble((current) => current?.id === bubbleId ? null : current);
      cloudTextDraftsRef.current.delete(bubbleId);
      cloudTimerRef.current = null;
    }, durationMs);
  }

  function showStreamingCloud(
    text: string,
    tone: PetCloudBubble["tone"] = "soft",
    durationMs = 4200,
    attachments?: PetCloudBubble["attachments"]
  ) {
    const formatted = formatCloudText(text);
    if (!formatted && !attachments?.length) return "";
    thinkingCloudRef.current = null;
    clearCloudTimer();
    clearCloudStreamTimer();
    setBrokenCloudImages({});
    const bubbleId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const initialLength = formatted.length > 0 ? Math.min(2, formatted.length) : 0;
    cloudTextDraftsRef.current.set(bubbleId, formatted);
    setCloudBubble({
      id: bubbleId,
      text: formatted.slice(0, initialLength),
      tone,
      attachments
    });
    if (initialLength >= formatted.length) {
      scheduleCloudDismiss(bubbleId, durationMs);
      return formatted;
    }

    let visibleChars = initialLength;
    cloudStreamTimerRef.current = window.setInterval(() => {
      const step = formatted.length > 140 ? 2 : 1;
      visibleChars = Math.min(formatted.length, visibleChars + step);
      setCloudBubble((current) => (
        current?.id === bubbleId ? { ...current, text: formatted.slice(0, visibleChars) } : current
      ));
      if (visibleChars >= formatted.length) {
        clearCloudStreamTimer();
        scheduleCloudDismiss(bubbleId, durationMs);
      }
    }, PET_CLOUD_STREAM_INTERVAL_MS);
    return formatted;
  }

  function showVoiceSyncedCloud(
    text: string,
    tone: PetCloudBubble["tone"] = "soft",
    attachments?: PetCloudBubble["attachments"]
  ) {
    const formatted = formatCloudText(text);
    if (!formatted && !attachments?.length) return null;
    thinkingCloudRef.current = null;
    clearCloudTimer();
    clearCloudStreamTimer();
    setBrokenCloudImages({});
    setCloudBubble(null);
    const bubbleId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
    return { bubbleId, text: formatted, tone, attachments };
  }

  function appendCloudBubbleText(
    bubbleId: string,
    delta: string,
    tone: PetCloudBubble["tone"] = "active",
    attachments?: PetCloudBubble["attachments"]
  ) {
    const currentDraft = cloudTextDraftsRef.current.get(bubbleId) ?? "";
    const nextDraft = `${currentDraft}${delta}`;
    const nextText = formatCloudText(nextDraft);
    if (!nextText && !attachments?.length) return;
    cloudTextDraftsRef.current.set(bubbleId, nextDraft);
    setCloudBubble((current) => {
      if (current?.id === bubbleId) {
        return {
          ...current,
          text: nextText,
          attachments: attachments?.length ? attachments : current.attachments
        };
      }
      if (current) return current;
      return {
        id: bubbleId,
        text: nextText,
        tone,
        attachments
      };
    });
  }

  function stopPetVoicePlayback(options: { clearCloudStream?: boolean; clearAssistantStream?: boolean } = {}) {
    activeVoiceReplyRequestRef.current = null;
    if (options.clearAssistantStream !== false) assistantStreamRef.current = null;
    if (options.clearCloudStream) clearCloudStreamTimer();
    setPetVoicePlaybackActive(false);
    if (isTauri()) {
      void api.stopChatAudio?.().catch((error: unknown) => {
        console.warn("pet native voice stop failed:", error);
      });
    }
    const audio = voiceAudioRef.current;
    if (audio) {
      audio.pause();
      audio.src = "";
      voiceAudioRef.current = null;
    }
  }

  function revealAssistantStreamText(runtime: PetAssistantStreamRuntime, delta: string) {
    appendCloudBubbleText(runtime.bubbleId, delta, runtime.tone, runtime.attachments);
  }

  function appendAssistantStreamDelta(conversationId: string, message: ChatMessage, delta: string) {
    const visibleDelta = delta;
    if (!visibleDelta) return;
    const replacedThinkingCloud = petVoiceReplyEnabledRef.current
      ? false
      : clearThinkingCloud(conversationId, { clearBubble: false });
    let runtime = assistantStreamRef.current;
    if (!runtime || runtime.messageId !== message.id || runtime.conversationId !== conversationId) {
      stopPetVoicePlayback({ clearCloudStream: true });
      clearCloudTimer();
      clearCloudStreamTimer();
      setBrokenCloudImages({});
      if (!replacedThinkingCloud) setCloudBubble(null);
      const bubbleId = `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const requestKey = `stream:${message.id}`;
      runtime = {
        conversationId,
        messageId: message.id,
        requestKey,
        bubbleId,
        tone: "active",
        text: "",
        finalized: false
      };
      assistantStreamRef.current = runtime;
      activeVoiceReplyRequestRef.current = requestKey;
      streamedAssistantMessageIdsRef.current.add(message.id);
      playPetBehavior("speaking", { durationMs: 1600 });
    }
    runtime.text += visibleDelta;
    if (petVoiceReplyEnabledRef.current) return;
    revealAssistantStreamText(runtime, visibleDelta);
  }

  function finalizeAssistantStream(conversationId: string, message: ChatMessage) {
    clearThinkingCloud(conversationId, { clearBubble: false });
    const runtime = assistantStreamRef.current;
    if (!runtime || runtime.messageId !== message.id || runtime.conversationId !== conversationId) {
      return;
    }
    const payload = assistantCloudPayload(message);
    const finalText = payload?.text ?? formatCloudText(message.content);
    const currentText = runtime.text;
    const missing = finalText.startsWith(currentText) ? finalText.slice(currentText.length) : "";
    runtime.attachments = payload?.attachments.length ? payload.attachments : runtime.attachments;
    rememberAssistantMirror(conversationId, message, payload?.signature ?? `${message.id}:${finalText}`);
    if (petVoiceReplyEnabledRef.current) {
      if (runtime.finalized) return;
      runtime.finalized = true;
      runtime.text = finalText || runtime.text;
      void speakPetAssistantReplyFullStream(
        runtime.requestKey,
        runtime.text,
        runtime.bubbleId,
        assistantCloudDurationMsRef.current,
        runtime.tone,
        runtime.attachments
      ).finally(() => {
        if (assistantStreamRef.current === runtime) assistantStreamRef.current = null;
      });
      return;
    }
    if (missing) {
      runtime.text += missing;
      revealAssistantStreamText(runtime, missing);
    }
    runtime.finalized = true;
    setCloudBubble((current) => (
      current?.id === runtime.bubbleId
        ? { ...current, attachments: payload?.attachments.length ? payload.attachments : current.attachments }
        : current
    ));
    activeVoiceReplyRequestRef.current = null;
    setPetVoicePlaybackActive(false);
    scheduleCloudDismiss(runtime.bubbleId, assistantCloudDurationMsRef.current);
  }

  async function refreshPetVoiceReplyState() {
    try {
      const persona = await resolvePetVoicePersona();
      if (!persona) {
        const fallback = defaultPetVoiceReplyConfig();
        petVoiceReplyConfigRef.current = fallback;
        setPetVoicePersonaName("");
        setPetVoiceReplyConfig(fallback);
        return;
      }
      const voiceReply = normalizePetVoiceReplyConfig(persona.voiceReply);
      petVoiceReplyConfigRef.current = voiceReply;
      setPetVoicePersonaName(persona.name ?? "");
      setPetVoiceReplyConfig(voiceReply);
    } catch (error) {
      console.error("pet voice state refresh failed:", error);
    }
  }

  async function resolveLatestPetVoiceReplyConfig() {
    try {
      const persona = await resolvePetVoicePersona();
      if (!persona) {
        return petVoiceReplyConfigRef.current;
      }
      const voiceReply = normalizePetVoiceReplyConfig(persona.voiceReply);
      petVoiceReplyConfigRef.current = voiceReply;
      setPetVoicePersonaName(persona.name ?? "");
      setPetVoiceReplyConfig(voiceReply);
      return voiceReply;
    } catch (error) {
      console.warn("pet latest voice reply config refresh failed:", error);
      return petVoiceReplyConfigRef.current;
    }
  }

  function applyPetVoiceReplyPersona(persona: Persona) {
    const context = activeContextRef.current ?? readStoredPetActiveContext();
    if (context?.personaId && context.personaId !== persona.id) return false;
    const voiceReply = normalizePetVoiceReplyConfig(persona.voiceReply);
    petVoiceReplyConfigRef.current = voiceReply;
    setPetVoicePersonaName(persona.name ?? "");
    setPetVoiceReplyConfig(voiceReply);
    return true;
  }

  async function togglePetVoiceReply() {
    if (petVoiceReplySaving) return;
    setPetVoiceReplySaving(true);
    try {
      const enabled = !petVoiceReplyEnabledRef.current;
      petVoiceReplyEnabledRef.current = enabled;
      setPetVoiceReplyEnabled(enabled);
      writeStoredPetVoiceReplyEnabled(enabled);
      if (!enabled) stopPetVoicePlayback();
      await refreshPetVoiceReplyState();
      showCloud(enabled ? "Pet 语音回复已开启。" : "Pet 语音回复已关闭。", "soft", 2400);
    } catch (error) {
      console.error("pet voice toggle failed:", error);
      showCloud("Pet 语音回复开关保存失败。", "error", 3200);
    } finally {
      setPetVoiceReplySaving(false);
    }
  }

  async function resolvePetVoicePersona() {
    const context = activeContextRef.current ?? readStoredPetActiveContext();
    const [conversations, personas] = await Promise.all([
      invoke<Conversation[]>("list_conversations"),
      api.listPersonas()
    ]);
    if (personas.length === 0) return null;
    const contextConversation = context?.conversationId
      ? conversations.find((conversation) => conversation.id === context.conversationId) ?? null
      : null;
    const personaId = contextConversation?.personaId ?? context?.personaId ?? conversations[0]?.personaId ?? null;
    return personas.find((persona) => persona.id === personaId) ?? personas[0] ?? null;
  }

  function assistantMirrorState(conversationId: string | null | undefined) {
    const resolvedConversationId = textValue(conversationId);
    if (!resolvedConversationId) return null;
    return assistantMirrorStateRef.current.get(resolvedConversationId) ?? null;
  }

  function rememberAssistantMirror(
    conversationId: string | null | undefined,
    message: ChatMessage,
    signature: string
  ) {
    const resolvedConversationId = textValue(conversationId) || message.conversationId;
    if (!resolvedConversationId) return;
    assistantMirrorStateRef.current.set(resolvedConversationId, {
      messageId: message.id,
      signature
    });
  }

  function assistantChangedSinceMirror(
    conversationId: string | null | undefined,
    message: ChatMessage,
    signature: string
  ) {
    const previous = assistantMirrorState(textValue(conversationId) || message.conversationId);
    if (!previous) return true;
    return previous.messageId !== message.id || previous.signature !== signature;
  }

  function showAssistantCloud(
    message: ChatMessage,
    conversationId = message.conversationId,
    durationMs = assistantCloudDurationMsRef.current
  ) {
    clearThinkingCloud(conversationId, { clearBubble: false });
    const runtime = assistantStreamRef.current;
    if (
      message.id
      && streamedAssistantMessageIdsRef.current.has(message.id)
      && runtime
      && runtime.messageId === message.id
      && runtime.conversationId === conversationId
    ) {
      finalizeAssistantStream(conversationId, message);
      return;
    }
    if (!assistantMessageVisibleInCloud(message)) return;
    const payload = assistantCloudPayload(message);
    if (!payload) return;
    if (!assistantChangedSinceMirror(conversationId, message, payload.signature)) return;
    rememberAssistantMirror(conversationId, message, payload.signature);
    stopPetVoicePlayback({ clearCloudStream: true });
    const requestKey = `${message.id}:${payload.signature}`;
    const attachments = payload.attachments.length ? payload.attachments : undefined;
    if (petVoiceReplyEnabledRef.current && payload.text.trim()) {
      activeVoiceReplyRequestRef.current = requestKey;
      const syncedCloud = showVoiceSyncedCloud(payload.text, "active", attachments);
      if (syncedCloud) {
        void speakPetAssistantReplyFullStream(
          requestKey,
          syncedCloud.text,
          syncedCloud.bubbleId,
          durationMs,
          syncedCloud.tone,
          syncedCloud.attachments
        );
      }
    } else {
      showStreamingCloud(payload.text, "active", durationMs, attachments);
    }
    const behavior = behaviorForAssistantText(payload.text);
    playPetBehavior(behavior, {
      durationMs: behavior === "stretch" ? 2300 : behavior === "error" ? 1300 : 1650
    });
  }

  async function playPetVoiceResult(
    requestKey: string,
    result: Awaited<ReturnType<typeof api.speakChatText>>,
    segmentText: string,
    onReveal?: (text: string) => void
  ) {
    if (activeVoiceReplyRequestRef.current !== requestKey) return;
    const dataUrl = String(result?.dataUrl ?? "");
    const artifactPath = String(result?.artifact?.path ?? "");
    if (!artifactPath && !dataUrl) {
      onReveal?.(segmentText);
      return;
    }
    const current = voiceAudioRef.current;
    if (current) {
      current.pause();
      current.src = "";
      voiceAudioRef.current = null;
    }

    const sources = [
      artifactPath ? convertFileSrc(artifactPath) : "",
      dataUrl
    ].filter(Boolean);
    const revealText = (durationMs: number) => {
      if (!onReveal) return () => {};
      const chars = Array.from(segmentText);
      if (chars.length === 0) return () => {};
      let revealed = 0;
      const startedAt = performance.now();
      const revealTo = (count: number) => {
        const next = Math.max(0, Math.min(chars.length, count));
        if (next <= revealed) return;
        onReveal(chars.slice(revealed, next).join(""));
        revealed = next;
      };
      const timer = window.setInterval(() => {
        const ratio = Math.min(1, (performance.now() - startedAt) / Math.max(240, durationMs));
        revealTo(Math.max(1, Math.floor(chars.length * ratio)));
        if (ratio >= 1) window.clearInterval(timer);
      }, 42);
      return () => {
        window.clearInterval(timer);
        revealTo(chars.length);
      };
    };

    for (const source of sources) {
      if (activeVoiceReplyRequestRef.current !== requestKey) return;
      const audio = new Audio(source);
      audio.preload = "auto";
      voiceAudioRef.current = audio;
      try {
        await new Promise<void>((resolve, reject) => {
          let finishReveal: (() => void) | null = null;
          const startReveal = () => {
            if (finishReveal) return;
            const durationMs = Number.isFinite(audio.duration) && audio.duration > 0
              ? audio.duration * 1000
              : Math.max(520, segmentText.length * 145);
            finishReveal = revealText(durationMs);
          };
          audio.onplaying = startReveal;
          audio.onended = () => {
            finishReveal?.();
            resolve();
          };
          audio.onerror = () => reject(new Error("pet voice audio playback failed"));
          void audio.play().then(startReveal).catch(reject);
        });
        if (voiceAudioRef.current === audio) voiceAudioRef.current = null;
        return;
      } catch (error) {
        if (voiceAudioRef.current === audio) voiceAudioRef.current = null;
        console.warn("pet voice segment playback failed:", error);
      }
    }

    if (artifactPath && isTauri() && activeVoiceReplyRequestRef.current === requestKey) {
      const finishReveal = revealText(Math.max(700, segmentText.length * 145));
      await api.playChatAudio?.(artifactPath);
      await new Promise((resolve) => window.setTimeout(resolve, Math.max(700, segmentText.length * 130)));
      finishReveal();
      return;
    }
    if (activeVoiceReplyRequestRef.current === requestKey) onReveal?.(segmentText);
  }

  async function speakPetAssistantReplyFullStream(
    requestKey: string,
    text: string,
    bubbleId: string,
    durationMs = assistantCloudDurationMsRef.current,
    cloudTone: PetCloudBubble["tone"] = "active",
    cloudAttachments?: PetCloudBubble["attachments"]
  ) {
    const speechText = sanitizeSpeechText(text).replace(/\s+/g, " ").trim();
    if (!petVoiceReplyEnabledRef.current || !speechText) {
      if (speechText) appendCloudBubbleText(bubbleId, speechText, cloudTone, cloudAttachments);
      if (activeVoiceReplyRequestRef.current === requestKey) activeVoiceReplyRequestRef.current = null;
      return;
    }
    setPetVoicePlaybackActive(true);
    try {
      const voiceReplyConfig = await resolveLatestPetVoiceReplyConfig();
      console.info(
        "SynthChat pet voice reply speak:",
        `engine=${voiceReplyConfig.engine || "default"}`,
        `voice=${voiceReplyConfig.voice || "default"}`
      );
      const result = await api.speakChatText(speechText, petVoiceReplySpeechOptions(voiceReplyConfig));
      if (activeVoiceReplyRequestRef.current !== requestKey) return;
      playPetBehavior("speaking", { durationMs: Math.max(1600, Math.min(12000, speechText.length * 120)) });
      await playPetVoiceResult(requestKey, result, speechText, (revealed) => {
        appendCloudBubbleText(bubbleId, revealed, cloudTone, cloudAttachments);
      });
    } catch (error) {
      if (activeVoiceReplyRequestRef.current === requestKey) {
        console.error("pet full voice reply failed:", error);
        appendCloudBubbleText(bubbleId, speechText, cloudTone, cloudAttachments);
      }
    } finally {
      if (activeVoiceReplyRequestRef.current === requestKey) {
        activeVoiceReplyRequestRef.current = null;
        setPetVoicePlaybackActive(false);
        scheduleCloudDismiss(bubbleId, durationMs);
      }
    }
  }

  async function refreshLatestAssistant(conversationId: string, showChanged = true) {
    if (!conversationId || activeContextRef.current?.conversationId !== conversationId) {
      return null;
    }
    if (showChanged && thinkingCloudRef.current?.conversationId === conversationId) {
      return null;
    }
    try {
      const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_PREVIEW_CHARS);
      if (activeContextRef.current?.conversationId !== conversationId) {
        return null;
      }
      const assistant = latestAssistantMessage(messages);
      if (!assistant) return null;
      const payload = assistantCloudPayload(assistant);
      if (!payload) return null;
      if (!showChanged) {
        rememberAssistantMirror(conversationId, assistant, payload.signature);
        return assistant;
      }
      if (assistantChangedSinceMirror(conversationId, assistant, payload.signature)) {
        showAssistantCloud(assistant, conversationId);
      } else {
        rememberAssistantMirror(conversationId, assistant, payload.signature);
      }
      return assistant;
    } catch (error) {
      console.error("pet message mirror failed:", error);
      return null;
    }
  }

  function setPetContext(context: PetActiveContext, persist = true) {
    activeContextRef.current = context;
    setActiveContext(context);
    if (persist) writeStoredPetActiveContext(context);
  }

  function updatePetActiveContext(context: PetSendContext) {
    const nextContext: PetActiveContext = {
      conversationId: context.conversationId,
      conversationTitle: activeContextRef.current?.conversationTitle ?? null,
      personaId: context.personaId,
      personaName: context.personaName,
      agentId: context.agentId,
      updatedAt: new Date().toISOString(),
      source: PET_ACTIVE_CONTEXT_SOURCE
    };
    setPetContext(nextContext);
    void emit(PET_ACTIVE_CONTEXT_EVENT, nextContext);
  }

  async function resolvePetSendContext(): Promise<PetSendContext> {
    const context = activeContextRef.current ?? readStoredPetActiveContext();
    const conversations = await invoke<Conversation[]>("list_conversations");
    const personas = await invoke<Persona[]>("list_personas");
    const agents = await api.listAgents();
    const contextConversation = context?.conversationId
      ? conversations.find((conversation) => conversation.id === context.conversationId) ?? null
      : null;
    const fallbackConversation = context?.personaId
      ? conversations.find((conversation) => conversation.personaId === context.personaId) ?? null
      : conversations[0] ?? null;
    const conversation = contextConversation ?? fallbackConversation;
    const validAgentId = (value: string | null | undefined) =>
      value && agents.some((agent) => agent.id === value) ? value : null;
    if (conversation) {
      const persona = conversation.personaId
        ? personas.find((item) => item.id === conversation.personaId) ?? null
        : null;
      return {
        conversationId: conversation.id,
        personaId: conversation.personaId ?? persona?.id ?? context?.personaId ?? null,
        personaName: persona?.name ?? context?.personaName ?? null,
        agentId: validAgentId(conversation.agentId ?? context?.agentId ?? null)
      };
    }
    const persona = context?.personaId
      ? personas.find((item) => item.id === context.personaId) ?? null
      : personas[0] ?? null;
    const created = await invoke<Conversation>("create_conversation", {
      title: persona?.name ?? "桌宠对话",
      personaId: persona?.id ?? null
    });
    return {
      conversationId: created.id,
      personaId: created.personaId ?? persona?.id ?? null,
      personaName: persona?.name ?? context?.personaName ?? null,
      agentId: validAgentId(created.agentId ?? context?.agentId ?? null)
    };
  }

  async function petVisionShouldSkipTurn(conversationId: string) {
    try {
      const [runs, queue] = await Promise.all([
        api.listAgentRuns(),
        api.listAgentQueue()
      ]);
      const activeRun = runs.some((run) =>
        run.conversationId === conversationId
        && !run.parentRunId
        && PET_VISION_BUSY_STATES.has(run.state)
      );
      if (activeRun) return true;
      return queue.some((item) =>
        item.conversationId === conversationId
        && (item.status === "pending" || item.status === "running")
      );
    } catch (error) {
      console.warn("pet vision busy check failed:", error);
      return true;
    }
  }

  async function waitForAssistantReply(
    conversationId: string,
    previousAssistantState: PetAssistantMirrorState | null
  ) {
    const deadline = Date.now() + 120000;
    while (Date.now() < deadline) {
      try {
        const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_PREVIEW_CHARS);
        const assistant = latestAssistantMessage(messages);
        if (!assistant) {
          await new Promise((resolve) => window.setTimeout(resolve, 800));
          continue;
        }
        const payload = assistantCloudPayload(assistant);
        if (!payload) {
          await new Promise((resolve) => window.setTimeout(resolve, 800));
          continue;
        }
        if (
          !previousAssistantState
          || previousAssistantState.messageId !== assistant.id
          || previousAssistantState.signature !== payload.signature
        ) {
          return assistant;
        }
      } catch (error) {
        console.error("pet wait reply failed:", error);
        return null;
      }
      await new Promise((resolve) => window.setTimeout(resolve, 800));
    }
    return null;
  }

  async function stagePetFiles(files: FileList | File[]) {
    const list = Array.from(files);
    if (list.length === 0) return;
    revealInput();
    for (const file of list) {
      const temporaryId = crypto.randomUUID();
      const preview = file.type.startsWith("image/") ? URL.createObjectURL(file) : null;
      setComposerAttachments((current) => [...current, {
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
        const saved = await api.uploadChatAttachment(
          file.name,
          file.type || "application/octet-stream",
          Array.from(new Uint8Array(buffer))
        );
        setComposerAttachments((current) => current.map((item) => (
          item.id === temporaryId ? { ...saved, preview, status: "ready" } : item
        )));
      } catch (error) {
        setComposerAttachments((current) => current.map((item) => (
          item.id === temporaryId ? { ...item, status: "error", error: String(error) } : item
        )));
      }
    }
  }

  async function stagePetFilePaths(paths: string[]) {
    const list = paths.map((path) => path.trim()).filter(Boolean);
    if (list.length === 0) return;
    revealInput();
    for (const path of list) {
      const temporaryId = crypto.randomUUID();
      setComposerAttachments((current) => [...current, {
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
        const preview = saved.mimeType.startsWith("image/") ? convertFileSrc(saved.path) : null;
        setComposerAttachments((current) => current.map((item) => (
          item.id === temporaryId ? { ...saved, preview, status: "ready" } : item
        )));
      } catch (error) {
        setComposerAttachments((current) => current.map((item) => (
          item.id === temporaryId ? { ...item, status: "error", error: String(error) } : item
        )));
      }
    }
  }

  function removePetAttachment(id: string) {
    setComposerAttachments((current) => current.filter((item) => item.id !== id));
  }

  function buildPetOutboundContent(text: string, readyAttachments: PetComposerAttachment[]) {
    const attachmentMarkers = readyAttachments
      .map((file) => `[media attached: "${file.path}" (${file.mimeType || "application/octet-stream"})] ${file.fileName}`)
      .join("\n");
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
    return [text, attachmentMarkers, attachmentContext].filter(Boolean).join("\n\n");
  }

  async function handleSubmit() {
    const text = input.trim();
    const readyAttachments = composerAttachments.filter((item) => item.status === "ready");
    const hasStagingAttachment = composerAttachments.some((item) => item.status === "staging");
    if (((!text && readyAttachments.length === 0) || hasStagingAttachment) || sendingRef.current) return;
    const submittedAttachments = composerAttachments;
    setInput("");
    setComposerAttachments([]);
    setModelMenuOpen(false);
    sendingRef.current = true;
    setSending(true);
    playPetBehavior("thinking");

    try {
      const context = await resolvePetSendContext();
      updatePetActiveContext(context);
      showThinkingCloud(context.conversationId);
      const previousAssistantState = assistantMirrorState(context.conversationId);
      const messages = await api.sendChatMessage({
        conversationId: context.conversationId,
        personaId: context.personaId,
        agentId: context.agentId,
        content: buildPetOutboundContent(text, readyAttachments),
        providerData: {
          source: "pet"
        }
      });
      const assistant = latestAssistantMessage(messages)
        ?? await waitForAssistantReply(context.conversationId, previousAssistantState);
      if (assistant) {
        if (streamedAssistantMessageIdsRef.current.has(assistant.id)) {
          finalizeAssistantStream(context.conversationId, assistant);
        } else {
          showAssistantCloud(assistant, context.conversationId);
        }
      } else {
        showThinkingCloud(context.conversationId);
        playPetBehavior("thinking", { durationMs: 1600 });
      }
    } catch (error) {
      console.error("pet send failed:", error);
      setInput((current) => current.trim() ? current : text);
      setComposerAttachments((current) => current.length > 0 ? current : submittedAttachments);
      showCloud("发送失败。", "error", 3600);
      playPetBehavior("error");
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  }

  function switchModel(model: PetModel) {
    void syncPetPointerPassthrough(false);
    if (model.id === selectedModel.id) {
      showCloud(`${model.name} 已经在这里。`, "soft", 1800);
      return;
    }
    selectedModelRef.current = model;
    writeStoredPetModel(model);
    setSelectedModel(model);
    setModelLoaded(false);
    modelBoundsRef.current = null;
    showCloud(model.greeting, "happy", 2600);
    loadModel(model, true);
  }

  async function petWindowAction(action: "expand" | "model" | "drag" | "orb" | "undock", edge: PetDockEdge | null = null) {
    try {
      await invoke("pet_window_action", { action, edge });
    } catch (error) {
      console.error("pet window action failed:", error);
    }
  }

  async function setPetWindowModeState(mode: PetWindowMode, edge: PetDockEdge = dockEdgeRef.current) {
    petWindowModeRef.current = mode;
    setPetWindowMode(mode);
    dockEdgeRef.current = edge;
    setDockEdge(edge);
    if (mode === "orb") {
      clearInputHideTimer();
      showInputRef.current = false;
      setShowInput(false);
      setModelMenuOpen(false);
      modelMenuOpenRef.current = false;
      setCloudBubble(null);
      await syncPetPointerPassthrough(false);
      await petWindowAction("orb", edge);
      return;
    }
    await syncPetPointerPassthrough(false);
    await petWindowAction("model");
  }

  async function toggleMainWindow() {
    try {
      await invoke("toggle_main_window");
    } catch (error) {
      console.error("toggle main window failed:", error);
    }
  }

  async function syncPetPointerPassthrough(ignore: boolean) {
    if (ignoreCursorEventsRef.current === ignore) return;
    ignoreCursorEventsRef.current = ignore;
    try {
      await invoke("pet_window_set_ignore_cursor_events", { ignore });
    } catch (error) {
      ignoreCursorEventsRef.current = !ignore;
      console.error("pet pointer passthrough failed:", error);
    }
  }

  function pointNearModel(clientX: number, clientY: number) {
    const bounds = modelBoundsRef.current;
    if (!bounds) return false;
    const padding = Math.max(72, Math.min(140, Math.max(bounds.width, bounds.height) * 0.16));
    return (
      clientX >= bounds.x - padding
      && clientX <= bounds.x + bounds.width + padding
      && clientY >= bounds.y - padding
      && clientY <= bounds.y + bounds.height + padding
    );
  }

  function pointInsideModelSurface(clientX: number, clientY: number) {
    const bounds = modelBoundsRef.current;
    if (!bounds) return false;
    return (
      clientX >= bounds.x - PET_MODEL_INPUT_WAKE_PADDING_PX
      && clientX <= bounds.x + bounds.width + PET_MODEL_INPUT_WAKE_PADDING_PX
      && clientY >= bounds.y - PET_MODEL_INPUT_WAKE_PADDING_PX
      && clientY <= bounds.y + bounds.height + PET_MODEL_INPUT_WAKE_PADDING_PX
    );
  }

  function normalizeCursorPosition(position: PetCursorPosition) {
    const rawClientX = typeof position.clientX === "number" ? position.clientX : Number.NaN;
    const rawClientY = typeof position.clientY === "number" ? position.clientY : Number.NaN;
    if (!Number.isFinite(rawClientX) || !Number.isFinite(rawClientY)) return null;

    const cssWidth = Math.max(1, window.innerWidth);
    const cssHeight = Math.max(1, window.innerHeight);
    if (
      rawClientX >= -cssWidth * 0.2
      && rawClientX <= cssWidth * 1.2
      && rawClientY >= -cssHeight * 0.2
      && rawClientY <= cssHeight * 1.2
    ) {
      return { clientX: rawClientX, clientY: rawClientY };
    }
    const scaleX = typeof position.windowWidth === "number" && position.windowWidth > 0
      ? position.windowWidth / cssWidth
      : window.devicePixelRatio;
    const scaleY = typeof position.windowHeight === "number" && position.windowHeight > 0
      ? position.windowHeight / cssHeight
      : window.devicePixelRatio;

    const normalized = {
      clientX: scaleX > 1.01 ? rawClientX / scaleX : rawClientX,
      clientY: scaleY > 1.01 ? rawClientY / scaleY : rawClientY
    };
    if (
      normalized.clientX >= -cssWidth * 0.4
      && normalized.clientX <= cssWidth * 1.4
      && normalized.clientY >= -cssHeight * 0.4
      && normalized.clientY <= cssHeight * 1.4
    ) {
      return normalized;
    }
    const windowX = typeof position.windowScreenX === "number" ? position.windowScreenX : Number.NaN;
    const windowY = typeof position.windowScreenY === "number" ? position.windowScreenY : Number.NaN;
    const screenX = typeof position.screenX === "number" ? position.screenX : position.x;
    const screenY = typeof position.screenY === "number" ? position.screenY : position.y;
    if (Number.isFinite(windowX) && Number.isFinite(windowY) && typeof screenX === "number" && typeof screenY === "number") {
      return {
        clientX: screenX - windowX,
        clientY: screenY - windowY
      };
    }
    return normalized;
  }

  async function updateGlobalLook() {
    if (!modelLoadedRef.current || globalLookInFlightRef.current) return;
    globalLookInFlightRef.current = true;
    try {
      const position = await invoke<PetCursorPosition>("cursor_position");
      if (!modelLoadedRef.current) return;

      const point = normalizeCursorPosition(position);
      const currentX = typeof position.x === "number" ? position.x : position.screenX;
      const currentY = typeof position.y === "number" ? position.y : position.screenY;
      const hasGlobalPoint = typeof currentX === "number" && typeof currentY === "number";

      if (point && hasGlobalPoint) {
        const previousPoint = lastLookPointRef.current;
        if (
          !previousPoint
          || Math.abs(previousPoint.x - currentX) > 1
          || Math.abs(previousPoint.y - currentY) > 1
        ) {
          lastLookMoveAtRef.current = Date.now();
          lastLookPointRef.current = { x: currentX, y: currentY };
          postToPet({
            type: "look",
            x: point.clientX,
            y: point.clientY,
            clientX: point.clientX,
            clientY: point.clientY,
            instant: false
          });
          return;
        }
      }

      if (Date.now() - lastLookMoveAtRef.current > PET_GLOBAL_LOOK_IDLE_MS) {
        const centerX = window.innerWidth / 2;
        const centerY = window.innerHeight / 2;
        postToPet({
          type: "look",
          x: centerX,
          y: centerY,
          clientX: centerX,
          clientY: centerY,
          instant: false
        });
        lastLookMoveAtRef.current = Date.now();
      }
    } catch {
      // pet.js can still use in-window pointer movement if global cursor lookup fails.
    } finally {
      globalLookInFlightRef.current = false;
    }
  }

  function rectContainsPoint(element: Element | null, clientX: number, clientY: number, padding = 0) {
    if (!element) return false;
    const rect = element.getBoundingClientRect();
    if (rect.width <= 0 || rect.height <= 0) return false;
    return (
      clientX >= rect.left - padding
      && clientX <= rect.right + padding
      && clientY >= rect.top - padding
      && clientY <= rect.bottom + padding
    );
  }

  function isPointerInPetUi(clientX: number, clientY: number) {
    // The bubble is display-only; only the input shell and model menu
    // participate in the hide/reveal hover logic.
    if (!showInputRef.current && !modelMenuOpenRef.current) {
      return Boolean(rectContainsPoint(modelMenuRef.current, clientX, clientY, 8));
    }
    if (rectContainsPoint(modelMenuRef.current, clientX, clientY, 8)) return true;
    const element = document.elementFromPoint(clientX, clientY);
    return Boolean(element?.closest(".pet-input-wrap, .pet-input-model-menu, .pet-input-attachment-row, .pet-file-drop-overlay"));
  }

  async function startModelDrag(screenX?: number, screenY?: number) {
    if (typeof screenX !== "number" || typeof screenY !== "number") return;
    if (modelDragActiveRef.current) return;
    const dragToken = ++modelDragTokenRef.current;
    modelDragActiveRef.current = true;
    modelDragMovedRef.current = false;
    modelDragStartReadyRef.current = false;
    modelDragLatestPointRef.current = { screenX, screenY };
    modelDragLastMovePointRef.current = { screenX, screenY };
    modelDragVelocityRef.current = { x: 0, y: 0 };
    modelDragLastSampleRef.current = { point: { screenX, screenY }, at: performance.now() };
    cancelModelDragInertia();
    try {
      await invoke("pet_window_drag", { action: "start", screenX, screenY, useCursor: true });
      if (dragToken !== modelDragTokenRef.current || !modelDragActiveRef.current) {
        void invoke("pet_window_drag", { action: "end" }).catch((error) => {
          console.error("pet drag stale end failed:", error);
        });
        return;
      }
      modelDragStartReadyRef.current = true;
      const latest = modelDragLatestPointRef.current ?? { screenX, screenY };
      queueModelDragMove(latest.screenX, latest.screenY);
    } catch (error) {
      resetModelDragState();
      console.error("pet drag start failed:", error);
    }
  }

  function moveModelDrag(screenX?: number, screenY?: number) {
    if (typeof screenX !== "number" || typeof screenY !== "number") return;
    if (!modelDragActiveRef.current) return;
    modelDragMovedRef.current = true;
    sampleModelDragVelocity(screenX, screenY);
    queueModelDragMove(screenX, screenY);
  }

  async function finishModelDrag(screenX?: number, screenY?: number) {
    const latest = modelDragLatestPointRef.current;
    const endPoint = typeof screenX === "number" && typeof screenY === "number"
      ? { screenX, screenY }
      : latest;
    const inertiaSnapshot = {
      moved: modelDragMovedRef.current,
      velocity: { ...modelDragVelocityRef.current },
      from: modelDragLastMovePointRef.current ?? endPoint
    };
    const wasActive = modelDragActiveRef.current;
    resetModelDragState();
    if (wasActive) {
      await invoke("pet_window_drag", { action: "end" }).catch((error) => {
        console.error("pet drag end failed:", error);
      });
    }
    const edge = await detectDockEdge(endPoint);
    if (edge) {
      await setPetWindowModeState("orb", edge);
      return;
    }
    startModelDragInertia(endPoint, inertiaSnapshot);
    showCloud("我先停在这里。", "soft", 2000);
  }

  async function detectDockEdge(point: PetDragPoint | null): Promise<PetDockEdge | null> {
    try {
      const position = await invoke<PetCursorPosition>("cursor_position");
      const originX = typeof position.screenXOrigin === "number" ? position.screenXOrigin : 0;
      const screenWidth = typeof position.screenWidth === "number" && position.screenWidth > 0
        ? position.screenWidth
        : window.screen.width;
      const screenRight = originX + screenWidth;
      const windowX = typeof position.windowScreenX === "number" ? position.windowScreenX : Number.NaN;
      const windowWidth = typeof position.windowWidth === "number" && position.windowWidth > 0
        ? position.windowWidth
        : Number.NaN;
      const pointerX = point?.screenX
        ?? (typeof position.screenX === "number" ? position.screenX : position.x);

      const windowNearLeft = Number.isFinite(windowX) && windowX <= originX + PET_EDGE_SNAP_THRESHOLD_PX;
      const windowNearRight = Number.isFinite(windowX) && Number.isFinite(windowWidth)
        && windowX + windowWidth >= screenRight - PET_EDGE_SNAP_THRESHOLD_PX;
      const pointerNearLeft = typeof pointerX === "number" && pointerX <= originX + PET_EDGE_POINTER_THRESHOLD_PX;
      const pointerNearRight = typeof pointerX === "number" && pointerX >= screenRight - PET_EDGE_POINTER_THRESHOLD_PX;

      if (windowNearLeft || pointerNearLeft) return "left";
      if (windowNearRight || pointerNearRight) return "right";
    } catch (error) {
      console.error("pet edge detect failed:", error);
    }
    return null;
  }

  function queueModelDragMove(screenX: number, screenY: number) {
    modelDragLatestPointRef.current = { screenX, screenY };
    if (!modelDragStartReadyRef.current || modelDragMoveFrameRef.current !== null) {
      return;
    }
    modelDragMoveFrameRef.current = window.requestAnimationFrame(() => {
      modelDragMoveFrameRef.current = null;
      void flushModelDragMove();
    });
  }

  function flushModelDragMove() {
    if (!modelDragActiveRef.current || !modelDragStartReadyRef.current) return;
    const point = modelDragLatestPointRef.current;
    if (!point) return;
    const movePoint = { ...point };
    modelDragLastMovePointRef.current = movePoint;
    void invoke("pet_window_drag", {
      action: "move",
      screenX: movePoint.screenX,
      screenY: movePoint.screenY,
      useCursor: true
    }).catch((error) => {
      console.error("pet drag move failed:", error);
      stopModelDrag();
    });
  }

  function sampleModelDragVelocity(screenX: number, screenY: number) {
    const now = performance.now();
    const previous = modelDragLastSampleRef.current;
    if (previous) {
      const elapsed = Math.max(16, now - previous.at);
      const vx = (screenX - previous.point.screenX) / elapsed;
      const vy = (screenY - previous.point.screenY) / elapsed;
      modelDragVelocityRef.current = {
        x: modelDragVelocityRef.current.x * 0.55 + vx * 0.45,
        y: modelDragVelocityRef.current.y * 0.55 + vy * 0.45
      };
    }
    modelDragLastSampleRef.current = { point: { screenX, screenY }, at: now };
  }

  function cancelModelDragInertia() {
    if (modelDragInertiaFrameRef.current !== null) {
      window.cancelAnimationFrame(modelDragInertiaFrameRef.current);
      modelDragInertiaFrameRef.current = null;
    }
  }

  function startModelDragInertia(endPoint: PetDragPoint | null, snapshot: PetDragInertiaSnapshot) {
    if (!endPoint || !snapshot.moved || !snapshot.from) return;
    const velocity = snapshot.velocity;
    const speed = Math.hypot(velocity.x, velocity.y);
    if (speed < PET_MODEL_INERTIA_MIN_SPEED) return;
    const distance = Math.min(PET_MODEL_INERTIA_MAX_DISTANCE, speed * PET_MODEL_INERTIA_DISTANCE_MULTIPLIER);
    const unitX = velocity.x / speed;
    const unitY = velocity.y / speed;
    const from = snapshot.from;
    const startAt = performance.now();
    const inertiaToken = modelDragTokenRef.current;
    cancelModelDragInertia();
    void invoke("pet_window_drag", { action: "start", screenX: from.screenX, screenY: from.screenY }).then(() => {
      if (inertiaToken !== modelDragTokenRef.current || modelDragActiveRef.current) {
        void invoke("pet_window_drag", { action: "end" }).catch(() => undefined);
        return;
      }
      const tick = () => {
        if (inertiaToken !== modelDragTokenRef.current || modelDragActiveRef.current) {
          modelDragInertiaFrameRef.current = null;
          void invoke("pet_window_drag", { action: "end" }).catch(() => undefined);
          return;
        }
        const elapsed = performance.now() - startAt;
        const progress = Math.min(1, elapsed / PET_MODEL_INERTIA_DURATION_MS);
        const ease = 1 - Math.pow(1 - progress, 3.2);
        const reboundProgress = progress > 0.68 ? (progress - 0.68) / 0.32 : 0;
        const rebound = Math.sin(reboundProgress * Math.PI) * PET_MODEL_INERTIA_REBOUND_PX * (1 - progress);
        const next = {
          screenX: from.screenX + unitX * (distance * ease - rebound),
          screenY: from.screenY + unitY * (distance * ease - rebound * 0.72)
        };
        void invoke("pet_window_drag", { action: "move", screenX: next.screenX, screenY: next.screenY });
        if (progress < 1) {
          modelDragInertiaFrameRef.current = window.requestAnimationFrame(tick);
        } else {
          modelDragInertiaFrameRef.current = null;
          void invoke("pet_window_drag", { action: "end" });
        }
      };
      modelDragInertiaFrameRef.current = window.requestAnimationFrame(tick);
    }).catch((error) => {
      console.error("pet drag inertia failed:", error);
      void invoke("pet_window_drag", { action: "end" }).catch(() => undefined);
    });
  }

  function resetModelDragState() {
    modelDragTokenRef.current += 1;
    modelDragActiveRef.current = false;
    modelDragStartReadyRef.current = false;
    modelDragLatestPointRef.current = null;
    modelDragLastMovePointRef.current = null;
    modelDragLastSampleRef.current = null;
    modelDragVelocityRef.current = { x: 0, y: 0 };
    if (modelDragMoveFrameRef.current !== null) {
      window.cancelAnimationFrame(modelDragMoveFrameRef.current);
      modelDragMoveFrameRef.current = null;
    }
  }

  function stopModelDrag() {
    if (!modelDragActiveRef.current) return;
    resetModelDragState();
    void invoke("pet_window_drag", { action: "end" }).catch((error) => {
      console.error("pet drag end failed:", error);
    });
  }

  function startOrbDrag(event: ReactPointerEvent<HTMLButtonElement>) {
    if (event.button !== 0) return;
    event.preventDefault();
    event.currentTarget.setPointerCapture?.(event.pointerId);
    orbDragActiveRef.current = true;
    orbDragMovedRef.current = false;
    orbDragStartPointRef.current = { screenX: event.screenX, screenY: event.screenY };
    void invoke("pet_window_drag", {
      action: "start",
      screenX: event.screenX,
      screenY: event.screenY,
      useCursor: true
    }).catch((error) => {
      orbDragActiveRef.current = false;
      orbDragStartPointRef.current = null;
      console.error("pet orb drag start failed:", error);
    });
  }

  function moveOrbDrag(event: ReactPointerEvent<HTMLButtonElement>) {
    if (!orbDragActiveRef.current) return;
    const start = orbDragStartPointRef.current;
    if (
      start
      && Math.hypot(event.screenX - start.screenX, event.screenY - start.screenY) > PET_ORB_CLICK_MOVE_TOLERANCE_PX
    ) {
      orbDragMovedRef.current = true;
    }
    void invoke("pet_window_drag", {
      action: "move",
      screenX: event.screenX,
      screenY: event.screenY,
      useCursor: true
    }).catch((error) => {
      console.error("pet orb drag move failed:", error);
    });
  }

  function finishOrbDrag(event: ReactPointerEvent<HTMLButtonElement>) {
    if (!orbDragActiveRef.current) return;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    orbDragActiveRef.current = false;
    orbDragStartPointRef.current = null;
    void invoke("pet_window_drag", { action: "end" }).catch((error) => {
      console.error("pet orb drag end failed:", error);
    });
    if (!orbDragMovedRef.current) {
      void setPetWindowModeState("model");
      window.setTimeout(() => showCloud("我回来啦。", "happy", 2200), 120);
    }
  }

  function cancelOrbDrag() {
    if (!orbDragActiveRef.current) return;
    orbDragActiveRef.current = false;
    orbDragMovedRef.current = false;
    orbDragStartPointRef.current = null;
    void invoke("pet_window_drag", { action: "end" }).catch((error) => {
      console.error("pet orb drag cancel failed:", error);
    });
  }

  function cloudStyle(): PetCloudStyle {
    const bounds = modelBoundsRef.current;
    const modelProfile = selectedModelRef.current;
    const viewportWidth = Math.max(1, window.innerWidth);
    const viewportHeight = Math.max(1, window.innerHeight);
    const maxBubbleWidth = Math.min(560, Math.max(320, viewportWidth - 28));
    const minBubbleWidth = Math.min(maxBubbleWidth, 304);
    const visibleAttachments = renderedCloudAttachments.filter((attachment) => !attachment.hidden);
    const imageAttachmentRows = visibleAttachments.filter((attachment) => isImageAttachment(attachment)).length;
    const fileAttachmentRows = visibleAttachments.length - imageAttachmentRows;
    const textLength = cloudBubble?.text?.trim().length ?? 0;
    const targetCharsPerLine = textLength > 0 ? Math.ceil(textLength / 5) : 0;
    const estimatedTextWidth = textLength > 0 ? 152 + targetCharsPerLine * 11 : minBubbleWidth;
    const estimatedAttachmentWidth = imageAttachmentRows > 0 ? 356 : fileAttachmentRows > 0 ? 318 : 0;
    const width = Math.max(
      minBubbleWidth,
      Math.min(maxBubbleWidth, Math.max(estimatedTextWidth, estimatedAttachmentWidth))
    );
    const estimatedCharsPerRenderedLine = Math.max(14, Math.floor((width - 72) / 11));
    const estimatedVisibleTextLines = textLength > 0
      ? Math.max(1, Math.min(5, Math.ceil(textLength / estimatedCharsPerRenderedLine)))
      : 0;
    const estimatedTextHeight = estimatedVisibleTextLines > 0 ? estimatedVisibleTextLines * 22 + 8 : 0;
    const estimatedAttachmentHeight = Math.min(208, imageAttachmentRows * 96 + fileAttachmentRows * 44);
    const height = Math.max(124, Math.min(268, 40 + estimatedTextHeight + estimatedAttachmentHeight));
    const fallbackLeft = Math.max(14, Math.round((viewportWidth - width) / 2));
    const fallbackTop = 8;
    if (!bounds) {
      const startX = Math.round(width * 0.54);
      const tailX = Math.round(width * 0.58);
      const tailY = height + 46;
      return {
        left: `${fallbackLeft}px`,
        top: `${fallbackTop}px`,
        width: `${width}px`,
        "--pet-cloud-tail-start-x": `${startX}px`,
        "--pet-cloud-tail-start-y": `${height - 14}px`,
        "--pet-cloud-tail-x": `${tailX}px`,
        "--pet-cloud-tail-y": `${tailY}px`,
        "--pet-cloud-tail-length": "56px",
        "--pet-cloud-tail-angle": "88deg",
        "--pet-cloud-dot-1-x": `${Math.round(startX + (tailX - startX) * 0.34)}px`,
        "--pet-cloud-dot-1-y": `${height + 10}px`,
        "--pet-cloud-dot-2-x": `${Math.round(startX + (tailX - startX) * 0.64)}px`,
        "--pet-cloud-dot-2-y": `${height + 30}px`,
        "--pet-cloud-dot-3-x": `${tailX}px`,
        "--pet-cloud-dot-3-y": `${tailY}px`
      };
    }

    const headAnchorX = bounds.x + bounds.width * modelProfile.headX;
    const headAnchorY = bounds.y + bounds.height * modelProfile.headY;
    const bubbleHeadGap = Math.max(modelProfile.tailGap + 12, bounds.height * 0.11);
    const tailHeadGap = Math.max(10, Math.round(modelProfile.tailGap * 0.42));
    const desiredLeft = headAnchorX - width * 0.54;
    const left = Math.min(
      Math.max(14, desiredLeft),
      Math.max(14, viewportWidth - width - 14)
    );
    const desiredBubbleBottomAbs = headAnchorY - bubbleHeadGap;
    const bubbleBottomAbs = Math.max(height + 4, Math.min(viewportHeight - 18, desiredBubbleBottomAbs));
    const top = Math.max(4, bubbleBottomAbs - height);
    const tailXAbs = Math.min(viewportWidth - 14, Math.max(14, headAnchorX));
    const tailYAbs = Math.max(
      bubbleBottomAbs + 22,
      Math.min(viewportHeight - 60, headAnchorY - tailHeadGap)
    );
    const tailX = Math.min(width + 64, Math.max(-64, tailXAbs - left));
    const tailY = Math.max(height + 24, tailYAbs - top);
    const startX = Math.min(width - 46, Math.max(46, width * 0.5 + (tailX - width * 0.5) * 0.34));
    const startY = height - 14;
    const dx = tailX - startX;
    const dy = tailY - startY;
    const dot = (ratio: number) => ({
      x: Math.round(startX + dx * ratio),
      y: Math.round(startY + dy * ratio)
    });
    const dot1 = dot(0.32);
    const dot2 = dot(0.62);
    const dot3 = dot(0.9);

    return {
      left: `${Math.round(left)}px`,
      top: `${Math.round(top)}px`,
      width: `${Math.round(width)}px`,
      "--pet-cloud-tail-start-x": `${Math.round(startX)}px`,
      "--pet-cloud-tail-start-y": `${Math.round(startY)}px`,
      "--pet-cloud-tail-x": `${Math.round(tailX)}px`,
      "--pet-cloud-tail-y": `${Math.round(tailY)}px`,
      "--pet-cloud-tail-length": `${Math.round(Math.min(86, Math.max(30, Math.hypot(dx, dy))))}px`,
      "--pet-cloud-tail-angle": `${Math.round(Math.atan2(dy, dx) * 180 / Math.PI)}deg`,
      "--pet-cloud-dot-1-x": `${dot1.x}px`,
      "--pet-cloud-dot-1-y": `${dot1.y}px`,
      "--pet-cloud-dot-2-x": `${dot2.x}px`,
      "--pet-cloud-dot-2-y": `${dot2.y}px`,
      "--pet-cloud-dot-3-x": `${dot3.x}px`,
      "--pet-cloud-dot-3-y": `${dot3.y}px`
    };
  }

  return (
    <main className={`live2d-pet-shell${cloudBubble || petVoicePlaybackActive ? " is-speaking" : ""}${petWindowMode === "orb" ? " is-orb" : ""}${petAvatarRevealed ? " is-avatar-revealed" : " is-avatar-priming"}`}>
      <iframe
        className="live2d-pet-frame"
        ref={frameRef}
        src="/pet/index.html?v=20260628-touch-drag-v2"
        title="SynthPet Live2D"
      />

      {petWindowMode !== "orb" && petStartupVisible ? (
        <PetStartupAwakening avatarRevealed={petAvatarRevealed} exiting={petStartupExiting} />
      ) : null}

      {petWindowMode === "orb" ? (
        <button
          className={`pet-pokeball-orb is-${dockEdge}`}
          type="button"
          aria-label="唤出桌宠"
          title="唤出桌宠"
          onPointerDown={startOrbDrag}
          onPointerMove={moveOrbDrag}
          onPointerUp={finishOrbDrag}
          onPointerCancel={cancelOrbDrag}
          onLostPointerCapture={cancelOrbDrag}
        >
          <span className="pet-pokeball-top" aria-hidden="true" />
          <span className="pet-pokeball-band" aria-hidden="true" />
          <span className="pet-pokeball-button" aria-hidden="true" />
        </button>
      ) : null}

      {petWindowMode !== "orb" ? (
      <section className={`pet-speech-area${cloudBubble ? " has-bubble" : ""}`} aria-live="polite">
        {cloudBubble ? (
          <section
            className={`pet-cloud-bubble is-${cloudBubble.tone}`}
            key={cloudBubble.id}
            style={cloudStyle()}
          >
            {renderedCloudAttachments.map((a, i) => {
              if (a.hidden) return null;
              const isImage = isImageAttachment(a) && !a.imageFailed;
              const ext = a.fileName.split(".").pop()?.toLowerCase() ?? "";
              const docIcon: Record<string, string> = { pdf: "📄", pptx: "📊", ppt: "📊", docx: "📝", doc: "📝", xlsx: "📊", xls: "📊", txt: "📃", csv: "📊" };
              return isImage ? (
                <img
                  key={i}
                  className="pet-cloud-attachment-img"
                  src={convertFileSrc(a.resolvedPath)}
                  alt={a.fileName}
                  title={a.fileName}
                  onError={() => {
                    setBrokenCloudImages((current) => {
                      if (current[a.resolvedPath]) return current;
                      return { ...current, [a.resolvedPath]: true };
                    });
                  }}
                />
              ) : (
                <span key={i} className="pet-cloud-attachment-file" title={a.resolvedPath || a.path}>
                  <span className="pet-cloud-attachment-icon">{docIcon[ext] ?? "📎"}</span>
                  <span className="pet-cloud-attachment-name">{a.fileName}</span>
                </span>
              );
            })}
            {cloudBubble.text ? (
              <span className="pet-cloud-text" title={cloudBubble.text}>{cloudBubble.text}</span>
            ) : null}
            <span className="pet-cloud-tail" aria-hidden="true">
              <span />
              <span />
              <span />
            </span>
          </section>
        ) : null}
      </section>
      ) : null}

      {petWindowMode !== "orb" ? (
      <section
        className={`pet-input-shell${showInput ? "" : " is-hidden"}${modelMenuOpen ? " is-menu-open" : ""}${inputDragActive ? " is-dragging" : ""}`}
        ref={inputShellRef}
        aria-label="桌宠输入"
        onFocusCapture={revealInput}
        onDragEnter={handlePetFileDragEnter}
        onDragOver={handlePetFileDragOver}
        onDragLeave={handlePetFileDragLeave}
        onDrop={handlePetFileDrop}
        onMouseEnter={revealPetInputShell}
        onMouseLeave={scheduleInputHide}
        onPointerDown={revealPetInputShell}
      >
        <div
          className="pet-input-hot-zone"
          aria-hidden="true"
          onPointerDown={activatePetInputHotZone}
        />
        <div className="pet-input-wrap">
          <button
            className="pet-input-model-button"
            onClick={toggleModelMenu}
            title="功能菜单"
            type="button"
            aria-expanded={modelMenuOpen}
            aria-label="功能菜单"
          >
            <Menu size={15} strokeWidth={2.4} aria-hidden="true" />
          </button>
          <input
            ref={inputRef}
            autoComplete="off"
            onChange={(event) => setInput(event.target.value)}
            onFocus={() => {
              revealInput();
              void syncPetPointerPassthrough(false);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                void handleSubmit();
              }
            }}
            placeholder={activeContext?.personaName ? `与 ${activeContext.personaName} 聊天...` : "说点什么..."}
            style={{ minWidth: "160px" }}
            spellCheck={false}
            type="text"
            value={input}
          />
          <button
            className="pet-input-send-button"
            disabled={
              sending
              || composerAttachments.some((item) => item.status === "staging")
              || (!input.trim() && composerAttachments.every((item) => item.status !== "ready"))
            }
            onClick={() => void handleSubmit()}
            title="发送"
            type="button"
            aria-label="发送"
          >
            <SendHorizontal size={16} strokeWidth={2.5} aria-hidden="true" />
          </button>
        </div>
        {composerAttachments.length > 0 ? (
          <div className="pet-input-attachment-row">
            {composerAttachments.map((file) => (
              <div className={`pet-input-attachment ${file.status}`} key={file.id}>
                {file.preview ? <img src={file.preview} alt={file.fileName} /> : <FileText size={14} />}
                <span>{file.fileName}</span>
                {file.status === "staging" ? <Loader2 className="spin" size={12} /> : null}
                {file.status === "error" ? <small>{file.error || "上传失败"}</small> : null}
                <button onClick={() => removePetAttachment(file.id)} title="移除附件" type="button">
                  <X size={11} />
                </button>
              </div>
            ))}
          </div>
        ) : null}
        {inputDragActive ? (
          <div className="pet-file-drop-overlay" aria-hidden="true">
            <div className="pet-file-drop-message">
              <FileText size={18} />
              <strong>松开即可添加</strong>
              <span>文件会作为 Pet 消息附件</span>
            </div>
          </div>
        ) : null}
        {modelMenuOpen ? (
          <div className="pet-input-model-menu" ref={modelMenuRef} role="menu" style={{ padding: "8px", gap: "6px" }}>
            <div style={{ gridColumn: "1 / -1", padding: "2px 8px 6px", fontSize: "11px", color: "#64748b", fontWeight: 700, letterSpacing: "0.5px" }}>功能选项</div>
            <div className="pet-vision-menu-row" role="group" aria-label="视觉感知">
              <button
                className={visionEnabled ? "is-selected" : ""}
                onClick={() => setVisionEnabled(v => !v)}
                type="button"
                role="menuitem"
              >
                {visionEnabled ? <Eye size={16} /> : <EyeOff size={16} />}
                <span>视觉感知 {visionEnabled ? "(开启)" : "(关闭)"}</span>
              </button>
              <label className="pet-vision-interval-control">
                <span>间隔</span>
                <input
                  aria-label="视觉感知间隔秒数"
                  min={MIN_PET_VISION_INTERVAL_SECONDS}
                  onBlur={() => setVisionIntervalSeconds((current) => clampPetVisionIntervalSeconds(current))}
                  onChange={(event) => {
                    const next = Number(event.target.value);
                    setVisionIntervalSeconds(Number.isFinite(next) ? next : DEFAULT_PET_VISION_INTERVAL_SECONDS);
                  }}
                  step={10}
                  type="number"
                  value={visionIntervalSeconds}
                />
                <span>秒</span>
              </label>
            </div>
            <button
              className={`pet-voice-menu-button${petVoiceReplyEnabled ? " is-selected" : ""}`}
              disabled={petVoiceReplySaving}
              onClick={() => void togglePetVoiceReply()}
              type="button"
              role="menuitem"
              title={petVoicePersonaName ? `当前角色：${petVoicePersonaName}` : "跟随当前 Pet 会话角色"}
            >
              {petVoiceReplySaving ? <Loader2 className="spin" size={16} /> : petVoiceReplyEnabled ? <Volume2 size={16} /> : <VolumeX size={16} />}
              <span>桌面语音回复 {petVoiceReplyEnabled ? "(开启)" : "(关闭)"}</span>
            </button>
            <div style={{ gridColumn: "1 / -1", height: 1, background: "rgba(0,0,0,0.06)", margin: "4px 4px" }} />
            <div style={{ gridColumn: "1 / -1", padding: "4px 8px 6px", fontSize: "11px", color: "#64748b", fontWeight: 700, letterSpacing: "0.5px" }}>模型切换</div>
            {AVAILABLE_MODELS.map((model) => (
              <button
                className={model.id === selectedModel.id ? "is-selected" : ""}
                key={model.id}
                onClick={() => switchModel(model)}
                type="button"
                role="menuitem"
              >
                {model.name}
              </button>
            ))}
          </div>
        ) : null}
      </section>
      ) : null}
    </main>
  );
}

