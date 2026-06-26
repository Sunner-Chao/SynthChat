import { useEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { Palette, SendHorizontal } from "lucide-react";
import { api, convertFileSrc } from "./lib/api";
import type { AgentRunEvent, ChatMessage, Conversation, Persona } from "./lib/types";
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
const PET_RECENT_CONVERSATION_MIRROR_INTERVAL_MS = 2400;
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
  const mirrorInitializedRef = useRef(false);
  const cloudTimerRef = useRef<number | null>(null);
  const globalLookTimerRef = useRef<number | null>(null);
  const globalLookInFlightRef = useRef(false);
  const lastLookMoveAtRef = useRef(Date.now());
  const lastLookPointRef = useRef<{ x: number; y: number } | null>(null);
  const lastSeenAssistantIdRef = useRef<string | null>(null);
  const lastSeenAssistantSignatureRef = useRef<string | null>(null);
  const lastShownAssistantSignatureRef = useRef<string | null>(null);
  const pokeCountRef = useRef(0);
  const lastPokeAtRef = useRef(0);
  const initialGreetingShownRef = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const hideTimeoutRef = useRef<number | null>(null);
  const assistantCloudDurationMsRef = useRef(DEFAULT_PET_ASSISTANT_CLOUD_DURATION_SECONDS * 1000);
  const isNearModelRef = useRef(false);
  const modelMenuOpenRef = useRef(false);
  const showInputRef = useRef(true);

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
      void refreshLatestAssistant(context.conversationId, false);
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
      void refreshLatestAssistant(context.conversationId, false);
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
      await refreshLatestAssistant(conversationId, mirrorInitializedRef.current);
      mirrorInitializedRef.current = true;
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
      const isWechat = payload.message.source === "wechat" || (payload as { source?: string }).source === "wechat";
      if (isWechat && payload.conversationId && context?.conversationId !== payload.conversationId) {
        setPetContext({
          conversationId: payload.conversationId,
          conversationTitle: null,
          personaId: null,
          personaName: null,
          agentId: null,
          updatedAt: new Date().toISOString(),
          source: "wechat"
        });
      }
      if (assistantMessageVisibleInCloud(payload.message)) showAssistantCloud(payload.message);
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

  function showAssistantCloud(message: ChatMessage, durationMs = assistantCloudDurationMsRef.current) {
    if (!assistantMessageVisibleInCloud(message)) return;
    const payload = assistantCloudPayload(message);
    if (!payload) return;
    if (payload.signature === lastShownAssistantSignatureRef.current) return;
    lastShownAssistantSignatureRef.current = payload.signature;
    lastSeenAssistantSignatureRef.current = payload.signature;
    if (message.id) {
      lastSeenAssistantIdRef.current = message.id;
    }
    showCloud(payload.text, "active", durationMs, payload.attachments.length ? payload.attachments : undefined);
    if (modelLoadedRef.current) {
      postToPet({ type: "expression", id: "开心" });
    }
  }

  async function refreshLatestAssistant(conversationId: string, showChanged = true, force = false) {
    try {
      const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_PREVIEW_CHARS);
      const assistant = latestAssistantMessage(messages);
      if (!assistant) return null;
      const payload = assistantCloudPayload(assistant);
      if (!payload) return null;
      const changed = payload.signature !== lastSeenAssistantSignatureRef.current;
      if ((showChanged && changed) || force) {
        // showAssistantCloud dedupes on the assistant payload signature, so forcing here
        // is safe — it won't re-show a bubble that is already on screen.
        showAssistantCloud(assistant);
      }
      lastSeenAssistantIdRef.current = assistant.id;
      lastSeenAssistantSignatureRef.current = payload.signature;
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

  async function waitForAssistantReply(conversationId: string, previousAssistantId: string | null) {
    const deadline = Date.now() + 120000;
    while (Date.now() < deadline) {
      try {
        const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_PREVIEW_CHARS);
        const assistant = latestAssistantMessage(messages);
        if (assistant && assistant.id !== previousAssistantId) return assistant;
      } catch (error) {
        console.error("pet wait reply failed:", error);
        return null;
      }
      await new Promise((resolve) => window.setTimeout(resolve, 800));
    }
    return null;
  }

  async function handleSubmit() {
    const text = input.trim();
    if (!text || sendingRef.current) return;
    setInput("");
    setModelMenuOpen(false);
    sendingRef.current = true;
    setSending(true);
    if (modelLoadedRef.current) {
      postToPet({ type: "expression", id: "闭眼" });
    }

    try {
      const context = await resolvePetSendContext();
      updatePetActiveContext(context);
      const previousAssistantId = lastSeenAssistantIdRef.current;
      const messages = await api.sendChatMessage({
        conversationId: context.conversationId,
        personaId: context.personaId,
        agentId: context.agentId,
        content: text,
        providerData: {
          source: "pet"
        }
      });
      const assistant = latestAssistantMessage(messages) ?? await waitForAssistantReply(context.conversationId, previousAssistantId);
      if (assistant) {
        showAssistantCloud(assistant);
      } else {
        showCloud("处理中...", "soft", 2600);
      }
    } catch (error) {
      console.error("pet send failed:", error);
      showCloud("发送失败。", "error", 3600);
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  }

  function switchModel(model: PetModel) {
    void syncPetPointerPassthrough(false);
    if (model.id === selectedModel.id) {
      modelMenuOpenRef.current = false;
      setModelMenuOpen(false);
      showCloud(`${model.name} 已经在这里。`, "soft", 1800);
      return;
    }
    selectedModelRef.current = model;
    setSelectedModel(model);
    setModelLoaded(false);
    modelMenuOpenRef.current = false;
    setModelMenuOpen(false);
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
    const viewportWidth = Math.max(1, window.innerWidth);
    const viewportHeight = Math.max(1, window.innerHeight);
    const width = Math.min(430, Math.max(292, viewportWidth - 28));
    const attachmentRows = cloudBubble?.attachments?.length ?? 0;
    const estimatedTextLines = Math.max(1, Math.ceil((cloudBubble?.text?.length ?? 0) / 26));
    const estimatedTextHeight = Math.min(144, estimatedTextLines * 22);
    const estimatedAttachmentHeight = Math.min(168, attachmentRows * 44);
    const height = Math.max(112, Math.min(238, 38 + estimatedTextHeight + estimatedAttachmentHeight));
    const fallbackLeft = Math.max(14, Math.round((viewportWidth - width) / 2));
    const fallbackTop = 12;
    if (!bounds) {
      const startX = Math.round(width * 0.54);
      const tailX = Math.round(width * 0.58);
      const tailY = height + 38;
      return {
        left: `${fallbackLeft}px`,
        top: `${fallbackTop}px`,
        width: `${width}px`,
        "--pet-cloud-tail-start-x": `${startX}px`,
        "--pet-cloud-tail-start-y": `${height - 10}px`,
        "--pet-cloud-tail-x": `${tailX}px`,
        "--pet-cloud-tail-y": `${tailY}px`,
        "--pet-cloud-tail-length": "48px",
        "--pet-cloud-tail-angle": "88deg",
        "--pet-cloud-dot-1-x": `${Math.round(startX + (tailX - startX) * 0.34)}px`,
        "--pet-cloud-dot-1-y": `${height + 6}px`,
        "--pet-cloud-dot-2-x": `${Math.round(startX + (tailX - startX) * 0.64)}px`,
        "--pet-cloud-dot-2-y": `${height + 24}px`,
        "--pet-cloud-dot-3-x": `${tailX}px`,
        "--pet-cloud-dot-3-y": `${tailY}px`
      };
    }

    const anchorX = bounds.x + bounds.width * 0.52;
    const modelTop = bounds.y;
    const headClearance = Math.max(20, bounds.height * 0.1);
    const tailGap = Math.max(18, bounds.height * 0.06);
    const speechBandBottom = Math.max(126, Math.min(176, viewportHeight * 0.34));
    let top = Math.max(8, Math.min(18, speechBandBottom - height - 12));
    const desiredLeft = anchorX - width * 0.54;
    const left = Math.min(
      Math.max(14, desiredLeft),
      Math.max(14, viewportWidth - width - 14)
    );
    let bubbleBottomAbs = top + height;
    const tailXAbs = Math.min(viewportWidth - 14, Math.max(14, anchorX));
    const desiredTailYAbs = Math.max(24, Math.min(viewportHeight - 74, modelTop - headClearance));
    if (desiredTailYAbs < bubbleBottomAbs + tailGap) {
      top = Math.max(8, desiredTailYAbs - tailGap - height);
      bubbleBottomAbs = top + height;
    }
    const tailYAbs = Math.max(bubbleBottomAbs + tailGap, desiredTailYAbs);
    const tailX = Math.min(width + 64, Math.max(-64, tailXAbs - left));
    const tailY = Math.max(height + 18, tailYAbs - top);
    const startX = Math.min(width - 46, Math.max(46, width * 0.5 + (tailX - width * 0.5) * 0.34));
    const startY = height - 12;
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
        src="/pet/index.html?v=20260626-hiyori-cloud"
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
            {cloudBubble.attachments?.map((a, i) => {
              const isImage = a.mimeType?.startsWith("image/");
              const ext = a.fileName.split(".").pop()?.toLowerCase() ?? "";
              const docIcon: Record<string, string> = { pdf: "📄", pptx: "📊", ppt: "📊", docx: "📝", doc: "📝", xlsx: "📊", xls: "📊", txt: "📃", csv: "📊" };
              return isImage ? (
                <img key={i} className="pet-cloud-attachment-img" src={convertFileSrc(a.path)} alt={a.fileName} title={a.fileName} />
              ) : (
                <span key={i} className="pet-cloud-attachment-file" title={a.path}>
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
        className={`pet-input-shell${showInput ? "" : " is-hidden"}${modelMenuOpen ? " is-menu-open" : ""}`}
        ref={inputShellRef}
        aria-label="桌宠输入"
        onFocusCapture={revealInput}
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
            title="切换模型"
            type="button"
            aria-expanded={modelMenuOpen}
            aria-label="切换模型"
          >
            <Palette size={15} strokeWidth={2.4} aria-hidden="true" />
            <span>{selectedModel.name}</span>
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
            placeholder={activeContext?.personaName ? `和 ${activeContext.personaName} 说点什么...` : "说点什么..."}
            spellCheck={false}
            type="text"
            value={input}
          />
          <button
            className="pet-input-send-button"
            disabled={!input.trim() || sending}
            onClick={() => void handleSubmit()}
            title="发送"
            type="button"
            aria-label="发送"
          >
            <SendHorizontal size={16} strokeWidth={2.5} aria-hidden="true" />
          </button>
        </div>
        {modelMenuOpen ? (
          <div className="pet-input-model-menu" ref={modelMenuRef} role="menu">
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
