import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { Cat, Palette, X } from "lucide-react";
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
const PET_HISTORY_LIMIT = 8;
const PET_PROACTIVE_IDLE_MS = 5 * 60 * 1000;

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
};

type PetCloudBubble = {
  id: string;
  text: string;
  tone: "soft" | "happy" | "active";
};

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
    createdAt: message.createdAt
  };
}

function textOnlyAssistantReply(messages: ChatMessage[]) {
  return [...messages]
    .reverse()
    .find((message) => message.role === "assistant" && message.content.trim());
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
  const [cloudBubble, setCloudBubble] = useState<PetCloudBubble | null>(null);
  const handlePetInputRef = useRef<(text: string) => void>(() => {});
  const hideUiTimerRef = useRef<number | null>(null);
  const cloudTimerRef = useRef<number | null>(null);
  const globalLookTimerRef = useRef<number | null>(null);
  const lastLookMoveAtRef = useRef(Date.now());
  const lastLookPointRef = useRef<{ x: number; y: number } | null>(null);
  const modelDragActiveRef = useRef(false);
  const modelLoadedRef = useRef(false);
  const sendingRef = useRef(false);
  const lastInteractionAtRef = useRef(Date.now());
  const pokeCountRef = useRef(0);
  const lastPokeAtRef = useRef(0);
  const controlsLockedRef = useRef(false);
  const proactiveRunningRef = useRef(false);
  const controlsVisible = uiVisible || showModelSelector || miniFocused || sending;
  const historyVisible = controlsVisible && historyEntries.length > 0;
  const shouldRenderPetLayer = !collapsed && (controlsVisible || Boolean(cloudBubble));

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
    controlsLockedRef.current = showModelSelector || miniFocused || sending;
    sendingRef.current = sending;
  }, [showModelSelector, miniFocused, sending]);

  useEffect(() => {
    activeContextRef.current = activeContext;
  }, [activeContext]);

  useEffect(() => {
    historyEntriesRef.current = historyEntries;
  }, [historyEntries]);

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
        showTransientUi();
        scheduleHideUi(3600);
      }
      lastEntryIdRef.current = entry.id;
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  // Main -> Pet real-time sync: mirror user/assistant messages from the main
  // window (or any source) into the pet bubble when they belong to the pet's
  // active conversation. Dedup by id keeps this safe alongside the synchronous
  // send-return path in handlePetInput.
  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      conversationId?: string;
      message?: ChatMessage;
    }>("synthchat-chat-event", (event) => {
      const payload = event.payload;
      const relevantTypes = ["new_message", "assistant_message", "conversation_updated"];
      if (!relevantTypes.includes(payload.type)) return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      // Only mirror messages for the conversation the pet is currently bound to.
      if (!context?.conversationId || !payload.conversationId) return;
      if (context.conversationId !== payload.conversationId) return;
      if (clearedConversationIdsRef.current.has(payload.conversationId)) return;
      if (!payload.message) return;
      const entry = chatMessageToPetBubbleEntry(payload.message);
      if (!entry) return;
      upsertHistoryEntry(entry);
      showTransientUi();
      if (payload.type === "assistant_message") {
        showCloudBubble(entry.text, "active", 5200);
        lastEntryIdRef.current = entry.id;
        if (modelLoadedRef.current) {
          postToPet({ type: "expression", id: "开心" });
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
        scheduleHideUi(1800);
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
        if (message.hovering) {
          showTransientUi();
        } else {
          scheduleHideUi();
        }
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
      clearedConversationIdsRef.current.add(conversationId);
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
    if (clearedConversationIdsRef.current.has(conversationId)) {
      return null;
    }
    try {
      const messages = await api.listMessages(conversationId, 12, PET_BUBBLE_MAX_CHARS);
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
        const messages = await api.listMessages(conversationId, 12, PET_BUBBLE_MAX_CHARS);
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
        showTransientUi();
        scheduleHideUi(3600);
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
    if (controlsLockedRef.current) return;
    clearHideUiTimer();
    hideUiTimerRef.current = window.setTimeout(() => {
      setUiVisible(false);
      hideUiTimerRef.current = null;
    }, delayMs);
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
      createdAt: new Date().toISOString()
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
      const wasCleared = clearedConversationIdsRef.current.delete(context.conversationId);
      updatePetActiveContext(context);
      const previousLastId = lastEntryIdRef.current;
      const messages = await invoke<ChatMessage[]>("send_chat_message", {
        request: {
          conversationId: context.conversationId,
          personaId: context.personaId,
          agentId: context.agentId,
          content: text.trim()
        }
      });
      const returnedEntries = (messages ?? [])
        .map(chatMessageToPetBubbleEntry)
        .filter((entry): entry is PetBubbleEntry => Boolean(entry))
        .slice(-PET_HISTORY_LIMIT);
      if (!wasCleared) {
        mergeHistoryEntries(returnedEntries);
      }
      const assistantMessage = textOnlyAssistantReply(messages ?? []);
      const assistantEntry = assistantMessage
        ? {
            id: assistantMessage.id,
            role: "assistant" as const,
            text: formatPetBubble(assistantMessage.content),
            createdAt: assistantMessage.createdAt
          }
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
    <main className={`live2d-pet-shell${collapsed ? " is-collapsed" : ""}${controlsVisible ? " is-ui-visible" : ""}${historyVisible ? " is-history-visible" : ""}`}>
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
          {(controlsVisible || showModelSelector) ? (
            <div
              className="pet-toolbar"
              onPointerEnter={showTransientUi}
              onPointerLeave={() => scheduleHideUi()}
            >
              <span className="pet-toolbar-title">
                <Cat size={14} strokeWidth={2.3} aria-hidden="true" />
                SynthPet - {selectedModel.name}
              </span>
              <button className="pet-toolbar-main" onClick={hideControls} title="隐藏控件" type="button">隐藏</button>
              <button className="pet-toolbar-main" onClick={clearPetMessages} title="清空消息" type="button">清空</button>
              <button onClick={() => setShowModelSelector(!showModelSelector)} title="切换形象" type="button" aria-label="切换形象">
                <Palette size={14} strokeWidth={2.3} aria-hidden="true" />
              </button>
              <button className="pet-close-btn" onClick={() => void petWindowAction("close")} title="关闭" type="button" aria-label="关闭">
                <X size={14} strokeWidth={2.5} aria-hidden="true" />
              </button>
            </div>
          ) : null}

          {showModelSelector ? (
            <div className="pet-model-selector">
              <div className="pet-model-selector-title">选择桌宠形象</div>
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
            </div>
          ) : null}

          {historyVisible ? (
            <section
              className="pet-chat-bubble"
              aria-live="polite"
              onPointerEnter={showTransientUi}
              onPointerLeave={() => scheduleHideUi()}
            >
              <div className="pet-chat-bubble-head">
                <span>消息</span>
                <strong>{historyEntries.length} 条</strong>
              </div>
              <div className="pet-chat-bubble-content">
                {historyEntries.map((entry) => (
                  <article className={`pet-bubble-entry is-${entry.role}`} key={entry.id}>
                    <span>{entry.role === "user" ? "我" : "对方"}</span>
                    <p>{entry.text}</p>
                  </article>
                ))}
              </div>
            </section>
          ) : null}

          {cloudBubble ? (
            <section className={`pet-cloud-bubble is-${cloudBubble.tone}`} key={cloudBubble.id} aria-live="polite">
              <span>{cloudBubble.text}</span>
            </section>
          ) : null}

          {controlsVisible ? (
            <section
              className="pet-mini-panel"
              onPointerEnter={showTransientUi}
              onPointerLeave={() => scheduleHideUi()}
            >
              <div className="pet-mini-status">
                <div className="pet-mini-status-copy">
                  <strong>{selectedModel.name}</strong>
                  <span>{status}</span>
                </div>
                <div className="pet-mini-actions">
                  <button onClick={clearPetMessages} type="button">清空</button>
                  <button onClick={hideControls} type="button">隐藏</button>
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
            </section>
          ) : null}
        </>
      ) : null}
    </main>
  );
}
