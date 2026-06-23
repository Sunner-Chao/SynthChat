import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
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
const PET_BUBBLE_HISTORY_LIMIT = 8;
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
  { id: "mao", name: "猫咪", path: "/pet/model/Mao/Mao.model3.json", greeting: "桌宠已待机，输入框可以直接和我说话。" },
  { id: "wanko", name: "Wanko (小狗)", path: "/pet/model/Wanko/Wanko.model3.json", greeting: "汪汪！我是 Wanko，很高兴见到你喵~" },
  { id: "tororo", name: "Tororo (猫)", path: "/pet/model/Tororo/tororo.model3.json", greeting: "我是 Tororo，已经在桌面待机。" },
  { id: "hijiki", name: "Hijiki (猫)", path: "/pet/model/Hijiki/hijiki.model3.json", greeting: "我是 Hijiki，最近的对话会显示在右侧气泡里。" },
  { id: "hiyori", name: "Hiyori (可爱女孩)", path: "/pet/model/Hiyori/Hiyori.model3.json", greeting: "你好呀！我是 Hiyori~" },
  { id: "natori", name: "Natori (夏鸟)", path: "/pet/model/Natori/Natori.model3.json", greeting: "你好！我是夏鸟，请多指教~" },
  { id: "mark", name: "Mark (标记)", path: "/pet/model/Mark/Mark.model3.json", greeting: "Hi！我是 Mark！" },
];

type PetModel = (typeof AVAILABLE_MODELS)[number];

type PetSendContext = {
  conversationId: string;
  personaId: string | null;
  personaName: string | null;
  agentId: string | null;
};

type PetBubbleRole = "user" | "assistant" | "status";

