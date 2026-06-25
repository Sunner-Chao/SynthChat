import { useEffect, useRef, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { emit, listen } from "@tauri-apps/api/event";
import { Palette, SendHorizontal } from "lucide-react";
import { api } from "./lib/api";
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
const PET_GLOBAL_LOOK_INTERVAL_MS = 32;
const PET_GLOBAL_LOOK_IDLE_MS = 3000;

const AVAILABLE_MODELS = [
  { id: "tororo", name: "Tororo", path: "/pet/model/Tororo/tororo.model3.json", greeting: "Tororo 到啦。" },
  { id: "hijiki", name: "Hijiki", path: "/pet/model/Hijiki/hijiki.model3.json", greeting: "Hijiki 换好了。" },
  { id: "mao", name: "Mao", path: "/pet/model/Mao/Mao.model3.json", greeting: "Mao 在这里。" },
  { id: "wanko", name: "Wanko", path: "/pet/model/Wanko/Wanko.model3.json", greeting: "汪，我换好啦。" },
  { id: "hiyori", name: "Hiyori", path: "/pet/model/Hiyori/Hiyori.model3.json", greeting: "Hiyori 来了。" },
  { id: "natori", name: "Natori", path: "/pet/model/Natori/Natori.model3.json", greeting: "夏鸟已经就位。" },
  { id: "mark", name: "Mark", path: "/pet/model/Mark/Mark.model3.json", greeting: "Mark is ready." }
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

function latestAssistantMessage(messages: ChatMessage[]) {
  return [...messages]
    .reverse()
    .find((message) => message.role === "assistant" && message.content.trim());
}

function messageToCloudText(message: ChatMessage | null | undefined) {
  if (!message || message.role !== "assistant") return "";
  return formatCloudText(message.content);
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
  const cloudBubbleRef = useRef<HTMLElement>(null);
  const activeContextRef = useRef<PetActiveContext | null>(readStoredPetActiveContext());
  const frameReadyRef = useRef(false);
  const selectedModelRef = useRef<PetModel>(
    AVAILABLE_MODELS.find((model) => model.id === "mao") ?? AVAILABLE_MODELS[0]
  );
  const pendingModelLoadRef = useRef<{ model: PetModel; force: boolean } | null>(null);
  const modelBoundsRef = useRef<PetModelBounds | null>(null);
  const modelDragActiveRef = useRef(false);
  const modelDragTokenRef = useRef(0);
  const modelDragStartReadyRef = useRef(false);
  const modelDragLatestPointRef = useRef<PetDragPoint | null>(null);
  const modelDragMoveFrameRef = useRef<number | null>(null);
  const modelDragMoveInFlightRef = useRef(false);
  const modelLoadedRef = useRef(false);
  const ignoreCursorEventsRef = useRef(false);
  const sendingRef = useRef(false);
  const mirrorInitializedRef = useRef(false);
  const cloudTimerRef = useRef<number | null>(null);
  const globalLookTimerRef = useRef<number | null>(null);
  const globalLookInFlightRef = useRef(false);
  const lastLookMoveAtRef = useRef(Date.now());
  const lastLookPointRef = useRef<{ x: number; y: number } | null>(null);
  const lastAssistantIdRef = useRef<string | null>(null);
  const pokeCountRef = useRef(0);
  const lastPokeAtRef = useRef(0);
  const initialGreetingShownRef = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const hideTimeoutRef = useRef<number | null>(null);
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

  useEffect(() => {
    document.body.classList.add("pet-window-body");
    document.documentElement.classList.add("pet-window-html");
    void petWindowAction("expand");
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
    }>("synthchat-pet-event", (event) => {
      const payload = event.payload;
      if (payload.type !== "proactive_message" || !payload.message) return;
      const context = activeContextRef.current ?? readStoredPetActiveContext();
      if (context?.conversationId && payload.conversationId && context.conversationId !== payload.conversationId) return;
      showAssistantCloud(payload.message, 5600);
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
      const payload = event.payload;
      const relevantTypes = ["new_message", "assistant_message", "conversation_updated"];
      if (!relevantTypes.includes(payload.type) || !payload.conversationId) return;

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
        setPetContext(nextContext);
      }

      if (payload.message?.role === "assistant") {
        showAssistantCloud(payload.message, 5200);
        return;
      }
      void refreshLatestAssistant(payload.conversationId, true);
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
      if (!context?.conversationId || context.conversationId !== payload.conversationId) return;
      if (payload.state === "failed" || payload.state === "aborted") {
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
        showCloud("带我换个位置吗？我会跟上。", "active", 2200);
        void startModelDrag(message.screenX, message.screenY);
        return;
      }
      if (message.type === "model_drag_move") {
        void moveModelDrag(message.screenX, message.screenY);
        return;
      }
      if (message.type === "model_drag_end") {
        stopModelDrag();
        showCloud("我先停在这里。", "soft", 2000);
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

  function showCloud(text: string, tone: PetCloudBubble["tone"] = "soft", durationMs = 4200) {
    const formatted = formatCloudText(text);
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

  function showAssistantCloud(message: ChatMessage, durationMs = 5200) {
    const text = messageToCloudText(message);
    if (!text) return;
    lastAssistantIdRef.current = message.id;
    showCloud(text, "active", durationMs);
    if (modelLoadedRef.current) {
      postToPet({ type: "expression", id: "开心" });
    }
  }

  async function refreshLatestAssistant(conversationId: string, showChanged: boolean) {
    try {
      const messages = await api.listMessages(conversationId, PET_HISTORY_LIMIT, PET_PREVIEW_CHARS);
      const assistant = latestAssistantMessage(messages);
      if (!assistant) return null;
      const changed = assistant.id !== lastAssistantIdRef.current;
      lastAssistantIdRef.current = assistant.id;
      if (showChanged && changed) {
        showAssistantCloud(assistant, 5200);
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
      const previousAssistantId = lastAssistantIdRef.current;
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
        showAssistantCloud(assistant, 5600);
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

  async function petWindowAction(action: "expand" | "model" | "drag") {
    try {
      await invoke("pet_window_action", { action, edge: null });
    } catch (error) {
      console.error("pet window action failed:", error);
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
    if (!showInputRef.current && !modelMenuOpenRef.current) {
      return Boolean(
        rectContainsPoint(modelMenuRef.current, clientX, clientY, 8)
        || rectContainsPoint(cloudBubbleRef.current, clientX, clientY, 4)
      );
    }
    if (
      rectContainsPoint(inputShellRef.current, clientX, clientY, 8)
      || rectContainsPoint(modelMenuRef.current, clientX, clientY, 8)
      || rectContainsPoint(cloudBubbleRef.current, clientX, clientY, 4)
    ) {
      return true;
    }
    const element = document.elementFromPoint(clientX, clientY);
    return Boolean(element?.closest(".pet-input-shell, .pet-cloud-bubble"));
  }

  async function startModelDrag(screenX?: number, screenY?: number) {
    if (typeof screenX !== "number" || typeof screenY !== "number") return;
    if (modelDragActiveRef.current) return;
    const dragToken = ++modelDragTokenRef.current;
    modelDragActiveRef.current = true;
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
    queueModelDragMove(screenX, screenY);
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

  function cloudStyle(): PetCloudStyle {
    const bounds = modelBoundsRef.current;
    const viewportWidth = Math.max(1, window.innerWidth);
    const viewportHeight = Math.max(1, window.innerHeight);
    const width = Math.min(430, Math.max(292, viewportWidth - 28));
    const height = 112;
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
    const gap = Math.max(12, bounds.height * 0.05);
    const speechBandBottom = Math.max(126, Math.min(176, viewportHeight * 0.34));
    let top = Math.max(8, Math.min(18, speechBandBottom - height - 12));
    const desiredLeft = anchorX - width * 0.54;
    const left = Math.min(
      Math.max(14, desiredLeft),
      Math.max(14, viewportWidth - width - 14)
    );
    const bubbleBottomAbs = top + height;
    let tailXAbs = Math.min(viewportWidth - 14, Math.max(14, anchorX));
    let tailYAbs = Math.min(viewportHeight - 74, Math.max(modelTop - gap, modelTop));
    if (tailYAbs < bubbleBottomAbs) {
      top = Math.max(8, modelTop - gap - height);
      tailYAbs = Math.max(top + height, modelTop - gap);
    }
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
    <main className={`live2d-pet-shell${cloudBubble ? " is-speaking" : ""}`}>
      <iframe
        className="live2d-pet-frame"
        ref={frameRef}
        src="/pet/index.html"
        title="SynthPet Live2D"
      />

      <section className={`pet-speech-area${cloudBubble ? " has-bubble" : ""}`} aria-live="polite">
        {cloudBubble ? (
          <section
            className={`pet-cloud-bubble is-${cloudBubble.tone}`}
            key={cloudBubble.id}
            ref={cloudBubbleRef}
            style={cloudStyle()}
          >
            <span className="pet-cloud-text" title={cloudBubble.text}>{cloudBubble.text}</span>
            <span className="pet-cloud-tail" aria-hidden="true">
              <span />
              <span />
              <span />
            </span>
          </section>
        ) : null}
      </section>

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
    </main>
  );
}
