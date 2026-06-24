import { useEffect, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { Cat, Lock, Palette, RotateCcw, Unlock, X } from "lucide-react";
import { api } from "./lib/api";
import type { ChatMessage, Conversation, Persona, AgentRunEvent } from "./lib/types";
import {
  PET_ACTIVE_CONTEXT_EVENT,
  PET_ACTIVE_CONTEXT_STORAGE_KEY,
  type PetActiveContext,
  parsePetActiveContext,
  readStoredPetActiveContext,
  writeStoredPetActiveContext
} from "./lib/petContext";

const HOST_MESSAGE_SOURCE = "synthchat-pet-host";
const FRAME_MESSAGE_SOURCE = "synthchat-pet-frame";
const PET_ACTIVE_CONTEXT_SOURCE = "pet";
const PET_BUBBLE_MAX_CHARS = 1200;
const PET_HISTORY_LIMIT = 40;
const PET_PROACTIVE_IDLE_MS = 5 * 60 * 1000;
const PET_PANEL_LAYOUT_STORAGE_KEY = "synthchat.pet.panelLayouts.v1";

type PetMessage =
  | {
      source?: string;
      type?: string;
      text?: string;
      areas?: string[];
      message?: string;
      hovering?: boolean;
      screenX?: number;
      screenY?: number;
    };

const AVAILABLE_MODELS = [
  { id: "tororo", name: "Tororo", path: "/pet/model/Tororo/tororo.model3.json", greeting: "我是 Tororo，已经在桌面待机。" },
  { id: "hijiki", name: "Hijiki", path: "/pet/model/Hijiki/hijiki.model3.json", greeting: "我是 Hijiki，已经在桌面待机。" },
  { id: "mao", name: "Mao", path: "/pet/model/Mao/Mao.model3.json", greeting: "我是 Mao，已经在桌面待机。" },
  { id: "wanko", name: "Wanko", path: "/pet/model/Wanko/Wanko.model3.json", greeting: "汪汪！我是 Wanko。" },
  { id: "hiyori", name: "Hiyori", path: "/pet/model/Hiyori/Hiyori.model3.json", greeting: "你好呀！我是 Hiyori。" },
  { id: "natori", name: "Natori", path: "/pet/model/Natori/Natori.model3.json", greeting: "你好！我是夏鸟。" },
  { id: "mark", name: "Mark", path: "/pet/model/Mark/Mark.model3.json", greeting: "Hi！我是 Mark。" },
];

type PetModel = (typeof AVAILABLE_MODELS)[number];

type PetSendContext = {
  conversationId: string;
  personaId: string | null;
  personaName: string | null;
  agentId: string | null;
};

type PetBubbleEntry = {
  id: string;
  role: "user" | "assistant";
  text: string;
  createdAt: string;
  source?: string;
};

type PetCloudBubble = {
  id: string;
  text: string;
  tone: "soft" | "happy" | "active";
};

type PetPanelId = "toolbar" | "models" | "chat" | "composer" | "cloud";

type PetPanelLayout = {
  x: number;
  y: number;
  width: number;
  height: number;
  locked: boolean;
};

const PET_PANEL_IDS: PetPanelId[] = ["toolbar", "models", "chat", "composer", "cloud"];

const PET_PANEL_LIMITS: Record<PetPanelId, { minWidth: number; minHeight: number }> = {
  toolbar: { minWidth: 260, minHeight: 46 },
  models: { minWidth: 176, minHeight: 150 },
  chat: { minWidth: 220, minHeight: 180 },
  composer: { minWidth: 260, minHeight: 96 },
  cloud: { minWidth: 220, minHeight: 72 }
};

function viewportSize() {
  return {
    width: typeof window === "undefined" ? 960 : window.innerWidth,
    height: typeof window === "undefined" ? 620 : window.innerHeight
  };
}

function clampNumber(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function defaultPetPanelLayouts(): Record<PetPanelId, PetPanelLayout> {
  const viewport = viewportSize();
  const safeWidth = Math.max(320, viewport.width);
  const safeHeight = Math.max(320, viewport.height);
  const composerWidth = Math.min(360, safeWidth - 28);
  const chatWidth = Math.min(280, safeWidth - 28);
  const chatX = Math.max(14, safeWidth - chatWidth - 14);
  const cloudWidth = Math.min(420, Math.max(250, Math.round(safeWidth * 0.34)));
  return {
    toolbar: {
      x: 14,
      y: 14,
      width: Math.min(410, safeWidth - 28),
      height: 48,
      locked: false
    },
    models: {
      x: 14,
      y: 66,
      width: Math.min(210, safeWidth - 28),
      height: 250,
      locked: false
    },
    chat: {
      x: chatX,
      y: 78,
      width: chatWidth,
      height: Math.min(342, safeHeight - 168),
      locked: false
    },
    composer: {
      x: Math.max(14, Math.round((safeWidth - composerWidth) / 2)),
      y: Math.max(14, safeHeight - 118),
      width: composerWidth,
      height: 104,
      locked: false
    },
    cloud: {
      x: clampNumber(Math.round(safeWidth * 0.38), 14, Math.max(14, safeWidth - cloudWidth - 14)),
      y: clampNumber(Math.round(safeHeight * 0.15), 58, Math.max(58, safeHeight - 120)),
      width: cloudWidth,
      height: 104,
      locked: false
    }
  };
}

function clampPetPanelLayout(id: PetPanelId, layout: PetPanelLayout): PetPanelLayout {
  const viewport = viewportSize();
  const limits = PET_PANEL_LIMITS[id];
  const maxWidth = Math.max(limits.minWidth, viewport.width - 28);
  const maxHeight = Math.max(limits.minHeight, viewport.height - 28);
  const width = clampNumber(Math.round(layout.width), limits.minWidth, maxWidth);
  const height = clampNumber(Math.round(layout.height), limits.minHeight, maxHeight);
  return {
    x: clampNumber(Math.round(layout.x), 0, Math.max(0, viewport.width - width)),
    y: clampNumber(Math.round(layout.y), 0, Math.max(0, viewport.height - height)),
    width,
    height,
    locked: Boolean(layout.locked)
  };
}

function readStoredPetPanelLayouts() {
  const defaults = defaultPetPanelLayouts();
  if (typeof window === "undefined") return defaults;
  try {
    const raw = window.localStorage.getItem(PET_PANEL_LAYOUT_STORAGE_KEY);
    if (!raw) return defaults;
    const parsed = JSON.parse(raw) as Partial<Record<PetPanelId, Partial<PetPanelLayout>>>;
    const next = { ...defaults };
    for (const id of PET_PANEL_IDS) {
      const stored = parsed[id];
      if (!stored) continue;
      next[id] = clampPetPanelLayout(id, {
        ...defaults[id],
        ...stored,
        x: Number.isFinite(stored.x) ? Number(stored.x) : defaults[id].x,
        y: Number.isFinite(stored.y) ? Number(stored.y) : defaults[id].y,
        width: Number.isFinite(stored.width) ? Number(stored.width) : defaults[id].width,
        height: Number.isFinite(stored.height) ? Number(stored.height) : defaults[id].height,
        locked: Boolean(stored.locked)
      });
    }
    return next;
  } catch {
    return defaults;
  }
}

function preferredCloudPanelLayout(
  current: PetPanelLayout,
  refs: Record<PetPanelId, PetPanelLayout>
): PetPanelLayout {
  const viewport = viewportSize();
  const width = clampNumber(Math.round(current.width), PET_PANEL_LIMITS.cloud.minWidth, Math.max(PET_PANEL_LIMITS.cloud.minWidth, viewport.width - 28));
  const height = clampNumber(Math.round(current.height), PET_PANEL_LIMITS.cloud.minHeight, Math.max(PET_PANEL_LIMITS.cloud.minHeight, viewport.height - 28));
  const targetX = clampNumber(Math.round(viewport.width * 0.38), 14, Math.max(14, viewport.width - width - 14));
  const targetY = clampNumber(Math.round(viewport.height * 0.15), 58, Math.max(58, viewport.height - height - 16));
  return clampPetPanelLayout("cloud", {
    ...current,
    width,
    height,
    x: targetX,
    y: targetY
  });
}

function formatPetBubble(text: string) {
  const normalized = text.trim();
  if (!normalized) return "主窗口已经处理完成，完整内容请在主窗口查看。";
  if (normalized.length <= PET_BUBBLE_MAX_CHARS) return normalized;
  return `${normalized.slice(0, PET_BUBBLE_MAX_CHARS)}...\n（回复较长，完整内容请在主窗口查看）`;
}

function formatPetCloudText(text: string) {
  const normalized = text.trim().replace(/\s+/g, " ");
  if (!normalized) return "";
  if (normalized.length <= 110) return normalized;
  return `${normalized.slice(0, 110)}...`;
}

function chatMessageToPetBubbleEntry(message: ChatMessage): PetBubbleEntry | null {
  if (message.role !== "user" && message.role !== "assistant") return null;
  if (message.role === "user" && (message.source === "proactive" || message.source === "proactive-internal")) return null;
  if (!message.content.trim()) return null;
  return {
    id: message.id,
    role: message.role,
    text: formatPetBubble(message.content),
    createdAt: message.createdAt,
    source: message.source
  };
}

function textOnlyAssistantReply(messages: ChatMessage[]) {
  return [...messages]
    .reverse()
    .find((message) => message.role === "assistant" && message.content.trim());
}

function formatPetMessageTime(createdAt: string) {
  const date = new Date(createdAt);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function petEntrySourceLabel(source?: string) {
  if (source === "wechat") return "微信";
  if (source === "pet") return "桌宠";
  if (source?.startsWith("desktop")) return "桌面";
  return "";
}

export function PetWindow() {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [input, setInput] = useState("");
  const [status, setStatus] = useState("启动中");
  const [historyEntries, setHistoryEntries] = useState<PetBubbleEntry[]>([]);
  const historyEntriesRef = useRef<PetBubbleEntry[]>([]);
  const clearedConversationIdsRef = useRef<Set<string>>(new Set());
  const lastEntryIdRef = useRef<string | null>(null);
  const [frameLoaded, setFrameLoaded] = useState(false);
  const [modelLoaded, setModelLoaded] = useState(false);
  const [collapsed, setCollapsed] = useState(false);
  const [activeContext, setActiveContext] = useState<PetActiveContext | null>(() => readStoredPetActiveContext());
  const activeContextRef = useRef<PetActiveContext | null>(activeContext);
  const [sending, setSending] = useState(false);
  const [selectedModel, setSelectedModel] = useState(AVAILABLE_MODELS[0]);
  const [showModelSelector, setShowModelSelector] = useState(false);
  const [uiVisible, setUiVisible] = useState(false);
  const [miniFocused, setMiniFocused] = useState(false);
  const [petHovered, setPetHovered] = useState(false);
  const [cloudBubble, setCloudBubble] = useState<PetCloudBubble | null>(null);
  const [panelLayouts, setPanelLayouts] = useState<Record<PetPanelId, PetPanelLayout>>(() => readStoredPetPanelLayouts());
  const handlePetInputRef = useRef<(text: string) => void>(() => {});
  const hideUiTimerRef = useRef<number | null>(null);
  const cloudTimerRef = useRef<number | null>(null);
  const chatScrollRef = useRef<HTMLDivElement>(null);
  const panelInteractionRef = useRef<{
    id: PetPanelId;
    mode: "move" | "resize";
    startX: number;
    startY: number;
    startLayout: PetPanelLayout;
  } | null>(null);
  const globalLookTimerRef = useRef<number | null>(null);
  const lastLookMoveAtRef = useRef(Date.now());
  const lastLookPointRef = useRef<{ x: number; y: number } | null>(null);
  const modelDragActiveRef = useRef(false);
  const modelLoadedRef = useRef(false);
  const petHoveredRef = useRef(false);
  const sendingRef = useRef(false);
  const lastInteractionAtRef = useRef(Date.now());
  const pokeCountRef = useRef(0);
  const lastPokeAtRef = useRef(0);
  const controlsLockedRef = useRef(false);
  const proactiveRunningRef = useRef(false);
  const controlsVisible = uiVisible || showModelSelector || miniFocused || sending;
  const toolbarVisible = controlsVisible || panelLayouts.toolbar.locked;
  const modelSelectorVisible = showModelSelector || panelLayouts.models.locked;
  const historyVisible = (controlsVisible || panelLayouts.chat.locked) && historyEntries.length > 0;
  const composerVisible = controlsVisible || panelLayouts.composer.locked;
  const auxiliaryPanelsVisible = toolbarVisible || modelSelectorVisible || historyVisible || composerVisible;
  const cloudVisible = Boolean(cloudBubble) && !auxiliaryPanelsVisible;
  const shouldRenderPetLayer = !collapsed && (auxiliaryPanelsVisible || cloudVisible);

  // Keep ref in sync so the message listener always calls the latest version
  useEffect(() => {
    handlePetInputRef.current = handlePetInput;
  });

  useEffect(() => {
    return () => {
      clearHideUiTimer();
      clearCloudTimer();
      if (globalLookTimerRef.current !== null) {
        window.clearInterval(globalLookTimerRef.current);
        globalLookTimerRef.current = null;
      }
      stopModelDrag();
    };
  }, []);

  useEffect(() => {
    if (showModelSelector || miniFocused || sending) {
      showTransientUi();
    } else if (uiVisible) {
      scheduleHideUi();
    }
  }, [showModelSelector, miniFocused, sending]);

  useEffect(() => {
    controlsLockedRef.current = showModelSelector || miniFocused || sending || petHovered;
    sendingRef.current = sending;
  }, [showModelSelector, miniFocused, sending, petHovered]);

  useEffect(() => {
    window.localStorage.setItem(PET_PANEL_LAYOUT_STORAGE_KEY, JSON.stringify(panelLayouts));
  }, [panelLayouts]);

  useEffect(() => {
    const onPointerMove = (event: PointerEvent) => {
      const interaction = panelInteractionRef.current;
      if (!interaction) return;
      event.preventDefault();
      const dx = event.clientX - interaction.startX;
      const dy = event.clientY - interaction.startY;
      setPanelLayouts((current) => {
        const base = interaction.startLayout;
        const nextLayout = interaction.mode === "move"
          ? { ...base, x: base.x + dx, y: base.y + dy }
          : { ...base, width: base.width + dx, height: base.height + dy };
        return {
          ...current,
          [interaction.id]: clampPetPanelLayout(interaction.id, nextLayout)
        };
      });
    };
    const onPointerUp = () => {
      panelInteractionRef.current = null;
    };
    window.addEventListener("pointermove", onPointerMove);
    window.addEventListener("pointerup", onPointerUp);
    window.addEventListener("pointercancel", onPointerUp);
    return () => {
      window.removeEventListener("pointermove", onPointerMove);
      window.removeEventListener("pointerup", onPointerUp);
      window.removeEventListener("pointercancel", onPointerUp);
    };
  }, []);

  useEffect(() => {
    const onResize = () => {
      setPanelLayouts((current) => {
        const next = { ...current };
        for (const id of PET_PANEL_IDS) next[id] = clampPetPanelLayout(id, current[id]);
        return next;
      });
    };
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    activeContextRef.current = activeContext;
  }, [activeContext]);

  useEffect(() => {
    historyEntriesRef.current = historyEntries;
  }, [historyEntries]);

  useEffect(() => {
    if (!historyVisible) return;
    window.requestAnimationFrame(() => {
      const element = chatScrollRef.current;
      if (!element) return;
      element.scrollTop = element.scrollHeight;
    });
  }, [historyEntries, historyVisible]);

  useEffect(() => {
    if (!cloudBubble) return;
    setPanelLayouts((current) => {
      if (current.cloud.locked) return current;
      const nextCloud = preferredCloudPanelLayout(current.cloud, current);
      if (
        nextCloud.x === current.cloud.x
        && nextCloud.y === current.cloud.y
        && nextCloud.width === current.cloud.width
        && nextCloud.height === current.cloud.height
      ) {
        return current;
      }
      return {
        ...current,
        cloud: nextCloud
      };
    });
  }, [cloudBubble?.id]);

  useEffect(() => {
    modelLoadedRef.current = modelLoaded;
  }, [modelLoaded]);

  useEffect(() => {
    if (!activeContext?.conversationId) return;
    void loadConversationHistory(activeContext.conversationId);
  }, [activeContext?.conversationId]);

  useEffect(() => {
    if (!modelLoaded || collapsed) {
      if (globalLookTimerRef.current !== null) {
        window.clearInterval(globalLookTimerRef.current);
        globalLookTimerRef.current = null;
      }
      return;
    }
    globalLookTimerRef.current = window.setInterval(() => {
      void updateGlobalLook();
    }, 32);
    return () => {
      if (globalLookTimerRef.current !== null) {
        window.clearInterval(globalLookTimerRef.current);
        globalLookTimerRef.current = null;
      }
    };
  }, [modelLoaded, collapsed]);

  useEffect(() => {
    const timer = window.setInterval(() => {
      if (collapsed || sending || miniFocused || showModelSelector) return;
      if (Date.now() - lastInteractionAtRef.current < PET_PROACTIVE_IDLE_MS) return;
      void triggerProactiveFromPet();
    }, 30000);
    return () => window.clearInterval(timer);
  }, [collapsed, sending, miniFocused, showModelSelector]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<PetActiveContext>(PET_ACTIVE_CONTEXT_EVENT, (event) => {
      const context = parsePetActiveContext(event.payload);
      if (!context) return;
      activeContextRef.current = context;
      setActiveContext(context);
      writeStoredPetActiveContext(context);
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
      activeContextRef.current = context;
      setActiveContext(context);
    };
    window.addEventListener("storage", onStorage);
    return () => {
      if (unlisten) unlisten();
      window.removeEventListener("storage", onStorage);
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      conversationId?: string;
      message?: ChatMessage;
    }>("synthchat-pet-event", (event) => {
      const payload = event.payload;
      if (payload.type !== "proactive_message" || !payload.message) return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      if (context?.conversationId && payload.conversationId && context.conversationId !== payload.conversationId) return;
      const entry = chatMessageToPetBubbleEntry(payload.message);
      if (!entry) return;
      upsertHistoryEntry(entry);
      if (entry.role === "assistant") {
        showCloudBubble(entry.text, "active", 5600);
      }
      lastEntryIdRef.current = entry.id;
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Keep the pet message panel as a compact mirror of the active chat
  // conversation. We refresh from the canonical message list so desktop and
  // WeChat turns converge even when an event only says "conversation updated".
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      source?: string;
      personaId?: string;
      conversationId?: string;
      message?: ChatMessage;
    }>("synthchat-chat-event", (event) => {
      const payload = event.payload;
      const relevantTypes = ["processing", "new_message", "assistant_message", "conversation_updated"];
      if (!relevantTypes.includes(payload.type)) return;
      if (!payload.conversationId) return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      const isCurrentConversation = context?.conversationId === payload.conversationId;
      const shouldFollowIncomingWechat = payload.source === "wechat" && (!context?.conversationId || !isCurrentConversation);
      if (!isCurrentConversation && !shouldFollowIncomingWechat) return;
      if (shouldFollowIncomingWechat) {
        const nextContext: PetActiveContext = {
          conversationId: payload.conversationId,
          conversationTitle: null,
          personaId: payload.personaId ?? null,
          personaName: null,
          agentId: null,
          updatedAt: new Date().toISOString(),
          source: "wechat"
        };
        activeContextRef.current = nextContext;
        setActiveContext(nextContext);
        writeStoredPetActiveContext(nextContext);
      }
      void loadConversationHistory(payload.conversationId).then((latestEntry) => {
        if (!latestEntry) return;
        if (latestEntry.role === "assistant") {
          showCloudBubble(latestEntry.text, "active", 5200);
          if (modelLoadedRef.current) {
            postToPet({ type: "expression", id: "开心" });
          }
        }
      });
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
      if (!context?.conversationId || !payload.conversationId) return;
      if (context.conversationId !== payload.conversationId) return;
      
      if (payload.toolEvent) {
        if (payload.toolEvent.status === "running") {
          setStatus("调用中");
          if (modelLoadedRef.current) postToPet({ type: "expression", id: "思考" });
        } else if (payload.toolEvent.status === "canceled" || payload.toolEvent.status === "cancelled") {
          // Hidden in the pet UI; canceled tool events often represent run cleanup noise.
        } else if (!payload.toolEvent.ok) {
          setStatus("失败");
          window.setTimeout(() => setStatus("在线"), 2200);
        } else {
          setStatus("成功");
          window.setTimeout(() => setStatus("在线"), 1600);
        }
      }
      
      if (payload.state) {
        if (payload.state === "completed") {
          setStatus("在线");
        } else if (payload.state === "failed" || payload.state === "aborted") {
          setStatus("错误");
          window.setTimeout(() => setStatus("在线"), 3000);
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
    document.body.classList.add("pet-window-body");
    document.documentElement.classList.add("pet-window-html");
    return () => {
      document.body.classList.remove("pet-window-body");
      document.documentElement.classList.remove("pet-window-html");
    };
  }, []);

  useEffect(() => {
    if (collapsed) {
      void petWindowAction("collapse");
    } else {
      void petWindowAction(controlsVisible ? "expand" : "model");
    }
  }, [collapsed, controlsVisible]);

  useEffect(() => {
    const onMessage = (event: MessageEvent<PetMessage>) => {
      const message = event.data;
      if (!message || typeof message !== "object" || !("type" in message)) return;
      if (message.source !== FRAME_MESSAGE_SOURCE) return;
      if (message.type === "ready") {
        loadModel();
      }
      if (message.type === "loaded") {
        setModelLoaded(true);
        setStatus("在线");
        postToPet({ type: "hide-bubble" });
      }
      if (message.type === "input") {
        void handlePetInputRef.current(message.text ?? "");
      }
      if (message.type === "tap") {
        markInteraction();
        const now = Date.now();
        pokeCountRef.current = now - lastPokeAtRef.current < 2500 ? pokeCountRef.current + 1 : 1;
        lastPokeAtRef.current = now;
        if (modelLoadedRef.current) {
          postToPet({ type: "motion", group: "Tap", index: 0 });
        }
        showTransientUi();
      }
      if (message.type === "poke") {
        markInteraction();
        showTransientUi();
        void invoke("toggle_main_window");
        setStatus("已切换");
        window.setTimeout(() => setStatus("在线"), 1600);
        scheduleHideUi(1200);
      }
      if (message.type === "model_hover") {
        return;
      }

      if (message.type === "model_drag_start") {
        setStatus("移动中");
        void startModelDrag(message.screenX, message.screenY);
      }
      if (message.type === "model_drag_move") {
        void moveModelDrag(message.screenX, message.screenY);
      }
      if (message.type === "drag_start") {
        void petWindowAction("drag");
      }
      if (message.type === "model_drag_end") {
        stopModelDrag();
        setStatus("在线");
      }
      if (message.type === "error") {
        setStatus("模型加载失败");
        console.error(message.message);
      }
    };
    window.addEventListener("message", onMessage as EventListener);
    return () => window.removeEventListener("message", onMessage as EventListener);
  }, []);

  useEffect(() => {
    if (!frameLoaded) return;
    const timer = window.setTimeout(loadModel, 100);
    return () => window.clearTimeout(timer);
  }, [frameLoaded]);

  function postToPet(message: unknown) {
    frameRef.current?.contentWindow?.postMessage(
      { source: HOST_MESSAGE_SOURCE, ...(message as Record<string, unknown>) },
      "*"
    );
  }

  function clearPetMessages() {
    const conversationId = activeContextRef.current?.conversationId;
    if (conversationId) {
      api.listAgentRuns().then(runs => {
        const activeRun = runs.find((r: any) => r.conversationId === conversationId && (r.state === "running" || r.state === "pending"));
        if (activeRun) {
          api.abortAgentRun(activeRun.runId, "用户在桌宠清空消息").catch(console.error);
        }
      }).catch(console.error);
    }
    clearCloudTimer();
    setCloudBubble(null);
    setHistoryEntries([]);
    historyEntriesRef.current = [];
    lastEntryIdRef.current = null;
    setStatus("已清空");
    window.setTimeout(() => setStatus("在线"), 1600);
  }

  async function loadConversationHistory(conversationId: string): Promise<PetBubbleEntry | null> {
    try {
      const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_BUBBLE_MAX_CHARS);
      const entries = (messages as ChatMessage[])
        .map(chatMessageToPetBubbleEntry)
        .filter((entry): entry is PetBubbleEntry => Boolean(entry))
        .slice(-PET_HISTORY_LIMIT);
      if (entries.length > 0) {
        const nextLastId = entries.at(-1)?.id ?? "";
        const changed = nextLastId !== (lastEntryIdRef.current ?? "");
        replaceHistoryEntries(entries);
        lastEntryIdRef.current = nextLastId;
        return changed ? entries.at(-1) ?? null : null;
      }
    } catch (error) {
      console.error("桌宠历史加载失败:", error);
    }
    return null;
  }

  async function waitForAssistantBubbleReply(conversationId: string, previousLastId: string | null) {
    // Tool-using turns can run many iterations; give them room before giving up.
    const deadline = Date.now() + 120000;
    while (Date.now() < deadline) {
      try {
        const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_BUBBLE_MAX_CHARS);
        const assistant = [...messages]
          .reverse()
          .find((message) =>
            message.role === "assistant"
            && message.id !== previousLastId
            && message.content.trim()
          );
        if (assistant) {
          return chatMessageToPetBubbleEntry(assistant);
        }
      } catch (error) {
        console.error("桌宠等待回复失败:", error);
        return null;
      }
      await new Promise((resolve) => window.setTimeout(resolve, 800));
    }
    return null;
  }

  function markInteraction() {
    lastInteractionAtRef.current = Date.now();
  }

  async function triggerProactiveFromPet() {
    if (proactiveRunningRef.current) return;
    const context = activeContextRef.current ?? readStoredPetActiveContext();
    if (!context?.personaId || !context.conversationId) {
      lastInteractionAtRef.current = Date.now();
      return;
    }
    proactiveRunningRef.current = true;
    lastInteractionAtRef.current = Date.now();
    try {
      await api.triggerProactiveOnce(context.personaId);
      const latestEntry = await loadConversationHistory(context.conversationId);
      if (latestEntry) {
        if (latestEntry.role === "assistant") {
          showCloudBubble(latestEntry.text, "active", 5600);
        }
      }
    } catch (error) {
      console.error("桌宠主动消息触发失败:", error);
    } finally {
      proactiveRunningRef.current = false;
    }
  }

  function clearHideUiTimer() {
    if (hideUiTimerRef.current !== null) {
      window.clearTimeout(hideUiTimerRef.current);
      hideUiTimerRef.current = null;
    }
  }

  function clearCloudTimer() {
    if (cloudTimerRef.current !== null) {
      window.clearTimeout(cloudTimerRef.current);
      cloudTimerRef.current = null;
    }
  }

  function showCloudBubble(text: string, tone: PetCloudBubble["tone"] = "soft", durationMs = 4200) {
    const formatted = formatPetCloudText(text);
    if (!formatted) return;
    clearCloudTimer();
    setCloudBubble({
      id: `${Date.now()}-${Math.random().toString(36).slice(2)}`,
      text: formatted,
      tone
    });
    cloudTimerRef.current = window.setTimeout(() => {
      setCloudBubble(null);
      cloudTimerRef.current = null;
    }, durationMs);
  }

  function normalizeHistoryEntries(entries: PetBubbleEntry[]) {
    const next: PetBubbleEntry[] = [];
    for (const entry of entries) {
      const sameIdIndex = next.findIndex((item) => item.id === entry.id);
      if (sameIdIndex >= 0) {
        next[sameIdIndex] = entry;
        continue;
      }
      if (entry.role === "user" && !entry.id.startsWith("local-user-")) {
        const echoIndex = next.findIndex(
          (item) =>
            item.role === "user"
            && item.id.startsWith("local-user-")
            && item.text.trim() === entry.text.trim()
        );
        if (echoIndex >= 0) {
          next[echoIndex] = entry;
          continue;
        }
      }
      next.push(entry);
    }
    return next.slice(-PET_HISTORY_LIMIT);
  }

  function replaceHistoryEntries(entries: PetBubbleEntry[]) {
    const next = normalizeHistoryEntries(entries);
    historyEntriesRef.current = next;
    setHistoryEntries(next);
  }

  function mergeHistoryEntries(entries: PetBubbleEntry[]) {
    setHistoryEntries((items) => {
      const next = normalizeHistoryEntries([...items, ...entries]);
      historyEntriesRef.current = next;
      return next;
    });
  }

  function upsertHistoryEntry(entry: PetBubbleEntry) {
    mergeHistoryEntries([entry]);
  }

  function panelStyle(id: PetPanelId): CSSProperties {
    const layout = panelLayouts[id];
    return {
      left: `${layout.x}px`,
      top: `${layout.y}px`,
      width: `${layout.width}px`,
      height: `${layout.height}px`
    };
  }

  function beginPanelInteraction(
    event: ReactPointerEvent<HTMLElement>,
    id: PetPanelId,
    mode: "move" | "resize"
  ) {
    const layout = panelLayouts[id];
    if (layout.locked) return;
    event.preventDefault();
    event.stopPropagation();
    markInteraction();
    showTransientUi();
    panelInteractionRef.current = {
      id,
      mode,
      startX: event.clientX,
      startY: event.clientY,
      startLayout: layout
    };
  }

  function togglePanelLock(id: PetPanelId) {
    setPanelLayouts((current) => ({
      ...current,
      [id]: { ...current[id], locked: !current[id].locked }
    }));
  }

  function resetPanelLayout(id: PetPanelId) {
    setPanelLayouts((current) => ({
      ...current,
      [id]: defaultPetPanelLayouts()[id]
    }));
  }

  function resetAllPanelLayouts() {
    setPanelLayouts(defaultPetPanelLayouts());
  }

  function renderPanelChrome(id: PetPanelId) {
    const locked = panelLayouts[id].locked;
    return (
      <>
        <button
          className="pet-panel-icon-btn"
          onClick={() => togglePanelLock(id)}
          title={locked ? "取消固定" : "固定位置"}
          type="button"
          aria-label={locked ? "取消固定" : "固定位置"}
        >
          {locked ? <Lock size={13} strokeWidth={2.4} /> : <Unlock size={13} strokeWidth={2.4} />}
        </button>
        <button
          className="pet-panel-icon-btn"
          onClick={() => resetPanelLayout(id)}
          title="重置位置"
          type="button"
          aria-label="重置位置"
        >
          <RotateCcw size={13} strokeWidth={2.4} />
        </button>
      </>
    );
  }

  function renderResizeHandle(id: PetPanelId) {
    if (panelLayouts[id].locked) return null;
    return (
      <span
        className="pet-panel-resize"
        onPointerDown={(event) => beginPanelInteraction(event, id, "resize")}
        role="presentation"
      />
    );
  }

  async function updateGlobalLook() {
    if (!modelLoadedRef.current || collapsed) return;
    try {
      const position = await invoke<{
        x: number;
        y: number;
        screenX: number;
        screenY: number;
        screenWidth: number;
        screenHeight: number;
        clientX?: number;
        clientY?: number;
        windowWidth?: number;
        windowHeight?: number;
        windowScreenX?: number;
        windowScreenY?: number;
        scaleFactor?: number;
      }>("cursor_position");
      const currentPoint = { x: position.x, y: position.y };
      const previousPoint = lastLookPointRef.current;
      if (
        !previousPoint ||
        Math.abs(previousPoint.x - currentPoint.x) > 1 ||
        Math.abs(previousPoint.y - currentPoint.y) > 1
      ) {
        lastLookMoveAtRef.current = Date.now();
        lastLookPointRef.current = currentPoint;
        postToPet({ type: "look", ...position, instant: false });
        return;
      }

      if (
        Date.now() - lastLookMoveAtRef.current > 3000 &&
        typeof position.windowWidth === "number" &&
        typeof position.windowHeight === "number"
      ) {
        postToPet({
          type: "look",
          x: position.windowWidth / 2,
          y: position.windowHeight / 2,
          clientX: position.windowWidth / 2,
          clientY: position.windowHeight / 2,
          instant: false
        });
        lastLookMoveAtRef.current = Date.now();
      }
    } catch {
      // If the platform cannot provide global cursor coordinates, pet.js still tracks in-window movement.
    }
  }

  function showTransientUi() {
    clearHideUiTimer();
    setUiVisible(true);
  }

  function scheduleHideUi(delayMs = 1600) {
    if (controlsLockedRef.current || petHoveredRef.current) return;
    clearHideUiTimer();
    hideUiTimerRef.current = window.setTimeout(() => {
      if (controlsLockedRef.current || petHoveredRef.current) {
        hideUiTimerRef.current = null;
        return;
      }
      setUiVisible(false);
      hideUiTimerRef.current = null;
    }, delayMs);
  }

  function handlePetPointerEnter() {
    petHoveredRef.current = true;
    controlsLockedRef.current = true;
    setPetHovered(true);
    clearHideUiTimer();
  }

  function handlePetPointerLeave() {
    petHoveredRef.current = false;
    controlsLockedRef.current = showModelSelector || miniFocused || sending;
    setPetHovered(false);
    scheduleHideUi();
  }

  function hideControls() {
    clearHideUiTimer();
    setShowModelSelector(false);
    setMiniFocused(false);
    setUiVisible(false);
  }

  async function startModelDrag(screenX?: number, screenY?: number) {
    if (typeof screenX !== "number" || typeof screenY !== "number") return;
    if (modelDragActiveRef.current) return;
    try {
      await invoke("pet_window_drag", { action: "start", screenX, screenY });
      modelDragActiveRef.current = true;
    } catch (error) {
      console.error("桌宠拖动初始化失败:", error);
    }
  }

  async function moveModelDrag(screenX?: number, screenY?: number) {
    if (typeof screenX !== "number" || typeof screenY !== "number") return;
    if (!modelDragActiveRef.current) return;
    try {
      await invoke("pet_window_drag", { action: "move", screenX, screenY });
    } catch (error) {
      console.error("桌宠拖动失败:", error);
      stopModelDrag();
    }
  }

  function stopModelDrag() {
    if (!modelDragActiveRef.current) return;
    modelDragActiveRef.current = false;
    void invoke("pet_window_drag", { action: "end" }).catch((error) => {
      console.error("桌宠拖动结束失败:", error);
    });
  }

  function loadModel() {
    if (collapsed) return;
    setStatus("加载模型中");
    postToPet({ type: "load", url: selectedModel.path });
  }

  async function petWindowAction(action: "close" | "drag" | "collapse" | "expand" | "model") {
    try {
      await invoke("pet_window_action", { action });
    } catch (error) {
      setStatus("窗口控制失败");
      console.error(error);
    }
  }

  async function resolvePetSendContext(): Promise<PetSendContext> {
    const context = activeContextRef.current ?? readStoredPetActiveContext();
    const conversations = await invoke<Conversation[]>("list_conversations");
    const personas = await invoke<Persona[]>("list_personas");
    const contextConversation = context?.conversationId
      ? conversations.find((conversation) => conversation.id === context.conversationId) ?? null
      : null;
    const fallbackConversation = context?.personaId
      ? conversations.find((conversation) => conversation.personaId === context.personaId) ?? null
      : conversations[0] ?? null;
    const conversation = contextConversation ?? fallbackConversation;
    if (conversation) {
      const persona = conversation.personaId
        ? personas.find((item) => item.id === conversation.personaId) ?? null
        : null;
      return {
        conversationId: conversation.id,
        personaId: conversation.personaId ?? persona?.id ?? context?.personaId ?? null,
        personaName: persona?.name ?? context?.personaName ?? null,
        agentId: conversation.agentId ?? context?.agentId ?? null
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
      agentId: created.agentId ?? context?.agentId ?? null
    };
  }

  function updatePetActiveContext(context: PetSendContext) {
    const nextContext: PetActiveContext = {
      conversationId: context.conversationId,
      conversationTitle: activeContextRef.current?.conversationTitle ?? null,
      personaId: context.personaId,
      personaName: context.personaName,
      agentId: context.agentId,
      updatedAt: new Date().toISOString()
    };
    activeContextRef.current = nextContext;
    setActiveContext(nextContext);
    writeStoredPetActiveContext(nextContext);
    void emit(PET_ACTIVE_CONTEXT_EVENT, { ...nextContext, source: PET_ACTIVE_CONTEXT_SOURCE });
  }

  const handlePetInput = async (text: string) => {
    if (!text.trim() || sendingRef.current) return;
    markInteraction();
    sendingRef.current = true;
    setSending(true);
    upsertHistoryEntry({
      id: `local-user-${Date.now()}-${Math.random().toString(36).slice(2)}`,
      role: "user",
      text: formatPetBubble(text.trim()),
      createdAt: new Date().toISOString(),
      source: "pet"
    });
    showTransientUi();
    setStatus("思考中...");
    postToPet({ type: "hide-bubble" });
    postToPet({ type: "status", working: true });
    if (modelLoadedRef.current) {
      postToPet({ type: "expression", id: "闭眼" });
    }
    try {
      const context = await resolvePetSendContext();
      clearedConversationIdsRef.current.delete(context.conversationId);
      updatePetActiveContext(context);
      const previousLastId = lastEntryIdRef.current;
      const messages = await invoke<ChatMessage[]>("send_chat_message", {
        request: {
          conversationId: context.conversationId,
          personaId: context.personaId,
          agentId: context.agentId,
          content: text.trim(),
          providerData: {
            source: "pet"
          }
        }
      });
      const returnedEntries = (messages ?? [])
        .map(chatMessageToPetBubbleEntry)
        .filter((entry): entry is PetBubbleEntry => Boolean(entry))
        .slice(-PET_HISTORY_LIMIT);
      mergeHistoryEntries(returnedEntries);
      const assistantMessage = textOnlyAssistantReply(messages ?? []);
      const assistantEntry = assistantMessage
        ? chatMessageToPetBubbleEntry(assistantMessage)
        : await waitForAssistantBubbleReply(context.conversationId, previousLastId);
      if (assistantEntry) {
        lastEntryIdRef.current = assistantEntry.id;
        upsertHistoryEntry(assistantEntry);
        showCloudBubble(assistantEntry.text, "active", 5600);
        if (modelLoadedRef.current) {
          postToPet({ type: "expression", id: "开心" });
        }
      } else {
        setStatus("处理中");
      }
      setStatus("在线");
    } catch (error) {
      console.error("桌宠消息发送或任务执行失败:", error);
      const errStr = String(error);
      if (errStr.includes("active agent run") || errStr.includes("仍在执行")) {
        setStatus("忙碌");
      } else {
        setStatus("错误");
      }
      window.setTimeout(() => setStatus("在线"), 3000);
    } finally {
      sendingRef.current = false;
      setSending(false);
      postToPet({ type: "status", working: false });
      scheduleHideUi(2800);
    }
  };

  const send = () => {
    const text = input.trim();
    if (!text || sending) return;
    setInput("");
    void handlePetInput(text);
  };

  const switchModel = (model: PetModel) => {
    markInteraction();
    setSelectedModel(model);
    setModelLoaded(false);
    setStatus("切换模型中");
    setShowModelSelector(false);
    window.setTimeout(() => {
      setStatus("加载模型中");
      postToPet({ type: "load", url: model.path });
    }, 100);
  };

  return (
    <main
      className={`live2d-pet-shell${collapsed ? " is-collapsed" : ""}${controlsVisible ? " is-ui-visible" : ""}${historyVisible ? " is-history-visible" : ""}`}
      onPointerEnter={handlePetPointerEnter}
      onPointerLeave={handlePetPointerLeave}
    >
      {collapsed ? (
        <section className="pet-collapsed-pill">
          <button
            className="pet-collapsed-drag"
            onMouseDown={(event) => {
              if (event.button === 0) void petWindowAction("drag");
            }}
            title="拖动移动桌宠"
            type="button"
          >
            桌宠
          </button>
          <button onClick={() => setCollapsed(false)} title="展开桌宠" type="button">
            展开
          </button>
          <button
            className="pet-close-btn"
            onClick={() => void petWindowAction("close")}
            title="关闭桌宠"
            type="button"
          >
            x
          </button>
        </section>
      ) : null}

      <iframe
        className="live2d-pet-frame"
        onLoad={() => setFrameLoaded(true)}
        ref={frameRef}
        src="/pet/index.html"
        title="SynthPet Live2D"
      />

      {shouldRenderPetLayer ? (
        <>
          {toolbarVisible ? (
            <div
              className={`pet-toolbar pet-panel${panelLayouts.toolbar.locked ? " is-locked" : ""}`}
              onPointerEnter={showTransientUi}
              onPointerLeave={() => scheduleHideUi()}
              style={panelStyle("toolbar")}
            >
              <span
                className="pet-toolbar-title pet-panel-drag"
                onPointerDown={(event) => beginPanelInteraction(event, "toolbar", "move")}
              >
                <Cat size={14} strokeWidth={2.3} aria-hidden="true" />
                SynthPet - {selectedModel.name}
              </span>
              <button className="pet-toolbar-main" onClick={hideControls} title="隐藏控件" type="button">隐藏</button>
              <button className="pet-toolbar-main" onClick={clearPetMessages} title="清空消息" type="button">清空</button>
              <button className="pet-toolbar-main" onClick={resetAllPanelLayouts} title="重置布局" type="button">重置</button>
              <button onClick={() => setShowModelSelector(!showModelSelector)} title="切换形象" type="button" aria-label="切换形象">
                <Palette size={14} strokeWidth={2.3} aria-hidden="true" />
              </button>
              {renderPanelChrome("toolbar")}
              <button className="pet-close-btn" onClick={() => void petWindowAction("close")} title="关闭" type="button" aria-label="关闭">
                <X size={14} strokeWidth={2.5} aria-hidden="true" />
              </button>
              {renderResizeHandle("toolbar")}
            </div>
          ) : null}

          {modelSelectorVisible ? (
            <div
              className={`pet-model-selector pet-panel${panelLayouts.models.locked ? " is-locked" : ""}`}
              style={panelStyle("models")}
            >
              <div
                className="pet-model-selector-title pet-panel-drag"
                onPointerDown={(event) => beginPanelInteraction(event, "models", "move")}
              >
                <span>选择形象</span>
                <div className="pet-panel-head-actions">
                  {renderPanelChrome("models")}
                </div>
              </div>
              {AVAILABLE_MODELS.map(model => (
                <button
                  key={model.id}
                  className={selectedModel.id === model.id ? "is-selected" : ""}
                  onClick={() => switchModel(model)}
                  type="button"
                >
                  {model.name}
                </button>
              ))}
              {renderResizeHandle("models")}
            </div>
          ) : null}

          {historyVisible ? (
            <section
              className={`pet-chat-bubble pet-panel${panelLayouts.chat.locked ? " is-locked" : ""}`}
              aria-live="polite"
              onPointerEnter={showTransientUi}
              onPointerLeave={() => scheduleHideUi()}
              style={panelStyle("chat")}
            >
              <div
                className="pet-chat-bubble-head pet-panel-drag"
                onPointerDown={(event) => beginPanelInteraction(event, "chat", "move")}
              >
                <span>消息</span>
                <div className="pet-panel-head-actions">
                  <strong>{historyEntries.length} 条</strong>
                  {renderPanelChrome("chat")}
                </div>
              </div>
              <div className="pet-chat-bubble-content" ref={chatScrollRef}>
                {historyEntries.map((entry) => (
                  <article className={`pet-bubble-entry is-${entry.role}`} key={entry.id}>
                    <div className="pet-bubble-meta">
                      <span className="pet-bubble-role">{entry.role === "user" ? "我" : "对方"}</span>
                      <span className="pet-bubble-context">
                        {petEntrySourceLabel(entry.source) ? <em>{petEntrySourceLabel(entry.source)}</em> : null}
                        <time>{formatPetMessageTime(entry.createdAt)}</time>
                      </span>
                    </div>
                    <div className="pet-bubble-text">{entry.text}</div>
                  </article>
                ))}
              </div>
              {renderResizeHandle("chat")}
            </section>
          ) : null}

          {cloudBubble ? (
            <section
              className={`pet-cloud-bubble pet-panel is-${cloudBubble.tone}${panelLayouts.cloud.locked ? " is-locked" : ""}`}
              key={cloudBubble.id}
              aria-live="polite"
              style={panelStyle("cloud")}
            >
              <span
                className="pet-cloud-drag"
                onPointerDown={(event) => beginPanelInteraction(event, "cloud", "move")}
              >
                {cloudBubble.text}
              </span>
              <div className="pet-cloud-actions">{renderPanelChrome("cloud")}</div>
              {renderResizeHandle("cloud")}
            </section>
          ) : null}

          {composerVisible ? (
            <section
              className={`pet-mini-panel pet-panel${panelLayouts.composer.locked ? " is-locked" : ""}`}
              onPointerEnter={showTransientUi}
              onPointerLeave={() => scheduleHideUi()}
              style={panelStyle("composer")}
            >
              <div
                className="pet-mini-status pet-panel-drag"
                onPointerDown={(event) => beginPanelInteraction(event, "composer", "move")}
              >
                <div className="pet-mini-status-copy">
                  <strong>{selectedModel.name}</strong>
                  <span>{status}</span>
                </div>
                <div className="pet-mini-actions">
                  <button onClick={clearPetMessages} type="button">清空</button>
                  <button onClick={hideControls} type="button">隐藏</button>
                  {renderPanelChrome("composer")}
                </div>
              </div>
              <div className="pet-mini-input">
                <input
                  onBlur={() => setMiniFocused(false)}
                  onChange={(event) => setInput(event.target.value)}
                  onFocus={() => setMiniFocused(true)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") send();
                  }}
                  placeholder="说点什么..."
                  value={input}
                />
                <button onClick={send} type="button">发送</button>
              </div>
              {renderResizeHandle("composer")}
            </section>
          ) : null}
        </>
      ) : null}
    </main>
  );
}