type PetBubbleEntry = {
  id: string;
  role: PetBubbleRole;
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

function createPetBubbleEntry(role: PetBubbleRole, text: string, createdAt = new Date().toISOString()): PetBubbleEntry {
  return {
    id: `${role}-${createdAt}-${Math.random().toString(36).slice(2)}`,
    role,
    text: formatPetBubble(text),
    createdAt
  };
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

function petBubbleRoleLabel(role: PetBubbleRole) {
  if (role === "user") return "你";
  if (role === "assistant") return "回复";
  return "状态";
}

function formatPetBubbleTime(createdAt: string) {
  const date = new Date(createdAt);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function textOnlyAssistantReply(messages: ChatMessage[]) {
  return [...messages]
    .reverse()
    .find((message) => message.role === "assistant" && message.content.trim());
}

type PetBubbleSegment =
  | { kind: "text"; value: string }
  | { kind: "image"; path: string; mimeType: string }
  | { kind: "file"; path: string; mimeType: string };

const MEDIA_MARKER = /\[media attached:\s*(?:"([^"]+)"|`([^`]+)`|([^\]\(]+?))\s*(?:\(([^)]+)\))?\]/gi;

function fileNameFromPath(path: string) {
  return path.split(/[\\/]/).pop() || path;
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

function parsePetBubbleSegments(text: string): PetBubbleSegment[] {
  const segments: PetBubbleSegment[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  MEDIA_MARKER.lastIndex = 0;
  while ((match = MEDIA_MARKER.exec(text)) !== null) {
    if (match.index > lastIndex) {
      segments.push({ kind: "text", value: text.slice(lastIndex, match.index) });
    }
    const path = (match[1] || match[2] || match[3] || "").trim();
    const mimeType = (match[4] || (isImagePath(path) ? imageMimeType(path) : "application/octet-stream")).trim();
    if (path) {
      segments.push({
        kind: isImagePath(path) || mimeType.startsWith("image/") ? "image" : "file",
        path,
        mimeType
      });
    }
    lastIndex = match.index + match[0].length;
  }
  if (lastIndex < text.length) {
    segments.push({ kind: "text", value: text.slice(lastIndex) });
  }
  return segments;
}

function PetBubbleEntryContent({ text }: { text: string }) {
  const segments = parsePetBubbleSegments(text);
  return (
    <>
      {segments.map((segment, index) => {
        if (segment.kind === "image") {
          return (
            <button
              className="pet-bubble-image"
              key={`${segment.path}-${index}`}
              onClick={() => void api.openLocalFile(segment.path)}
              title="打开图片"
              type="button"
            >
              <img
                alt={fileNameFromPath(segment.path)}
                loading="lazy"
                onLoad={() => window.dispatchEvent(new Event("synthchat-pet-bubble-resize"))}
                src={api.assetUrl(segment.path)}
              />
            </button>
          );
        }
        if (segment.kind === "file") {
          return (
            <button
              className="pet-bubble-file"
              key={`${segment.path}-${index}`}
              onClick={() => void api.openLocalFile(segment.path)}
              title="打开文件"
              type="button"
            >
              <span>文件</span>
              <strong>{fileNameFromPath(segment.path)}</strong>
              <small>{segment.mimeType || "application/octet-stream"}</small>
            </button>
          );
        }
        return segment.value.split(/\n{2,}/).map((block, blockIndex) => {
          const trimmed = block.trim();
          if (!trimmed) return null;
          return (
            <p className="pet-bubble-text" key={`${index}-${blockIndex}`}>
              {trimmed}
            </p>
          );
        });
      })}
    </>
  );
}

function PetBubbleContent({ entries }: { entries: PetBubbleEntry[] }) {
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const scrollToBottom = () => {
      window.requestAnimationFrame(() => {
        const element = scrollRef.current;
        if (!element) return;
        element.scrollTop = element.scrollHeight;
      });
    };
    scrollToBottom();
    window.addEventListener("synthchat-pet-bubble-resize", scrollToBottom);
    return () => window.removeEventListener("synthchat-pet-bubble-resize", scrollToBottom);
  }, [entries]);

  return (
    <div className="pet-chat-bubble-content" ref={scrollRef}>
      {entries.map((entry) => (
        <article className={`pet-bubble-entry is-${entry.role}`} key={entry.id}>
          <div className="pet-bubble-entry-meta">
            <span>{petBubbleRoleLabel(entry.role)}</span>
            <time>{formatPetBubbleTime(entry.createdAt)}</time>
          </div>
          <PetBubbleEntryContent text={entry.text} />
        </article>
      ))}
    </div>
  );
}

export function PetWindow() {
  const frameRef = useRef<HTMLIFrameElement>(null);
  const [input, setInput] = useState("");
  const [status, setStatus] = useState("启动中");
  const [bubbleEntries, setBubbleEntries] = useState<PetBubbleEntry[]>(() => [
    createPetBubbleEntry("status", AVAILABLE_MODELS[0].greeting)
  ]);
  const bubbleEntriesRef = useRef<PetBubbleEntry[]>(bubbleEntries);
  const clearedConversationIdsRef = useRef<Set<string>>(new Set());
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
  const shouldRenderPetControls = !collapsed && (controlsVisible || Boolean(cloudBubble));

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
    bubbleEntriesRef.current = bubbleEntries;
  }, [bubbleEntries]);

  useEffect(() => {
    activeContextRef.current = activeContext;
  }, [activeContext]);

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
      appendBubbleEntry(entry);
      showCloudBubble(entry.text, "active", 5600);
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
      appendBubbleEntry(entry);
      if (payload.type === "assistant_message") {
        showCloudBubble(entry.text, "active", 5200);
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
          appendBubbleEntry(createPetBubbleEntry("status", `正在调用工具: ${payload.toolEvent.title || payload.toolEvent.toolName}`));
          if (modelLoadedRef.current) postToPet({ type: "expression", id: "思考" });
        } else if (!payload.toolEvent.ok) {
          appendBubbleEntry(createPetBubbleEntry("status", `工具调用失败: ${payload.toolEvent.title || payload.toolEvent.toolName}`));
        } else {
          appendBubbleEntry(createPetBubbleEntry("status", `工具调用完成: ${payload.toolEvent.title || payload.toolEvent.toolName}`));
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
        const combo = pokeCountRef.current > 0 && pokeCountRef.current % 3 === 0;
        if (combo) {
          appendStatusBubble("连续触摸已记录。");
          showCloudBubble("欸，连续点我三次了，是有什么悄悄话吗？", "happy", 3600);
        } else {
          showCloudBubble(touchCloudText(pokeCountRef.current), "soft", 2800);
        }
        if (modelLoadedRef.current) {
          postToPet({ type: "motion", group: "Tap", index: 0 });
        }
        showTransientUi();
        scheduleHideUi(combo ? 2800 : 1800);
      }
      if (message.type === "poke") {
        markInteraction();
        showTransientUi();
        void invoke("show_main_window");
        appendStatusBubble("已打开主窗口。");
        showCloudBubble("主窗口打开啦，我会在旁边继续陪着。", "active", 3000);
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
        showCloudBubble("带我换个位置吗？我会跟上。", "active", 2400);
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
        showCloudBubble("我先停在这里。", "soft", 2200);
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

  function appendBubbleEntry(entry: PetBubbleEntry) {
    setBubbleEntries((items) => {
      // Dedup by id so the synchronous send-return and the event-bridge path
      // cannot show the same message twice.
      if (items.some((item) => item.id === entry.id)) {
        return items.map((item) => (item.id === entry.id ? entry : item));
      }
      // Reconcile an optimistic user echo (local-user- prefix) with the real
      // backend message: when a backend user message arrives with the same
      // text, replace the local echo in place instead of appending a duplicate.
      if (entry.role === "user" && !entry.id.startsWith("local-user-")) {
        const echoIndex = items.findIndex(
          (item) =>
            item.role === "user"
            && item.id.startsWith("local-user-")
            && item.text.trim() === entry.text.trim()
        );
        if (echoIndex >= 0) {
          const next = items.slice();
          next[echoIndex] = entry;
          return next;
        }
      }
      return [...items, entry].slice(-PET_BUBBLE_HISTORY_LIMIT);
    });
  }

  function appendStatusBubble(text: string) {
    appendBubbleEntry(createPetBubbleEntry("status", text));
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
    setBubbleEntries([createPetBubbleEntry("status", "桌宠消息已清空。")]);
    showCloudBubble("消息记录清空啦。", "soft", 2200);
  }

  async function loadConversationHistory(conversationId: string): Promise<PetBubbleEntry | null> {
    if (clearedConversationIdsRef.current.has(conversationId)) {
      return null;
    }
    try {
      const messages = await api.listMessages(conversationId, PET_BUBBLE_HISTORY_LIMIT * 2, PET_BUBBLE_MAX_CHARS);
      const entries = (messages as ChatMessage[])
        .map(chatMessageToPetBubbleEntry)
        .filter((entry): entry is PetBubbleEntry => Boolean(entry))
        .slice(-PET_BUBBLE_HISTORY_LIMIT);
      if (entries.length > 0) {
        const previousLastId = bubbleEntriesRef.current.at(-1)?.id ?? "";
        const nextLastId = entries.at(-1)?.id ?? "";
        setBubbleEntries(entries);
        return nextLastId !== previousLastId ? entries.at(-1) ?? null : null;
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
        const messages = await api.listMessages(conversationId, PET_BUBBLE_HISTORY_LIMIT * 2, PET_BUBBLE_MAX_CHARS);
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

  function touchCloudText(count: number) {
    const variants = [
      "我在哦。轻轻碰一下就能叫醒我。",
      "刚刚碰到我啦，我会把最近对话记在右边。",
      "如果想打开主窗口，双击我就可以。"
    ];
    return variants[Math.max(0, count - 1) % variants.length];
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
    // Optimistic user echo with a local- prefix so the real backend user
    // message (arriving via the synthchat-chat-event bridge) reconciles in
    // place instead of duplicating.
    appendBubbleEntry({
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
      updatePetActiveContext(context);
      const previousLastId = bubbleEntriesRef.current.at(-1)?.id ?? null;
      const messages = await invoke<ChatMessage[]>("send_chat_message", {
        request: {
          conversationId: context.conversationId,
          personaId: context.personaId,
          agentId: context.agentId,
          content: text.trim()
        }
      });
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
        appendBubbleEntry(assistantEntry);
        if (modelLoadedRef.current) {
          postToPet({ type: "expression", id: "开心" });
        }
      } else {
        appendStatusBubble("我还在处理这条消息，请稍等一下。");
      }
      setStatus("在线");
    } catch (error) {
      console.error("桌宠消息发送或任务执行失败:", error);
      const errStr = String(error);
      if (errStr.includes("active agent run") || errStr.includes("仍在执行")) {
        appendStatusBubble("代理仍在执行上一个任务，请在主窗口查看或将其终止。");
      } else {
        appendStatusBubble(`任务异常中断 (${errStr})，完整信息请在主窗口查看。`);
      }
      setStatus("normal");
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
    appendStatusBubble(model.greeting);
    showCloudBubble(model.greeting, "happy", 3200);
    setShowModelSelector(false);
    window.setTimeout(() => {
      setStatus("加载模型中");
      postToPet({ type: "load", url: model.path });
    }, 100);
  };

  return (
    <main className={`live2d-pet-shell${collapsed ? " is-collapsed" : ""}${controlsVisible ? " is-ui-visible" : ""}`}>
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

      {shouldRenderPetControls ? (
        <>
          <div
            className="pet-toolbar"
            onPointerEnter={showTransientUi}
            onPointerLeave={() => scheduleHideUi()}
          >
            <span className="pet-toolbar-title">
              SynthPet - {selectedModel.name}
            </span>
            <button className="pet-toolbar-main" onClick={hideControls} title="最小化隐藏控件" type="button">隐藏</button>
            <button className="pet-toolbar-main" onClick={clearPetMessages} title="清空桌宠消息" type="button">清空</button>
            <button onClick={() => setShowModelSelector(!showModelSelector)} title="切换模型" type="button">🎭</button>
            <button className="pet-close-btn" onClick={() => void petWindowAction("close")} title="关闭" type="button">x</button>
          </div>

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

          {cloudBubble ? (
            <section className={`pet-cloud-bubble is-${cloudBubble.tone}`} key={cloudBubble.id} aria-live="polite">
              <span>{cloudBubble.text}</span>
            </section>
          ) : null}

          {controlsVisible ? (
            <>
              <section
                className="pet-chat-bubble"
                aria-live="polite"
                onPointerEnter={showTransientUi}
                onPointerLeave={() => scheduleHideUi()}
              >
                <PetBubbleContent entries={bubbleEntries} />
              </section>

              <section
                className="pet-mini-panel"
                onPointerEnter={showTransientUi}
                onPointerLeave={() => scheduleHideUi()}
              >
                <div className="pet-mini-status">
                  <strong>桌宠</strong>
                  <div className="pet-mini-actions">
                    <button onClick={clearPetMessages} type="button">清空</button>
                    <button onClick={hideControls} type="button">隐藏</button>
                    <span>{status}</span>
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
            </>
          ) : null}
        </>
      ) : null}
    </main>
  );
}
