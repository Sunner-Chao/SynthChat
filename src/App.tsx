import { ChangeEvent, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  Bot,
  BookOpen,
  Brain,
  Camera,
  Check,
  ChevronRight,
  Compass,
  Edit3,
  Copy,
  ExternalLink,
  FolderOpen,
  Globe,
  Heart,
  Image,
  ImagePlus,
  Info,
  Pencil,
  Plus,
  MessageSquareText,
  Maximize2,
  Newspaper,
  Palette,
  PlugZap,
  Puzzle,
  Search,
  Send,
  Settings,
  Smartphone,
  Smile,
  Sparkles,
  Trash2,
  Upload,
  Wand2,
  Users,
  X
} from "lucide-react";
import { useAppStore } from "./lib/store";


import { api } from "./lib/api";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { Avatar, MenuRow } from "./components/common";
import { PersonaMemoryManager } from "./components/PersonaMemoryManager";
import { resolvePersonaAgentBinding } from "./lib/personaAgentBinding";
import { SettingsPanel } from "./panels/SettingsPanel";
import { MemoryPanel, WorldbooksPanel, PluginsPanel } from "./panels/ToolPanels";
import { McpExtensionPanel } from "./panels/McpExtensionPanel";
import { SkillsCenterPanel } from "./panels/SkillsCenterPanel";
import { AgentsManagerPanel } from "./panels/AgentsManagerPanel";
import { MomentsPanel } from "./panels/MomentsPanel";
import { PersonaPanel } from "./panels/PersonaPanel";
import { ChatExperience } from "./panels/ChatExperience";
import { EnvironmentCheck } from "./panels/EnvironmentCheck";
import {
  PET_ACTIVE_CONTEXT_EVENT,
  PET_THINKING_STATE_EVENT,
  publishPetThinkingState,
  writeStoredPetActiveContext,
  type PetActiveContext,
  type PetThinkingState
} from "./lib/petContext";
import "./styles.css";
import "./panels-beautiful.css";
import type {
  AccountConfig,
  AgentConfig,
  AgentQueuedRequest,
  AgentRunEvent,
  AppSection,
  CapabilityAdapter,
  ChatMessage,
  EmojiGroup,
  ImageProvider,
  LlmProvider,
  ManagedProcessEvent,
  McpServer,
  MomentComment,
  MomentPost,
  Persona,
  ProfileConfig,
  SkillSummary,
  ThemeConfig,
  ToolEvent,
  ToolEventEnvelope,
  WechatConfig,
  WechatQrStartResult,
  WechatQrStatusResult,
  Worldbook
} from "./lib/types";

const navItems: Array<{ id: AppSection; label: string; icon: typeof MessageSquareText }> = [
  { id: "chat", label: "聊天", icon: MessageSquareText },
  { id: "contacts", label: "通讯录", icon: Users },
  { id: "discover", label: "发现", icon: Compass },
  { id: "personas", label: "角色", icon: Bot },
  { id: "moments", label: "朋友圈", icon: Newspaper },
  { id: "memory", label: "记忆", icon: Brain },
  { id: "worldbooks", label: "世界书", icon: BookOpen },
  { id: "plugins", label: "插件", icon: Puzzle },
  { id: "mcp", label: "工具", icon: PlugZap },
  { id: "agents", label: "智能体", icon: Sparkles },
  { id: "skills", label: "技能", icon: Wand2 },
  { id: "settings", label: "设置", icon: Settings }
];

const primaryNavItems = navItems.filter((item) =>
  ["chat", "contacts", "discover", "settings"].includes(item.id)
);

function parseToolEvent(content: string): ToolEvent | null {
  try {
    const parsed = JSON.parse(content) as Partial<ToolEventEnvelope>;
    if (parsed?.type === "toolEvent" && parsed.event) return parsed.event;
  } catch {
    return null;
  }
  return null;
}

