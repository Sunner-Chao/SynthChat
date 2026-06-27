import { useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { Eye, EyeOff, Menu, Palette, SendHorizontal } from "lucide-react";
import { api, convertFileSrc } from "./lib/api";
import type { AgentRunEvent, ChatMessage, Conversation, EmojiGroup, Persona } from "./lib/types";
import {
  PET_ACTIVE_CONTEXT_EVENT,
  PET_ACTIVE_CONTEXT_STORAGE_KEY,
  parsePetActiveContext,
  readStoredPetActiveContext,
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
  url?: string;
  screenX?: number;
  screenY?: number;
  x?: number;
  y?: number;
  width?: number;
  height?: number;
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

type PetDockEdge = "left" | "right";
type PetWindowMode = "model" | "orb";

type PetAssistantMirrorState = {
  messageId: string;
  signature: string;
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

function isMediaDirectiveLine(line: string) {
  const trimmed = line.trim();
  return trimmed.includes("[media attached:") || /^`?MEDIA:\s*(?:"[^"]+"|'[^']+'|`[^`]+`|.+)`?$/i.test(trimmed);
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

function stripToolDirectiveBlocks(content: string) {
  const match = /(^|\n)\s*<(?:tool_call|tool_calls|function=|function_call|function_calls|tool_result)(?:\s|>|=)/i.exec(content);
  if (!match || match.index < 0) return content;
  return content.slice(0, match.index).trimEnd();
}

function touchCloudText(count: number) {
  const variants = [
    "我在哦。",
    "有什么想问的，直接在下面说就好。",
    "我会在这里看着当前对话。"
  ];
  return variants[Math.max(0, count - 1) % variants.length];
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

function buildPetVisionContent(attachment: PetAttachment) {
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

export function PetWindow() {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const inputShellRef = useRef<HTMLElement>(null);
  const modelMenuRef = useRef<HTMLDivElement>(null);
  const activeContextRef = useRef<PetActiveContext | null>(readStoredPetActiveContext());
  const frameReadyRef = useRef(false);
  const selectedModelRef = useRef<PetModel>(
    AVAILABLE_MODELS.find((model) => model.id === "hiyori") ?? AVAILABLE_MODELS[0]
  );
  const pendingModelLoadRef = useRef<{ model: PetModel; force: boolean } | null>(null);
  const modelBoundsRef = useRef<PetModelBounds | null>(null);
  const modelDragActiveRef = useRef(false);
  const modelDragMovedRef = useRef(false);
  const modelDragTokenRef = useRef(0);
  const modelDragStartReadyRef = useRef(false);
  const modelDragLatestPointRef = useRef<PetDragPoint | null>(null);
  const modelDragMoveFrameRef = useRef<number | null>(null);
  const modelDragMoveInFlightRef = useRef(false);
  const orbDragActiveRef = useRef(false);
  const orbDragMovedRef = useRef(false);
  const orbDragStartPointRef = useRef<PetDragPoint | null>(null);
  const dockEdgeRef = useRef<PetDockEdge>("right");
  const petWindowModeRef = useRef<PetWindowMode>("model");
  const modelLoadedRef = useRef(false);
  const ignoreCursorEventsRef = useRef(false);
  const sendingRef = useRef(false);
  const cloudTimerRef = useRef<number | null>(null);
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
  const assistantCloudDurationMsRef = useRef(DEFAULT_PET_ASSISTANT_CLOUD_DURATION_SECONDS * 1000);
  const isNearModelRef = useRef(false);
  const modelMenuOpenRef = useRef(false);
  const showInputRef = useRef(true);
  const [brokenCloudImages, setBrokenCloudImages] = useState<Record<string, true>>({});
  const [emojiGroups, setEmojiGroups] = useState<EmojiGroup[]>([]);
  const [visionEnabled, setVisionEnabled] = useState(false);

  useEffect(() => {
    if (!visionEnabled) return;
    let intervalId: number;
    let isCapturing = false;

    async function tick() {
      if (isCapturing) return;
      isCapturing = true;
      try {
        const dataUrl = await invoke<string>("capture_screen_base64");
        const { mimeType, bytes } = decodePetVisionDataUrl(dataUrl);
        const saved = await api.uploadChatAttachment(
          petVisionFileName(),
          mimeType,
          Array.from(bytes)
        );
        const attachment = {
          fileName: saved.fileName,
          path: saved.path,
          mimeType: saved.mimeType
        };
        const context = await resolvePetSendContext();
        if (context.conversationId && context.personaId) {
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
          const assistant = latestAssistantMessage(messages)
            ?? await waitForAssistantReply(context.conversationId, previousAssistantState);
          if (assistant) {
            showAssistantCloud(assistant, context.conversationId);
          }
        }
      } catch (err) {
        console.error("vision error:", err);
        showCloud("视觉感知暂时看不到屏幕。", "error", 3200);
      } finally {
        isCapturing = false;
      }
    }

    void tick();
    intervalId = window.setInterval(tick, 60000);
    return () => window.clearInterval(intervalId);
  }, [visionEnabled]);

  const [input, setInput] = useState("");
  const [activeContext, setActiveContext] = useState<PetActiveContext | null>(activeContextRef.current);
  const [selectedModel, setSelectedModel] = useState<PetModel>(selectedModelRef.current);
  const [modelLoaded, setModelLoaded] = useState(false);
  const [sending, setSending] = useState(false);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [cloudBubble, setCloudBubble] = useState<PetCloudBubble | null>(null);
  const [showInput, setShowInput] = useState(true);
  const [petWindowMode, setPetWindowMode] = useState<PetWindowMode>("model");
  const [dockEdge, setDockEdge] = useState<PetDockEdge>("right");

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

  useEffect(() => {
    document.body.classList.add("pet-window-body");
    document.documentElement.classList.add("pet-window-html");
    void setPetWindowModeState("model");
    return () => {
      document.body.classList.remove("pet-window-body");
      document.documentElement.classList.remove("pet-window-html");
      clearCloudTimer();
      clearGlobalLookTimer();
      void syncPetPointerPassthrough(false);
      stopModelDrag();
    };
  }, []);

  useEffect(() => {
    activeContextRef.current = activeContext;
  }, [activeContext]);

  useEffect(() => {
    const conversationId = activeContext?.conversationId;
    if (!conversationId) return;
    void refreshLatestAssistant(conversationId, false);
  }, [activeContext?.conversationId]);

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
    }>("synthchat-pet-event", (event) => {
      const payload = event.payload;
      if ((payload.type !== "assistant_final" && payload.type !== "proactive_message") || !payload.message) return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      const hasContext = Boolean(context?.conversationId);
      const isCurrentConversation = context?.conversationId === payload.conversationId;
      const isWechat = payload.message.source === "wechat" || (payload as { source?: string }).source === "wechat";
      if (!payload.conversationId) {
        if (assistantMessageVisibleInCloud(payload.message)) showAssistantCloud(payload.message);
        return;
      }
      const shouldAdoptConversation = !isCurrentConversation && (isWechat || !hasContext);
      if (shouldAdoptConversation) {
        setPetContext({
          conversationId: payload.conversationId,
          conversationTitle: null,
          personaId: null,
          personaName: null,
          agentId: null,
          updatedAt: new Date().toISOString(),
          source: isWechat ? "wechat" : "desktop"
        });
      }
      if (!isCurrentConversation && !shouldAdoptConversation) return;
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
    }>("synthchat-chat-event", (event) => {
      // The chat stream only keeps the pet's send target/context in sync.
      // Bubble display is driven by the dedicated synthchat-pet-event path.
      const payload = event.payload;
      const relevantTypes = ["new_message", "assistant_message", "conversation_updated"];
      if (!relevantTypes.includes(payload.type) || !payload.conversationId) return;

      const context = activeContextRef.current ?? readStoredPetActiveContext();
      const isCurrentConversation = context?.conversationId === payload.conversationId;
      const eventSource = payload.source ?? payload.message?.source ?? "";
      const hasContext = Boolean(context?.conversationId);
      // Follow rules:
      // - WeChat-originated messages always follow (locked or not).
      // - When the pet has no locked context yet, follow the desktop-active
      //   conversation so the input target stays intuitive.
      const shouldFollowIncomingWechat = eventSource === "wechat" && (!hasContext || !isCurrentConversation);
      const shouldFollowWhenUnbound = !hasContext;
      const shouldFollow = shouldFollowIncomingWechat || shouldFollowWhenUnbound;

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
        const inPetUi = isPointerInPetUi(clientX, clientY);
        const isNear = overModel || inPetUi || modelMenuOpenRef.current;

        void syncPetPointerPassthrough(!isNear);

        if (isNear) {
          clearInputHideTimer();
          if (!isNearModelRef.current) {
            isNearModelRef.current = true;
            setShowInput(true);
          }
        } else {
          if (isNearModelRef.current && !modelMenuOpenRef.current) {
            isNearModelRef.current = false;
            if (hideTimeoutRef.current !== null) {
              window.clearTimeout(hideTimeoutRef.current);
            }
            hideTimeoutRef.current = window.setTimeout(() => {
              if (!modelMenuOpenRef.current) {
                inputRef.current?.blur();
                showInputRef.current = false;
                setShowInput(false);
              }
              hideTimeoutRef.current = null;
            }, 800);
          }
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
        setModelMenuOpen(false);
        if (!initialGreetingShownRef.current) {
          initialGreetingShownRef.current = true;
          window.setTimeout(() => showCloud(selectedModel.greeting, "happy", 2400), 120);
        }
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
        showCloud(touchCloudText(pokeCountRef.current), "soft", 2600);
        inputRef.current?.focus();
        return;
      }
      if (message.type === "poke") {
        showCloud("我在旁边，需要时叫我就好。", "active", 3000);
        inputRef.current?.focus();
        return;
      }
      if (message.type === "error") {
        showCloud(message.message ?? "模型加载失败。", "error", 3600);
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
    if (modelMenuOpenRef.current) return;
    isNearModelRef.current = false;
    clearInputHideTimer();
    hideTimeoutRef.current = window.setTimeout(() => {
      if (!modelMenuOpenRef.current) {
        inputRef.current?.blur();
        showInputRef.current = false;
        setShowInput(false);
      }
      hideTimeoutRef.current = null;
    }, 800);
  }

  function isPointInsidePetInput(clientX: number, clientY: number) {
    const rect = inputShellRef.current?.getBoundingClientRect();
    if (!rect) return true;
    return clientX >= rect.left && clientX <= rect.right && clientY >= rect.top && clientY <= rect.bottom;
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
    clearCloudTimer();
    setBrokenCloudImages({});
    setCloudBubble({
      id: `${Date.now()}-${Math.random().toString(36).slice(2)}`,
      text: formatted || "",
      tone,
      attachments
    });
    cloudTimerRef.current = window.setTimeout(() => {
      setCloudBubble(null);
      cloudTimerRef.current = null;
    }, durationMs);
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
    if (!assistantMessageVisibleInCloud(message)) return;
    const payload = assistantCloudPayload(message);
    if (!payload) return;
    if (!assistantChangedSinceMirror(conversationId, message, payload.signature)) return;
    rememberAssistantMirror(conversationId, message, payload.signature);
    showCloud(payload.text, "active", durationMs, payload.attachments.length ? payload.attachments : undefined);
    if (modelLoadedRef.current) {
      postToPet({ type: "expression", id: "开心" });
    }
  }

  async function refreshLatestAssistant(conversationId: string, showChanged = true) {
    if (!conversationId || activeContextRef.current?.conversationId !== conversationId) {
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
    if (modelLoadedRef.current) {
      postToPet({ type: "expression", id: "闭眼" });
    }

    try {
      const context = await resolvePetSendContext();
      updatePetActiveContext(context);
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
        showAssistantCloud(assistant, context.conversationId);
      } else {
        showCloud("处理中...", "soft", 2600);
      }
    } catch (error) {
      console.error("pet send failed:", error);
      setInput((current) => current.trim() ? current : text);
      setComposerAttachments((current) => current.length > 0 ? current : submittedAttachments);
      showCloud("发送失败。", "error", 3600);
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
    const padding = 48;
    return (
      clientX >= bounds.x - padding
      && clientX <= bounds.x + bounds.width + padding
      && clientY >= bounds.y - padding
      && clientY <= bounds.y + bounds.height + padding
    );
  }

  function normalizeCursorPosition(position: PetCursorPosition) {
    const rawClientX = typeof position.clientX === "number" ? position.clientX : Number.NaN;
    const rawClientY = typeof position.clientY === "number" ? position.clientY : Number.NaN;
    if (!Number.isFinite(rawClientX) || !Number.isFinite(rawClientY)) return null;

    const cssWidth = Math.max(1, window.innerWidth);
    const cssHeight = Math.max(1, window.innerHeight);
    const scaleX = typeof position.windowWidth === "number" && position.windowWidth > 0
      ? position.windowWidth / cssWidth
      : window.devicePixelRatio;
    const scaleY = typeof position.windowHeight === "number" && position.windowHeight > 0
      ? position.windowHeight / cssHeight
      : window.devicePixelRatio;

    return {
      clientX: scaleX > 1.01 ? rawClientX / scaleX : rawClientX,
      clientY: scaleY > 1.01 ? rawClientY / scaleY : rawClientY
    };
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
    if (
      rectContainsPoint(inputShellRef.current, clientX, clientY, 8)
      || rectContainsPoint(modelMenuRef.current, clientX, clientY, 8)
    ) {
      return true;
    }
    const element = document.elementFromPoint(clientX, clientY);
    return Boolean(element?.closest(".pet-input-shell"));
  }

  async function startModelDrag(screenX?: number, screenY?: number) {
    if (typeof screenX !== "number" || typeof screenY !== "number") return;
    if (modelDragActiveRef.current) return;
    const dragToken = ++modelDragTokenRef.current;
    modelDragActiveRef.current = true;
    modelDragMovedRef.current = false;
    modelDragStartReadyRef.current = false;
    modelDragLatestPointRef.current = { screenX, screenY };
    try {
      await invoke("pet_window_drag", { action: "start", screenX, screenY });
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
    queueModelDragMove(screenX, screenY);
  }

  async function finishModelDrag(screenX?: number, screenY?: number) {
    const latest = modelDragLatestPointRef.current;
    const endPoint = typeof screenX === "number" && typeof screenY === "number"
      ? { screenX, screenY }
      : latest;
    stopModelDrag();
    const edge = await detectDockEdge(endPoint);
    if (edge) {
      await setPetWindowModeState("orb", edge);
      return;
    }
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
    if (!modelDragStartReadyRef.current || modelDragMoveInFlightRef.current || modelDragMoveFrameRef.current !== null) {
      return;
    }
    modelDragMoveFrameRef.current = window.requestAnimationFrame(() => {
      modelDragMoveFrameRef.current = null;
      void flushModelDragMove();
    });
  }

  async function flushModelDragMove() {
    if (!modelDragActiveRef.current || !modelDragStartReadyRef.current || modelDragMoveInFlightRef.current) return;
    const point = modelDragLatestPointRef.current;
    if (!point) return;
    modelDragMoveInFlightRef.current = true;
    try {
      await invoke("pet_window_drag", { action: "move", screenX: point.screenX, screenY: point.screenY });
    } catch (error) {
      console.error("pet drag move failed:", error);
      stopModelDrag();
      return;
    } finally {
      modelDragMoveInFlightRef.current = false;
    }

    const latest = modelDragLatestPointRef.current;
    if (
      modelDragActiveRef.current
      && latest
      && (latest.screenX !== point.screenX || latest.screenY !== point.screenY)
    ) {
      queueModelDragMove(latest.screenX, latest.screenY);
    }
  }

  function resetModelDragState() {
    modelDragTokenRef.current += 1;
    modelDragActiveRef.current = false;
    modelDragStartReadyRef.current = false;
    modelDragLatestPointRef.current = null;
    modelDragMoveInFlightRef.current = false;
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
    void invoke("pet_window_drag", { action: "start", screenX: event.screenX, screenY: event.screenY }).catch((error) => {
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
    void invoke("pet_window_drag", { action: "move", screenX: event.screenX, screenY: event.screenY }).catch((error) => {
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
    <main className={`live2d-pet-shell${cloudBubble ? " is-speaking" : ""}${petWindowMode === "orb" ? " is-orb" : ""}`}>
      <iframe
        className="live2d-pet-frame"
        ref={frameRef}
        src="/pet/index.html?v=20260626-hiyori-cloud-v2"
        title="SynthPet Live2D"
      />

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
        onMouseEnter={revealInput}
        onMouseLeave={scheduleInputHide}
        onPointerDown={() => {
          revealInput();
          void syncPetPointerPassthrough(false);
        }}
      >
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
            <span>功能菜单</span>
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
            <button
              className={visionEnabled ? "is-selected" : ""}
              onClick={() => setVisionEnabled(v => !v)}
              type="button"
              role="menuitem"
              style={{ gridColumn: "1 / -1", display: "flex", alignItems: "center", justifyContent: "center", gap: "8px", height: "34px" }}
            >
              {visionEnabled ? <Eye size={16} /> : <EyeOff size={16} />}
              <span>视觉感知 {visionEnabled ? "(开启)" : "(关闭)"}</span>
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