function formatTime(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function isVisibleChatEventMessage(message: ChatMessage) {
  if (message.source === "desktop-agent-error") return false;
  const providerData = message.providerData;
  const record = providerData && typeof providerData === "object" && !Array.isArray(providerData)
    ? providerData as Record<string, unknown>
    : null;
  if (
    record?.silent === true
    && (message.source === "pet-vision" || record.source === "pet-vision" || record.visibility === "pet-only")
  ) {
    return false;
  }
  return !(message.role === "user" && message.source === "proactive-internal");
}

function isMemoryWriteToolEvent(event?: ToolEvent | null) {
  if (!event || event.ok === false || event.status === "running") return false;
  const toolName = event.toolName.trim();
  return ["remember_fact", "fact_store", "manage_memory", "memory"].includes(toolName);
}

function maskSecret(value?: string | null) {
  const text = value?.trim() ?? "";
  if (!text) return "未记录";
  if (text.length <= 10) return `${text.slice(0, 2)}***`;
  return `${text.slice(0, 6)}...${text.slice(-4)}`;
}

function providerPresetLabel(id: string) {
  const labels: Record<string, string> = {
    openai: "OpenAI (GPT)",
    openaiResponses: "OpenAI Responses",
    anthropic: "Anthropic (Claude)",
    google: "Google (Gemini)",
    deepseek: "DeepSeek",
    siliconflow: "硅基流动",
    custom: "自定义"
  };
  return labels[id] ?? id;
}

function providerPresetDefaults(id: string) {
  const defaults: Record<string, { providerType: string; baseUrl: string; appendChatPath: boolean }> = {
    openai: { providerType: "openai_compatible", baseUrl: "https://api.openai.com/v1", appendChatPath: true },
    openaiResponses: { providerType: "openai_responses", baseUrl: "https://api.openai.com/v1", appendChatPath: true },
    anthropic: { providerType: "anthropic", baseUrl: "https://api.anthropic.com/v1", appendChatPath: true },
    google: { providerType: "gemini", baseUrl: "https://generativelanguage.googleapis.com/v1beta", appendChatPath: true },
    deepseek: { providerType: "openai_compatible", baseUrl: "https://api.deepseek.com", appendChatPath: true },
    siliconflow: { providerType: "openai_compatible", baseUrl: "https://api.siliconflow.cn/v1", appendChatPath: true },
    custom: { providerType: "openai_compatible", baseUrl: "", appendChatPath: true }
  };
  return defaults[id] ?? defaults.custom;
}

const WECHAT_THINKING_MIN_VISIBLE_MS = 900;
const WECHAT_REPLY_INSERT_DEFER_MS = 750;

function imageProviderTypeLabel(id: string) {
  const labels: Record<string, string> = {
    openai_image: "OpenAI Image",
    gemini_image: "Gemini Image",
    novelai: "NovelAI"
  };
  return labels[id] ?? id;
}

export function App() {
  const [envCheckDone, setEnvCheckDone] = useState(false);
  const config = useAppStore((state) => state.config);
  const activeSection = useAppStore((state) => state.activeSection);
  const setSection = useAppStore((state) => state.setSection);
  const bootstrap = useAppStore((state) => state.bootstrap);
  const refreshChatData = useAppStore((state) => state.refreshChatData);
  const refreshAgents = useAppStore((state) => state.refreshAgents);
  const refreshSkills = useAppStore((state) => state.refreshSkills);
  const refreshAgentQueue = useAppStore((state) => state.refreshAgentQueue);
  const refreshAgentRuns = useAppStore((state) => state.refreshAgentRuns);
  const refreshMemories = useAppStore((state) => state.refreshMemories);
  const setConversationProcessing = useAppStore((state) => state.setConversationProcessing);
  const incrementConversationUnread = useAppStore((state) => state.incrementConversationUnread);
  const handleAgentRunEvent = useAppStore((state) => state.handleAgentRunEvent);
  const handleManagedProcessEvent = useAppStore((state) => state.handleManagedProcessEvent);
  const upsertIncomingMessage = useAppStore((state) => state.upsertIncomingMessage);
  const refreshPersonasFromBackend = useCallback(async () => {
    const personas = await api.listPersonas();
    useAppStore.setState({ personas });
  }, []);
  const processingConversationIds = useAppStore((state) => state.processingConversationIds);
  const processingConversationCount = useAppStore((state) => state.processingConversationIds.length);
  const hasChatUnread = useAppStore((state) => Object.values(state.conversationUnreadCounts).some((count) => count > 0));
  const conversations = useAppStore((state) => state.conversations);
  const personas = useAppStore((state) => state.personas);
  const activeConversationId = useAppStore((state) => state.activeConversationId);
  const themes = useAppStore((state) => state.themes);
  const lastCountedMessageRef = useRef<Map<string, string>>(new Map());
  const processingStartedAtRef = useRef<Map<string, number>>(new Map());
  const activeWechatTurnRef = useRef<Set<string>>(new Set());
  const visibleWechatUserRef = useRef<Set<string>>(new Set());
  const deferredWechatMessagesRef = useRef<Map<string, Array<{ message: ChatMessage; personaId: string | null }>>>(new Map());
  const deferredWechatTimerRef = useRef<Map<string, number>>(new Map());
  const bridgedProcessingIdsRef = useRef<Set<string>>(new Set());

  const showConversationProcessing = useCallback((
    conversationId: string,
    personaId?: string | null,
    follow = false,
    switchSection = false
  ) => {
    if (!conversationId) return;
    processingStartedAtRef.current.set(conversationId, Date.now());
    if (switchSection) {
      setSection("chat");
    }
    setConversationProcessing(conversationId, true);
    if (follow) {
      void (async () => {
        await refreshChatData(conversationId, personaId ?? null);
        setConversationProcessing(conversationId, true);
      })();
    }
  }, [refreshChatData, setConversationProcessing, setSection]);

  const hideConversationProcessing = useCallback((conversationId: string) => {
    if (!conversationId) return;
    const startedAt = processingStartedAtRef.current.get(conversationId);
    const delay = startedAt
      ? Math.max(0, WECHAT_THINKING_MIN_VISIBLE_MS - (Date.now() - startedAt))
      : 0;
    window.setTimeout(() => {
      if (startedAt && processingStartedAtRef.current.get(conversationId) !== startedAt) return;
      processingStartedAtRef.current.delete(conversationId);
      setConversationProcessing(conversationId, false);
    }, delay);
  }, [setConversationProcessing]);

  const flushDeferredWechatMessages = useCallback((conversationId: string) => {
    const timer = deferredWechatTimerRef.current.get(conversationId);
    if (timer !== undefined) {
      window.clearTimeout(timer);
      deferredWechatTimerRef.current.delete(conversationId);
    }
    const pending = deferredWechatMessagesRef.current.get(conversationId);
    if (!pending || pending.length === 0) return;
    deferredWechatMessagesRef.current.delete(conversationId);
    pending.forEach((item) => upsertIncomingMessage(item.message));
  }, [upsertIncomingMessage]);

  const scheduleWechatFallbackRefresh = useCallback((conversationId: string, personaId?: string | null) => {
    const timer = deferredWechatTimerRef.current.get(conversationId);
    if (timer !== undefined) {
      window.clearTimeout(timer);
    }
    const nextTimer = window.setTimeout(() => {
      deferredWechatTimerRef.current.delete(conversationId);
      flushDeferredWechatMessages(conversationId);
      activeWechatTurnRef.current.delete(conversationId);
      visibleWechatUserRef.current.delete(conversationId);
      void refreshChatData(conversationId, personaId ?? null);
    }, WECHAT_REPLY_INSERT_DEFER_MS);
    deferredWechatTimerRef.current.set(conversationId, nextTimer);
  }, [flushDeferredWechatMessages, refreshChatData]);

  const deferWechatTurnMessage = useCallback((
    conversationId: string,
    personaId: string | null | undefined,
    message: ChatMessage
  ) => {
    const pending = deferredWechatMessagesRef.current.get(conversationId) ?? [];
    const entry = { message, personaId: personaId ?? null };
    const existingIndex = pending.findIndex((item) => item.message.id === message.id);
    if (existingIndex >= 0) {
      pending[existingIndex] = entry;
    } else {
      pending.push(entry);
    }
    deferredWechatMessagesRef.current.set(conversationId, pending);
    scheduleWechatFallbackRefresh(conversationId, personaId ?? null);
  }, [scheduleWechatFallbackRefresh]);

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

  useEffect(() => {
    if (!activeConversationId) return;
    const conversation = conversations.find((item) => item.id === activeConversationId);
    if (!conversation) return;
    const persona = conversation.personaId
      ? personas.find((item) => item.id === conversation.personaId) ?? null
      : null;
    const context: PetActiveContext = {
      conversationId: conversation.id,
      conversationTitle: conversation.title,
      personaId: conversation.personaId ?? persona?.id ?? null,
      personaName: persona?.name ?? null,
      agentId: conversation.agentId ?? persona?.agentId ?? null,
      updatedAt: new Date().toISOString(),
      source: "main"
    };
    writeStoredPetActiveContext(context);
    void emit(PET_ACTIVE_CONTEXT_EVENT, context);
  }, [activeConversationId, conversations, personas]);

  useEffect(() => {
    const previous = bridgedProcessingIdsRef.current;
    const next = new Set(processingConversationIds);
    next.forEach((conversationId) => {
      if (previous.has(conversationId)) return;
      const conversation = conversations.find((item) => item.id === conversationId);
      const payload: PetThinkingState = {
        source: "desktop",
        personaId: conversation?.personaId ?? null,
        conversationId,
        thinking: true,
        updatedAt: new Date().toISOString()
      };
      publishPetThinkingState(payload);
      void emit(PET_THINKING_STATE_EVENT, payload).catch(() => undefined);
      void emitTo("pet", PET_THINKING_STATE_EVENT, payload).catch(() => undefined);
      void emitTo("pet", "synthchat-pet-event", {
        type: "thinking_started",
        source: payload.source,
        personaId: payload.personaId,
        conversationId
      }).catch(() => undefined);
    });
    previous.forEach((conversationId) => {
      if (next.has(conversationId)) return;
      // The desktop chat row owns the exact hide timing, including its
      // minimum-visible delay and streaming overlap. Avoid clearing the pet
      // bubble here before ChatExperience publishes desktop-ui=false.
    });
    bridgedProcessingIdsRef.current = next;
  }, [conversations, processingConversationIds]);

  useEffect(() => {
    const tick = async () => {
      const due = await api.tickScheduledAgentJobs();
      if (due.length > 0) {
        await refreshChatData(null, null);
      }
    };
    void tick();
    const timer = window.setInterval(() => {
      void tick();
    }, 60_000);
    return () => window.clearInterval(timer);
  }, [refreshChatData]);

  useEffect(() => {
    if (activeSection !== "contacts") return;
    const timer = window.setInterval(() => {
      if (processingConversationCount > 0) return;
      void refreshChatData(null, null);
    }, 5000);
    return () => window.clearInterval(timer);
  }, [activeSection, processingConversationCount, refreshChatData]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      source?: string;
      personaId?: string;
      conversationId?: string;
      message?: ChatMessage;
      delta?: string;
      isLast?: boolean;
    }>("synthchat-chat-event", (event) => {
      const payload = event.payload;
      const eventSource = payload.source ?? "";
      const messageSource = payload.message?.source ?? eventSource;
      const isWechatEvent = eventSource === "wechat" || payload.message?.source === "wechat";
      const isActiveWechatTurn = Boolean(
        payload.conversationId && activeWechatTurnRef.current.has(payload.conversationId)
      );
      const isWechatTurnEvent = isWechatEvent || isActiveWechatTurn;
      const externalSource =
        messageSource !== "desktop"
        && messageSource !== "desktop-control"
        && messageSource !== "proactive-internal";

      // Authoritative thinking lifecycle: the backend emits turn_started /
      // turn_finished for EVERY user-facing turn (desktop, pet, wechat,
      // proactive), in order, from the single hub (run_chat_turn). The desktop
      // drives its "thinking" UI solely from this pair and auto-follows the
      // turn's conversation, so timing is uniform across all sources.
      if (payload.type === "turn_started" && payload.conversationId) {
        if (isWechatEvent) {
          const timer = deferredWechatTimerRef.current.get(payload.conversationId);
          if (timer !== undefined) {
            window.clearTimeout(timer);
            deferredWechatTimerRef.current.delete(payload.conversationId);
          }
          deferredWechatMessagesRef.current.delete(payload.conversationId);
          activeWechatTurnRef.current.add(payload.conversationId);
        }
        const shouldSwitchSection = externalSource && messageSource !== "pet" && messageSource !== "wechat";
        showConversationProcessing(
          payload.conversationId,
          payload.personaId ?? null,
          true,
          shouldSwitchSection
        );
        return;
      }
      if (payload.type === "turn_finished" && payload.conversationId) {
        hideConversationProcessing(payload.conversationId);
        if (isWechatEvent) {
          if (
            payload.message
            && isVisibleChatEventMessage(payload.message)
            && (payload.message.role === "assistant" || payload.message.role === "tool")
          ) {
            if (visibleWechatUserRef.current.has(payload.conversationId)) {
              upsertIncomingMessage(payload.message);
            } else {
              deferWechatTurnMessage(payload.conversationId, payload.personaId, payload.message);
            }
          }
          scheduleWechatFallbackRefresh(payload.conversationId, payload.personaId ?? null);
          return;
        }
        if (
          payload.message
          && isVisibleChatEventMessage(payload.message)
          && (payload.message.role === "assistant" || payload.message.role === "tool")
        ) {
          upsertIncomingMessage(payload.message, { final: payload.message.role === "assistant" });
        }
        window.setTimeout(() => {
          void refreshChatData(payload.conversationId ?? null, payload.personaId ?? null);
        }, 180);
        return;
      }
      const isMessageEvent =
        payload.type === "assistant_stream"
        || payload.type === "assistant_thinking_stream"
        || payload.type === "new_message"
        || payload.type === "tool_message"
        || payload.type === "assistant_message";
      const isVisibleMessageEvent =
        Boolean(payload.conversationId && payload.message && isVisibleChatEventMessage(payload.message));
      let appliedInlineVisibleMessage = false;
      if (isMessageEvent && payload.conversationId && payload.message && isVisibleMessageEvent) {
        if (payload.message.role === "user" && messageSource === "desktop") {
          // Desktop sends its own user messages and handles optimistic UI locally.
          return;
        }
        const isWechatUserMessage = payload.message.role === "user" && isWechatEvent;
        if (isWechatUserMessage) {
          visibleWechatUserRef.current.add(payload.conversationId);
        }
        const shouldDeferWechatTurnMessage =
          isWechatTurnEvent
          && !visibleWechatUserRef.current.has(payload.conversationId)
          && (payload.message.role === "assistant" || payload.message.role === "tool");
        if (shouldDeferWechatTurnMessage) {
          deferWechatTurnMessage(payload.conversationId, payload.personaId, payload.message);
          return;
        }
        if (payload.type === "assistant_message" || payload.type === "new_message") {
          const state = useAppStore.getState();
          const shouldMarkUnread =
            payload.conversationId !== state.activeConversationId
            || state.activeSection !== "chat";
          const messageKey = payload.message.id.trim() || `${payload.message.createdAt}:${payload.message.content.length}`;
          const previousKey = lastCountedMessageRef.current.get(payload.conversationId);
          if (shouldMarkUnread && previousKey !== messageKey) {
            lastCountedMessageRef.current.set(payload.conversationId, messageKey);
            incrementConversationUnread(payload.conversationId);
          }
        }
        upsertIncomingMessage(payload.message, {
          streaming: (
            (payload.type === "assistant_stream" || payload.type === "assistant_thinking_stream")
            && payload.message.role === "assistant"
            && !payload.isLast
          ),
          final: (
            (payload.type === "assistant_message" || (payload.type === "assistant_stream" && payload.isLast))
            && payload.message.role === "assistant"
          )
        });
        appliedInlineVisibleMessage = true;
        if (isWechatUserMessage) {
          flushDeferredWechatMessages(payload.conversationId);
        }
      }
      if (payload.type === "assistant_stream" || payload.type === "assistant_thinking_stream") {
        return;
      }
      if (payload.type === "tool_message") {
        return;
      }
      if (
        appliedInlineVisibleMessage
        && (messageSource === "pet" || payload.message?.role === "assistant")
        && (payload.type === "new_message" || payload.type === "assistant_message")
      ) {
        // The live message has already been inserted. A same-tick refresh can
        // read a stale backend snapshot and temporarily wipe or downgrade the
        // streaming bubble before the final event catches up.
        return;
      }
      if (
        payload.type === "conversation_updated"
        && payload.conversationId
        && isWechatEvent
        && !visibleWechatUserRef.current.has(payload.conversationId)
      ) {
        scheduleWechatFallbackRefresh(payload.conversationId, payload.personaId ?? null);
        return;
      }
      if (payload.type === "new_message" || payload.type === "assistant_message" || payload.type === "conversation_updated") {
        void refreshChatData(payload.conversationId ?? null, payload.personaId ?? null);
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
      deferredWechatTimerRef.current.forEach((timer) => window.clearTimeout(timer));
      deferredWechatTimerRef.current.clear();
    };
  }, [
    deferWechatTurnMessage,
    flushDeferredWechatMessages,
    hideConversationProcessing,
    incrementConversationUnread,
    refreshChatData,
    scheduleWechatFallbackRefresh,
    setConversationProcessing,
    showConversationProcessing,
    upsertIncomingMessage
  ]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{
      type: string;
      personaId?: string;
      persona?: Persona;
    }>("synthchat-persona-event", (event) => {
      const payload = event.payload;
      if (payload.type !== "persona_updated") return;
      if (payload.persona) {
        useAppStore.setState((state) => ({
          personas: [payload.persona!, ...state.personas.filter((item) => item.id !== payload.persona!.id)]
            .sort((a, b) => a.name.localeCompare(b.name))
        }));
        return;
      }
      void refreshPersonasFromBackend();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refreshPersonasFromBackend]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<AgentRunEvent>("synthchat-agent-run-event", (event) => {
      const payload = event.payload;
      handleAgentRunEvent(payload);
      if (payload.phase === "memory_write_observed" || isMemoryWriteToolEvent(payload.toolEvent)) {
        void refreshMemories();
      }
      // Processing visibility is owned by the authoritative turn_started /
      // turn_finished lifecycle (see synthchat-chat-event handler).
      if (payload.message) {
        upsertIncomingMessage(payload.message);
      }
      void refreshAgentQueue();
      if (payload.state === "completed" || payload.state === "failed" || payload.state === "aborted") {
        void Promise.all([
          refreshAgentRuns(),
          refreshChatData(payload.conversationId, payload.personaId)
        ]);
      }
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [handleAgentRunEvent, refreshAgentQueue, refreshAgentRuns, refreshChatData, refreshMemories, setConversationProcessing, upsertIncomingMessage]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ type?: string; item?: AgentQueuedRequest | null }>("synthchat-agent-queue-event", (event) => {
      const item = event.payload.item;
      if (item) {
        useAppStore.setState((state) => ({
          agentQueue: [item, ...state.agentQueue.filter((entry) => entry.id !== item.id)]
            .sort((a, b) => a.createdAt.localeCompare(b.createdAt))
        }));
      }
      void refreshAgentQueue();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refreshAgentQueue]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ type: string; conversationId?: string | null }>("synthchat-agent-goal-event", (event) => {
      void refreshAgentQueue();
      void refreshAgentRuns();
      void refreshChatData(event.payload.conversationId ?? null, null);
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refreshAgentQueue, refreshAgentRuns, refreshChatData]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<ManagedProcessEvent>("synthchat-managed-process-event", (event) => {
      const payload = event.payload;
      handleManagedProcessEvent(payload);
      if (payload.conversationId) {
        void refreshChatData(payload.conversationId, null);
      }
      void Promise.all([refreshAgentRuns(), refreshAgentQueue()]);
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [handleManagedProcessEvent, refreshAgentQueue, refreshAgentRuns, refreshChatData]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen("synthchat-skills-changed", () => {
      void refreshSkills();
      void refreshAgents();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [refreshAgents, refreshSkills]);

  useEffect(() => {
    const styleId = "synthchat-active-theme";
    let style = document.getElementById(styleId) as HTMLStyleElement | null;
    if (!style) {
      style = document.createElement("style");
      style.id = styleId;
      document.head.appendChild(style);
    }
    style.textContent = themes.filter((theme) => theme.active).map((theme) => theme.css).join("\n");
  }, [themes]);

  useEffect(() => {
    const rawMode = themes[0]?.mode ?? "light";
    const resolveMode = (m: string): "light" | "dark" => {
      if (m === "auto") {
        return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
      }
      return m as "light" | "dark";
    };
    document.documentElement.setAttribute("data-theme", resolveMode(rawMode));

    if (rawMode !== "auto") return;

    const mql = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => {
      document.documentElement.setAttribute("data-theme", resolveMode("auto"));
    };
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }, [themes]);

  const skipEnvCheck = config?.chat?.skipEnvCheck ?? false;
  if (!envCheckDone && !skipEnvCheck) {
    return <EnvironmentCheck onComplete={() => setEnvCheckDone(true)} />;
  }

  return (
    <main className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">
            <Sparkles size={18} />
          </div>
          <div>
            <strong>SynthChat</strong>
            <span>clean rebuild</span>
          </div>
        </div>

        <nav className="nav-list" aria-label="主导航">
          {primaryNavItems.map((item) => {
            const Icon = item.icon;
            return (
              <button
                className={activeSection === item.id ? "nav-item active" : "nav-item"}
                key={item.id}
                onClick={() => setSection(item.id)}
                type="button"
                title={item.label}
              >
                <Icon size={20} strokeWidth={activeSection === item.id ? 2.2 : 1.8} />
                <span>{item.label}</span>
                {item.id === "chat" && hasChatUnread ? <i aria-hidden="true" className="nav-item-dot" /> : null}
              </button>
            );
          })}
        </nav>

      </aside>

      <section className="workspace">
        <Header />
        <Content />
      </section>
    </main>
  );
}

function Header() {
  const activeSection = useAppStore((state) => state.activeSection);
  const title = navItems.find((item) => item.id === activeSection)?.label ?? "SynthChat";
  return (
    <header className="workspace-header">
      <div>
        <h1>{title}</h1>
      </div>
    </header>
  );
}

function Content() {
  const activeSection = useAppStore((state) => state.activeSection);
  return (
    <div className="workspace-content">
      <div
        aria-hidden={activeSection !== "chat"}
        className={activeSection === "chat" ? "workspace-panel active" : "workspace-panel hidden"}
      >
        <ChatExperience />
      </div>
      {activeSection !== "chat" ? (
        <div className="workspace-panel active" key={activeSection}>
          <ActivePanel section={activeSection} />
        </div>
      ) : null}
    </div>
  );
}

function ActivePanel({ section }: { section: AppSection }) {
  if (section === "contacts") return <ContactsPanel />;
  if (section === "discover") return <DiscoverPanel />;
  if (section === "moments") return <MomentsPanel />;
  if (section === "personas") return <PersonaPanel />;
  if (section === "mcp") return <McpExtensionPanel />;
  if (section === "settings") return <SettingsPanel />;
  if (section === "memory") return <MemoryPanel />;
  if (section === "worldbooks") return <WorldbooksPanel />;
  if (section === "plugins") return <PluginsPanel />;
  if (section === "agents") return <AgentsManagerPanel />;
  if (section === "skills") return <SkillsCenterPanel />;
  return null;
}

function ContactsPanel() {
  const {
    personas,
    accounts,
    config,
    memories,
    setSection,
    saveConfig,
    savePersona,
    refreshMemories,
    deleteMemory,
    openPersonaConversation,
    linkWechatAccount,
    unlinkWechatAccount,
    refreshAccounts
  } = useAppStore();
  const agents = useAppStore((state) => state.agents);
  const llmProviders = useAppStore((state) => state.llmProviders);
  const [query, setQuery] = useState("");
  const [selectedPersonaId, setSelectedPersonaId] = useState(personas[0]?.id ?? "");
  const [detailView, setDetailView] = useState<"profile" | "memory">("profile");
  const [showWechatSheet, setShowWechatSheet] = useState(false);
  const [pollStatus, setPollStatus] = useState("");
  const personaBindings = useMemo(
    () => new Map(personas.map((persona) => [persona.id, resolvePersonaAgentBinding(persona, agents, llmProviders)])),
    [agents, llmProviders, personas]
  );
  const visiblePersonas = personas;
  useEffect(() => {
    if (visiblePersonas.some((persona) => persona.id === selectedPersonaId)) return;
    setSelectedPersonaId(visiblePersonas[0]?.id ?? "");
  }, [selectedPersonaId, visiblePersonas]);
  const filtered = visiblePersonas.filter((persona) =>
    (personaBindings.get(persona.id)?.searchText ?? `${persona.name} ${persona.id}`.toLowerCase()).includes(query.toLowerCase())
  );
  const selectedPersona = visiblePersonas.find((p) => p.id === selectedPersonaId) ?? visiblePersonas[0] ?? null;
  const linkedAccount = selectedPersona ? accounts.find((account) => account.linkedPersona === selectedPersona.id) : null;
  const selectedBinding = selectedPersona ? personaBindings.get(selectedPersona.id) : null;
  const selectedMemories = selectedPersona ? memories.filter((memory) => memory.personaId === selectedPersona.id) : [];
  const persistentMemories = selectedMemories.filter((memory) => (memory.target ?? "memory") !== "session");
  const sessionMemories = selectedMemories.filter((memory) => (memory.target ?? "memory") === "session");
  const saveChatConfig = async (patch: Partial<NonNullable<typeof config>["chat"]>) => {
    if (!config) return;
    await saveConfig({ ...config, chat: { ...config.chat, ...patch } });
  };
  const updatePersonaMemory = async (memory: NonNullable<Persona["memory"]>) => {
    if (!selectedPersona) return;
    await savePersona({ ...selectedPersona, memory });
  };
  const removeMemoryEntry = async (memoryId: string) => {
    await deleteMemory(memoryId);
    if (selectedPersona) await refreshMemories(selectedPersona.id);
  };
  useEffect(() => {
    if (!selectedPersona) return;
    void refreshMemories(selectedPersona.id);
  }, [refreshMemories, selectedPersona?.id]);
  useEffect(() => {
    setDetailView("profile");
  }, [selectedPersonaId]);
  const syncLinkedWechat = async () => {
    if (!linkedAccount) return;
    setPollStatus("正在同步微信消息...");
    try {
      const result = await api.wechatPollOnce(linkedAccount.id);
      await refreshAccounts();
      const conversationId = result.processed.find((item) => item.conversationId)?.conversationId;
      if (conversationId) {
        const store = useAppStore.getState();
        store.setSection("chat");
        store.setConversationProcessing(conversationId, true);
        void store.refreshChatData(conversationId, selectedPersona?.id ?? null).then(() => {
          store.setConversationProcessing(conversationId, true);
          window.setTimeout(() => {
            useAppStore.getState().setConversationProcessing(conversationId, false);
          }, WECHAT_THINKING_MIN_VISIBLE_MS);
        });
      }
      setPollStatus(result.receivedCount
        ? `收到 ${result.receivedCount} 条，已处理 ${result.processed.length} 条，跳过 ${result.skippedCount} 条`
        : "没有新的微信消息");
    } catch (error) {
      setPollStatus(String(error));
    }
  };
  return (
    <section className="tab-split">
      <aside className="side-panel tab-list-panel">
        <div className="side-title">
          <h3>通讯录</h3>
          <div className="title-actions">
            <button title="导入角色" type="button"><Upload size={16} /></button>
            <button onClick={() => setSection("personas")} title="新建角色" type="button"><Plus size={16} /></button>
          </div>
        </div>
        <div className="search-bar">
          <Search size={17} />
          <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder="搜索" />
        </div>
        <div className="card-list">
          {filtered.map((persona) => {
            const binding = personaBindings.get(persona.id);
            return (
              <button
                className={persona.id === selectedPersonaId ? "contact-row active" : "contact-row"}
                key={persona.id}
                onClick={() => setSelectedPersonaId(persona.id)}
                type="button"
              >
                <Avatar name={persona.name} src={persona.avatarPath ? api.assetUrl(persona.avatarPath) : ""} />
                <span>
                  <strong>{persona.name}</strong>
                  <small>{binding?.infoText ?? "未配置服务商"}</small>
                </span>
              </button>
            );
          })}
        </div>
      </aside>
      <article className="primary-panel">
        <div className="panel-title">
          <span>Contacts</span>
          <strong>{selectedPersona?.name ?? "角色详情"}</strong>
        </div>
        {selectedPersona ? (
          detailView === "memory" ? (
            <div className="contact-memory-detail">
              <div className="panel-title action-title contact-memory-title">
                <button className="icon-only-btn" onClick={() => setDetailView("profile")} title="返回资料" type="button">
                  <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
                </button>
                <div className="panel-title-text"><span>{selectedPersona.name}</span><strong>记忆管理</strong></div>
              </div>
              <PersonaMemoryManager
                bindingModel={selectedBinding?.model ?? ""}
                bindingProviderName={selectedBinding?.providerName ?? ""}
                chatConfig={config?.chat ?? null}
                onDeleteMemory={removeMemoryEntry}
                onRefresh={() => void refreshMemories(selectedPersona.id)}
                onSaveChatConfig={saveChatConfig}
                onUpdateMemory={updatePersonaMemory}
                onViewAll={() => setSection("memory")}
                persistentMemories={persistentMemories}
                personaMemory={selectedPersona.memory}
                sessionMemories={sessionMemories}
              />
            </div>
          ) : (
            <div className="profile-detail">
              <Avatar name={selectedPersona.name} src={selectedPersona.avatarPath ? api.assetUrl(selectedPersona.avatarPath) : ""} size="large" />
              <h2>{selectedPersona.name}</h2>
              <p className="persona-id-text">{selectedPersona.id}</p>
              <div className="menu-card">
                <MenuRow
                  icon={MessageSquareText}
                  label="发消息"
                  value="进入会话"
                  onClick={() => {
                    void openPersonaConversation(selectedPersona.id).then(() => setSection("chat"));
                  }}
                />
                <MenuRow
                  icon={Smartphone}
                  label="链接微信"
                  value={linkedAccount ? (linkedAccount.note || "已链接") : "未链接"}
                  onClick={() => setShowWechatSheet(true)}
                  iconColor="green"
                />
                <MenuRow icon={Brain} label="记忆管理" value="长期与会话" onClick={() => setDetailView("memory")} />
                <MenuRow icon={BookOpen} label="世界书" value="绑定与查看" onClick={() => setSection("worldbooks")} />
                <MenuRow icon={Edit3} label="编辑角色" value="人设与模型" onClick={() => setSection("personas")} />
              </div>
            {pollStatus ? <p className="form-hint">{pollStatus}</p> : null}
            {showWechatSheet ? (
              <div className="sheet-backdrop" onClick={() => setShowWechatSheet(false)}>
                <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
                  <div className="sheet-title">链接微信账号</div>
                  {accounts.length === 0 ? (
                    <p className="form-hint">暂无已登录微信账号，请先到设置 &gt; 微信账号扫码登录。</p>
                  ) : (
                    accounts.map((account) => {
                      const occupied = account.linkedPersona && account.linkedPersona !== selectedPersona.id;
                      const occupiedPersona = personas.find((persona) => persona.id === account.linkedPersona);
                      const isDisabled = Boolean(occupied) || !account.online;
                      return (
                        <button
                          className="sheet-item"
                          disabled={isDisabled}
                          key={account.id}
                          onClick={() => {
                            void linkWechatAccount(selectedPersona.id, account.id).then(() => setShowWechatSheet(false));
                          }}
                          type="button"
                        >
                          <span>{account.note || account.id}</span>
                          <small className={occupied ? "status-text-muted" : account.online ? "status-text-online" : "status-text-muted"}>
                            {occupied ? `已链接到 ${occupiedPersona?.name ?? account.linkedPersona}` : account.online ? "在线" : "离线"}
                          </small>
                        </button>
                      );
                    })
                  )}
                  <div style={{ display: "flex", gap: "12px", padding: "8px 0" }}>
                    {linkedAccount ? (
                      <button
                        className="sheet-cancel btn-danger-text"
                        onClick={() => {
                          void unlinkWechatAccount(selectedPersona.id).then(() => setShowWechatSheet(false));
                        }}
                        type="button"
                        style={{ flex: 1 }}
                      >
                        断开
                      </button>
                    ) : null}
                    <button className="sheet-cancel" onClick={() => setShowWechatSheet(false)} type="button" style={{ flex: 1 }}>取消</button>
                  </div>
                </div>
              </div>
            ) : null}
            </div>
          )
        ) : (
          <div className="empty-state">
            <Users size={36} />
            <h2>还没有角色</h2>
            <button onClick={() => setSection("personas")} type="button">新建角色</button>
          </div>
        )}
      </article>
    </section>
  );
}

function DiscoverPanel() {
  const { moments, worldbooks, setSection } = useAppStore();
  const entries: Array<{ id: AppSection; title: string; meta: string; icon: typeof Newspaper }> = [
    { id: "moments", title: "朋友圈", meta: `${moments.length} 条动态`, icon: Camera },
    { id: "worldbooks", title: "世界书", meta: `${worldbooks.length} 本世界书`, icon: BookOpen }
  ];
  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <div className="panel-title-text"><span>Discover</span><strong>发现</strong></div>
      </div>
      <div className="menu-card" style={{ margin: "0 16px" }}>
        {entries.map((entry) => {
          const Icon = entry.icon;
          return (
            <MenuRow key={entry.id} icon={Icon} label={entry.title} value={entry.meta} onClick={() => setSection(entry.id)} />
          );
        })}
      </div>
    </section>
  );
}

function PlaceholderPanel({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <section className="primary-panel">
      <div className="empty-state">
        <PlugZap size={36} />
        <h2>{title}</h2>
        <p>{subtitle}</p>
      </div>
    </section>
  );
}
