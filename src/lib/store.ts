import { create } from "zustand";
import { api } from "./api";
import type {
  AgentDefinition,
  AgentQueuedRequest,
  AgentRunEvent,
  AgentRunRecord,
  AppConfig,
  AgentConfig,
  AccountConfig,
  AppSection,
  BrowserProvider,
  CapabilityAdapter,
  ChatMessage,
  Conversation,
  EmojiGroup,
  EnhancedSkillSummary,
  ImageProvider,
  LlmProvider,
  MarketplaceSkill,
  ManagedProcessEvent,
  MemoryEntry,
  McpCallResult,
  McpListToolsResult,
  McpServer,
  MomentPost,
  Persona,
  PluginSummary,
  ProactiveStatus,
  ProfileConfig,
  SearchProvider,
  SkillBundle,
  ThemeConfig,
  ToolEvent,
  SkillSummary,
  VideoProvider,
  VisionProvider,
  Worldbook
} from "./types";

const DEFAULT_UI_MESSAGE_LIMIT = 180;
const MIN_UI_MESSAGE_LIMIT = 40;
const MAX_UI_MESSAGE_LIMIT = 1000;
const DEFAULT_UI_MESSAGE_PREVIEW_CHARS = 12_000;

// Module-level ref for pending settings view (not in React state to avoid batching delays)
let pendingSettingsViewRef: string | null = null;
export function consumePendingSettingsView(): string | null {
  const v = pendingSettingsViewRef;
  pendingSettingsViewRef = null;
  return v;
}

function uiMessageLimit(config: AppConfig | null) {
  const configured = config?.chat.uiMessageLimit ?? DEFAULT_UI_MESSAGE_LIMIT;
  if (!Number.isFinite(configured)) return DEFAULT_UI_MESSAGE_LIMIT;
  return Math.min(MAX_UI_MESSAGE_LIMIT, Math.max(MIN_UI_MESSAGE_LIMIT, Math.floor(configured)));
}

function uiMessagePreviewChars(config: AppConfig | null) {
  const configured = config?.chat.uiMessagePreviewChars ?? DEFAULT_UI_MESSAGE_PREVIEW_CHARS;
  if (!Number.isFinite(configured)) return DEFAULT_UI_MESSAGE_PREVIEW_CHARS;
  return Math.min(100_000, Math.max(2_000, Math.floor(configured)));
}

function limitMessages(messages: ChatMessage[], limit: number) {
  return messages.length > limit ? messages.slice(-limit) : messages;
}

function sameConversations(left: Conversation[], right: Conversation[]) {
  return left.length === right.length && left.every((item, index) => {
    const other = right[index];
    return Boolean(other)
      && item.id === other.id
      && item.title === other.title
      && item.updatedAt === other.updatedAt
      && item.lastMessage === other.lastMessage
      && item.personaId === other.personaId
      && item.wechatAccountId === other.wechatAccountId;
  });
}

function sameMessages(left: ChatMessage[], right: ChatMessage[]) {
  return left.length === right.length && left.every((item, index) => {
    const other = right[index];
    return Boolean(other)
      && item.id === other.id
      && item.role === other.role
      && item.content === other.content
      && item.createdAt === other.createdAt
      && item.source === other.source
      && item.accountId === other.accountId;
  });
}

function parseNewConversationCommand(content: string): string | null | undefined {
  const body = content
    .trim()
    .replace(/^[/／]/, "")
    .trim();
  if (!body) return undefined;
  const parts = body.split(/\s+/);
  const command = parts.shift()?.toLowerCase();
  if (command !== "new" && command !== "reset") return undefined;
  const title = parts
    .filter((part) => !["--confirm", "confirm", "确认", "--yes", "-y", "now"].includes(part.toLowerCase()))
    .join(" ")
    .trim();
  return title || null;
}

function parseSessionSwitchCommand(content: string): string | undefined {
  const body = content
    .trim()
    .replace(/^[/／]/, "")
    .trim();
  if (!body) return undefined;
  const parts = body.split(/\s+/);
  const command = parts.shift()?.toLowerCase();
  if (!["sessions", "session", "conversations"].includes(command ?? "")) return undefined;
  const selector = parts.join(" ").trim();
  if (!selector || selector.toLowerCase() === "list") return undefined;
  return selector;
}

function sameToolRun(left: AgentRunEvent["toolEvent"], right: AgentRunEvent["toolEvent"]) {
  if (!left || !right) return false;
  if (left.callId && right.callId) return left.callId === right.callId;
  if (left.callId || right.callId) return false;
  return left.serverId === right.serverId
    && left?.toolName === right?.toolName
    && left?.title === right?.title;
}

function mergeToolEventList(previousEvents: ToolEvent[], incoming: ToolEvent | null | undefined) {
  if (!incoming) return previousEvents;
  const events = [...previousEvents];
  const runningIndex = events.findIndex((item) => item.status === "running" && sameToolRun(item, incoming));
  if (runningIndex >= 0 && incoming.status !== "running") {
    events[runningIndex] = incoming;
    return events;
  }
  const duplicateIndex = events.findIndex((item) =>
    sameToolRun(item, incoming)
    && item.status === incoming.status
    && item.elapsedMs === incoming.elapsedMs
    && item.summary === incoming.summary
  );
  if (duplicateIndex >= 0) {
    events[duplicateIndex] = incoming;
    return events;
  }
  return [...events, incoming];
}

function mergeToolRunEvents(previous: AgentRunEvent | undefined, event: AgentRunEvent) {
  return mergeToolEventList(previous?.accumulatedToolEvents ?? [], event.toolEvent);
}

function mergeRunPhases(previous: AgentRunEvent | undefined, event: AgentRunEvent) {
  const phases = [...(previous?.accumulatedPhases ?? [])];
  if (!event.phase) return phases;
  const next = { phase: event.phase, detail: event.detail ?? null, updatedAt: event.updatedAt };
  const last = phases[phases.length - 1];
  if (last && last.phase === next.phase && JSON.stringify(last.detail ?? null) === JSON.stringify(next.detail ?? null)) {
    phases[phases.length - 1] = next;
    return phases;
  }
  phases.push(next);
  return phases.slice(-24);
}

interface AppState {
  activeSection: AppSection;
  previousSection: AppSection | null;
  config: AppConfig | null;
  conversations: Conversation[];
  activeConversationId: string | null;
  messages: ChatMessage[];
  processingConversationIds: string[];
  conversationUnreadCounts: Record<string, number>;
  llmProviders: LlmProvider[];
  profile: ProfileConfig;
  accounts: AccountConfig[];
  imageProviders: ImageProvider[];
  videoProviders: VideoProvider[];
  searchProviders: SearchProvider[];
  visionProviders: VisionProvider[];
  browserProviders: BrowserProvider[];
  themes: ThemeConfig[];
  emojiGroups: EmojiGroup[];
  memories: MemoryEntry[];
  worldbooks: Worldbook[];
  agents: AgentDefinition[];
  agentQueue: AgentQueuedRequest[];
  agentRuns: AgentRunRecord[];
  activeAgentRuns: Record<string, AgentRunEvent>;
  managedProcessEvents: ManagedProcessEvent[];
  plugins: PluginSummary[];
  skillBundles: SkillBundle[];
  marketplaceSkills: MarketplaceSkill[];
  selectedAgentId: string | null;
  moments: MomentPost[];
  personas: Persona[];
  mcpServers: McpServer[];
  capabilityAdapters: CapabilityAdapter[];
  agentConfig: AgentConfig | null;
  skills: EnhancedSkillSummary[];
  proactiveStatuses: ProactiveStatus[];
  lastMcpResult: McpCallResult | null;
  lastMcpToolsResult: McpListToolsResult | null;
  streamedAssistantIds: Set<string>;
  loading: boolean;
  setSection: (section: AppSection, settingsView?: string) => void;
  goBack: () => void;
  bootstrap: () => Promise<void>;
  refreshChatData: (preferredConversationId?: string | null, preferredPersonaId?: string | null) => Promise<void>;
  setConversationProcessing: (conversationId: string, processing: boolean) => void;
  upsertIncomingMessage: (message: ChatMessage) => void;
  createConversation: (personaId?: string) => Promise<void>;
  openPersonaConversation: (personaId: string) => Promise<void>;
  deleteConversation: (conversationId: string) => Promise<void>;
  selectConversation: (conversationId: string) => Promise<void>;
  sendMessage: (content: string, personaId?: string, agentId?: string) => Promise<void>;
  deleteMessage: (messageId: string) => Promise<void>;
  saveLlmProviders: (providers: LlmProvider[]) => Promise<void>;
  saveProfile: (profile: ProfileConfig) => Promise<void>;
  uploadProfileAvatar: (file: File) => Promise<void>;
  clearProfileAvatar: () => Promise<void>;
  refreshAccounts: () => Promise<void>;
  saveAccounts: (accounts: AccountConfig[]) => Promise<void>;
  linkWechatAccount: (personaId: string, accountId: string) => Promise<void>;
  unlinkWechatAccount: (personaId: string) => Promise<void>;
  saveImageProviders: (providers: ImageProvider[]) => Promise<void>;
  saveVideoProviders: (providers: VideoProvider[]) => Promise<void>;
  saveSearchProviders: (providers: SearchProvider[]) => Promise<void>;
  saveVisionProviders: (providers: VisionProvider[]) => Promise<void>;
  saveBrowserProviders: (providers: BrowserProvider[]) => Promise<void>;
  saveThemes: (themes: ThemeConfig[]) => Promise<void>;
  importThemeCss: (file: File) => Promise<void>;
  saveEmojiGroups: (groups: EmojiGroup[]) => Promise<void>;
  uploadEmojiImage: (groupId: string, emotion: string, file: File) => Promise<void>;
  refreshMemories: (personaId?: string) => Promise<void>;
  saveMemory: (memory: Partial<MemoryEntry> & { personaId: string; summary: string; importance: number }) => Promise<void>;
  deleteMemory: (id: string) => Promise<void>;
  saveWorldbook: (book: Worldbook) => Promise<void>;
  deleteWorldbook: (id: string) => Promise<void>;
  refreshMoments: () => Promise<void>;
  createMoment: (body: string) => Promise<void>;
  updateMomentText: (postId: string, body: string) => Promise<void>;
  addMomentComment: (postId: string, text: string) => Promise<void>;
  updateMomentComment: (postId: string, commentId: string, text: string) => Promise<void>;
  deleteMoment: (postId: string) => Promise<void>;
  deleteMomentComment: (postId: string, commentId: string) => Promise<void>;
  toggleMomentLike: (postId: string) => Promise<void>;
  uploadMomentCover: (postId: string, file: File) => Promise<void>;
  clearMomentCover: (postId: string) => Promise<void>;
  refreshMcpServers: () => Promise<void>;
  saveMcpServers: (servers: McpServer[]) => Promise<void>;
  refreshCapabilityAdapters: () => Promise<void>;
  saveCapabilityAdapters: (adapters: CapabilityAdapter[]) => Promise<void>;
  refreshAgentConfig: () => Promise<void>;
  saveAgentConfig: (config: AgentConfig) => Promise<void>;
  refreshAgents: () => Promise<void>;
  refreshAgentQueue: () => Promise<void>;
  refreshAgentRuns: () => Promise<void>;
  handleAgentRunEvent: (event: AgentRunEvent) => void;
  handleManagedProcessEvent: (event: ManagedProcessEvent) => void;
  saveAgent: (agent: AgentDefinition) => Promise<void>;
  deleteAgent: (id: string) => Promise<void>;
  refreshSkills: () => Promise<void>;
  refreshSkillsForAgent: (agentId: string) => Promise<void>;
  installBuiltinSkills: () => Promise<void>;
  saveSkillConfig: (agentId: string, skillId: string, config: Record<string, string>) => Promise<void>;
  refreshSkillBundles: () => Promise<void>;
  installSkillBundle: (bundleId: string, agentId?: string) => Promise<void>;
  refreshMarketplaceSkills: (query?: string, source?: string) => Promise<void>;
  installMarketplaceSkill: (skillId: string, agentId?: string) => Promise<void>;
  installExternalSkillUrl: (url: string, name?: string, category?: string, agentId?: string, force?: boolean) => Promise<void>;
  refreshProactiveStatuses: () => Promise<void>;
  triggerProactiveOnce: (personaId: string) => Promise<void>;
  listMcpTools: (serverId: string, timeoutSeconds?: number) => Promise<void>;
  callMcpTool: (serverId: string, toolName: string, payload: unknown, timeoutSeconds?: number) => Promise<void>;
  savePersona: (persona: Persona) => Promise<Persona>;
  deletePersona: (id: string) => Promise<void>;
  uploadPersonaAvatar: (personaId: string, file: File) => Promise<Persona>;
  clearPersonaAvatar: (personaId: string) => Promise<Persona>;
  saveConfig: (config: AppConfig) => Promise<void>;
  togglePlugin: (pluginId: string, enabled: boolean) => Promise<void>;
}

export const useAppStore = create<AppState>((set, get) => ({
  activeSection: "chat",
  previousSection: null,
  config: null,
  conversations: [],
  activeConversationId: null,
  messages: [],
  processingConversationIds: [],
  conversationUnreadCounts: {},
  llmProviders: [],
  profile: { name: "我", avatarPath: null },
  accounts: [],
  imageProviders: [],
  videoProviders: [],
  searchProviders: [],
  visionProviders: [],
  browserProviders: [],
  themes: [],
  emojiGroups: [],
  memories: [],
  worldbooks: [],
  agents: [],
  agentQueue: [],
  agentRuns: [],
  activeAgentRuns: {},
  managedProcessEvents: [],
  plugins: [],
  skillBundles: [],
  marketplaceSkills: [],
  selectedAgentId: null,
  moments: [],
  personas: [],
  mcpServers: [],
  capabilityAdapters: [],
  agentConfig: null,
  skills: [],
  proactiveStatuses: [],
  lastMcpResult: null,
  lastMcpToolsResult: null,
  streamedAssistantIds: new Set<string>(),
  loading: false,
  setSection: (activeSection, settingsView) => {
    if (settingsView) {
      pendingSettingsViewRef = settingsView;
    }
    set((state) => ({ activeSection, previousSection: state.activeSection }));
  },
  goBack: () => {
    const { previousSection } = get();
    if (previousSection) {
      set({ activeSection: previousSection, previousSection: null });
    }
  },
  bootstrap: async () => {
    set({ loading: true });
    const config = await api.getConfig();
    await api.cleanupHistoricalResources().catch((error) => {
      console.warn("historical resource cleanup failed", error);
    });
    const [
      conversations,
      moments,
      personas,
      mcpServers,
      capabilityAdapters,
      agentConfig,
      skills,
      proactiveStatuses,
      llmProviders,
      profile,
      accounts,
      imageProviders,
      videoProviders,
      searchProviders,
      visionProviders,
      browserProviders,
      themes,
      emojiGroups,
      memories,
      worldbooks,
      plugins,
      agents,
      agentRuns,
      agentQueue,
      skillBundles
    ] = await Promise.all([
      api.listConversations(),
      api.listMoments(),
      api.listPersonas(),
      api.listMcpServers(),
      api.listCapabilityAdapters(),
      api.getAgentConfig(),
      api.listSkills(),
      api.listProactiveStatuses(),
      api.listLlmProviders(),
      api.getProfile(),
      api.listAccounts(),
      api.listImageProviders(),
      api.listVideoProviders(),
      api.listSearchProviders(),
      api.listVisionProviders(),
      api.listBrowserProviders(),
      api.listThemes(),
      api.listEmojiGroups(),
      api.listMemories(),
      api.listWorldbooks(),
      api.listPlugins(),
      api.listAgents(),
      api.listAgentRuns(),
      api.listAgentQueue(),
      api.listSkillBundles()
    ]);
    const currentActive = get().activeConversationId;
    const activeConversationId = currentActive && conversations.some((item) => item.id === currentActive)
      ? currentActive
      : conversations[0]?.id ?? null;
    const messageLimit = uiMessageLimit(config);
    const previewChars = uiMessagePreviewChars(config);
    const messages = activeConversationId ? await api.listMessages(activeConversationId, messageLimit, previewChars) : [];
    set({
      config,
      conversations,
      activeConversationId,
      messages,
      moments,
      personas,
      mcpServers,
      capabilityAdapters,
      agentConfig,
      skills: skills as EnhancedSkillSummary[],
      proactiveStatuses,
      llmProviders,
      profile,
      accounts,
      imageProviders,
      videoProviders,
      searchProviders,
      visionProviders,
      browserProviders,
      themes,
      emojiGroups,
      memories,
      worldbooks,
      plugins,
      agents,
      agentQueue,
      agentRuns,
      skillBundles,
      activeAgentRuns: {},
      managedProcessEvents: [],
      processingConversationIds: [],
      loading: false
    });
    void Promise.all([api.tickScheduledAgentJobs(), api.drainAgentQueue()]).then(async () => {
      const [nextRuns, nextQueue, nextConversations] = await Promise.all([
        api.listAgentRuns(),
        api.listAgentQueue(),
        api.listConversations()
      ]);
      const state = get();
      const messageLimit = uiMessageLimit(state.config);
      const previewChars = uiMessagePreviewChars(state.config);
      const nextMessages = state.activeConversationId
        ? await api.listMessages(state.activeConversationId, messageLimit, previewChars)
        : [];
      set({
        agentRuns: nextRuns,
        agentQueue: nextQueue,
        conversations: nextConversations,
        messages: nextMessages
      });
    }).catch((error) => {
      console.warn("agent scheduler bootstrap failed", error);
    });
  },
  refreshChatData: async (preferredConversationId, preferredPersonaId) => {
    const [conversations, agentQueue] = await Promise.all([
      api.listConversations(),
      api.listAgentQueue()
    ]);
    const state = get();
    const currentActive = state.activeConversationId;
    const activeConversationId =
      (currentActive && conversations.some((item) => item.id === currentActive)
        ? currentActive
        : null)
      ?? (preferredConversationId && conversations.some((item) => item.id === preferredConversationId)
        ? preferredConversationId
        : null)
      ?? (preferredPersonaId
        ? conversations.find((item) => item.personaId === preferredPersonaId)?.id ?? null
        : null)
      ?? conversations[0]?.id
      ?? null;
    const messageLimit = uiMessageLimit(state.config);
    const previewChars = uiMessagePreviewChars(state.config);
    const messages = activeConversationId ? await api.listMessages(activeConversationId, messageLimit, previewChars) : [];
    if (
      state.activeConversationId === activeConversationId
      && sameConversations(state.conversations, conversations)
      && sameMessages(state.messages, messages)
    ) {
      set({ agentQueue });
      return;
    }
    // Preserve existing unread counts, only clear for active conversation
    const unreadCounts = { ...state.conversationUnreadCounts };
    if (activeConversationId) delete unreadCounts[activeConversationId];
    set((current) => ({
      conversations,
      agentQueue,
      activeConversationId,
      messages,
      conversationUnreadCounts: unreadCounts,
      processingConversationIds: messages.at(-1)?.role === "assistant"
        ? current.processingConversationIds.filter((id) => id !== activeConversationId)
        : current.processingConversationIds
    }));
  },
  setConversationProcessing: (conversationId, processing) => {
    if (!conversationId) return;
    set((state) => {
      const exists = state.processingConversationIds.includes(conversationId);
      if (processing && !exists) {
        return { processingConversationIds: [...state.processingConversationIds, conversationId] };
      }
      if (!processing && exists) {
        return {
          processingConversationIds: state.processingConversationIds.filter((id) => id !== conversationId)
        };
      }
      return state;
    });
  },
  upsertIncomingMessage: (message) => {
    set((state) => {
      if (state.activeConversationId && message.conversationId !== state.activeConversationId) {
        return state;
      }
      const index = state.messages.findIndex((item) => item.id === message.id);
      const messageLimit = uiMessageLimit(state.config);
      const messages = index >= 0
        ? state.messages.map((item) => (item.id === message.id ? message : item))
        : [...state.messages, message];
      return { messages: limitMessages(messages, messageLimit) };
    });
  },
  createConversation: async (personaId) => {
    const persona = personaId ? get().personas.find((item) => item.id === personaId) : null;
    const conversation = await api.createConversation(persona?.name, personaId);
    const conversations = await api.listConversations();
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const messages = await api.listMessages(conversation.id, messageLimit, previewChars);
    set({
      conversations,
      activeConversationId: conversation.id,
      messages
    });
  },
  openPersonaConversation: async (personaId) => {
    const persona = get().personas.find((item) => item.id === personaId);
    const conversation = await api.createConversation(persona?.name, personaId);
    const conversations = await api.listConversations();
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const messages = await api.listMessages(conversation.id, messageLimit, previewChars);
    set({
      conversations,
      activeConversationId: conversation.id,
      messages
    });
  },
  deleteConversation: async (conversationId) => {
    await api.deleteConversation(conversationId);
    const conversations = await api.listConversations();
    const activeConversationId = get().activeConversationId === conversationId
      ? conversations[0]?.id ?? null
      : get().activeConversationId;
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const messages = activeConversationId ? await api.listMessages(activeConversationId, messageLimit, previewChars) : [];
    const { [conversationId]: _, ...unreadCounts } = get().conversationUnreadCounts;
    set({ conversations, activeConversationId, messages, conversationUnreadCounts: unreadCounts });
  },
  selectConversation: async (conversationId) => {
    set((state) => state.activeConversationId === conversationId
      ? state
      : {
          activeConversationId: conversationId,
          messages: [],
          conversationUnreadCounts: { ...state.conversationUnreadCounts, [conversationId]: 0 }
        });
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const messages = await api.listMessages(conversationId, messageLimit, previewChars);
    if (get().activeConversationId === conversationId) {
      set({ activeConversationId: conversationId, messages });
    }
  },
  sendMessage: async (content, personaId, agentId) => {
    const cleanContent = content.trim();
    if (!cleanContent) return;
    const state = get();
    const newConversationTitle = parseNewConversationCommand(cleanContent);
    if (newConversationTitle !== undefined) {
      const persona = personaId ? state.personas.find((item) => item.id === personaId) : null;
      const conversation = await api.createConversation(newConversationTitle ?? persona?.name, personaId);
      const conversations = await api.listConversations();
      const messageLimit = uiMessageLimit(state.config);
      const previewChars = uiMessagePreviewChars(state.config);
      set({
        conversations,
        activeConversationId: conversation.id,
        messages: await api.listMessages(conversation.id, messageLimit, previewChars),
        processingConversationIds: state.processingConversationIds.filter((id) => id !== state.activeConversationId)
      });
      return;
    }
    const sessionSelector = parseSessionSwitchCommand(cleanContent);
    if (sessionSelector) {
      const selector = sessionSelector.toLowerCase();
      const conversations = state.conversations.length > 0 ? state.conversations : await api.listConversations();
      const matches = conversations.filter((conversation) =>
        conversation.id.toLowerCase() === selector
        || conversation.id.toLowerCase().startsWith(selector)
        || conversation.title.toLowerCase() === selector
      );
      if (matches.length === 1) {
        const conversation = matches[0];
        const messageLimit = uiMessageLimit(state.config);
        const previewChars = uiMessagePreviewChars(state.config);
        set({
          conversations,
          activeConversationId: conversation.id,
          messages: await api.listMessages(conversation.id, messageLimit, previewChars),
          conversationUnreadCounts: { ...state.conversationUnreadCounts, [conversation.id]: 0 },
          processingConversationIds: state.processingConversationIds.filter((id) => id !== state.activeConversationId)
        });
        return;
      }
    }
    let activeConversationId = state.activeConversationId;
    const activeConversation = state.conversations.find((item) => item.id === activeConversationId);
    if (!activeConversationId || !activeConversation || (personaId && activeConversation.personaId !== personaId)) {
      const persona = personaId ? state.personas.find((item) => item.id === personaId) : null;
      const conversation = await api.createConversation(persona?.name, personaId);
      activeConversationId = conversation.id;
      const conversations = await api.listConversations();
      const messageLimit = uiMessageLimit(state.config);
      const previewChars = uiMessagePreviewChars(state.config);
      set({ conversations, activeConversationId, messages: await api.listMessages(conversation.id, messageLimit, previewChars) });
    }
    const temporaryMessage: ChatMessage = {
      id: `local-${crypto.randomUUID()}`,
      conversationId: activeConversationId ?? "",
      role: "user",
      content: cleanContent,
      createdAt: new Date().toISOString(),
      source: "desktop",
      accountId: null
    };
    set((current) => ({
      messages: limitMessages([...current.messages, temporaryMessage], uiMessageLimit(current.config)),
      processingConversationIds: current.processingConversationIds.includes(activeConversationId ?? "")
        ? current.processingConversationIds
        : [...current.processingConversationIds, activeConversationId ?? ""].filter(Boolean),
      conversations: current.conversations.map((conversation) =>
        conversation.id === activeConversationId
          ? { ...conversation, lastMessage: cleanContent, updatedAt: temporaryMessage.createdAt }
          : conversation
      )
    }));
    // 异步发送消息，不阻塞 UI
    api.sendChatMessage({
      conversationId: activeConversationId,
      personaId: personaId ?? activeConversation?.personaId ?? null,
      agentId: agentId ?? activeConversation?.agentId ?? null,
      content: cleanContent
    }).then((responseMessages) => {
      void get().refreshAgentQueue();
      if (!responseMessages || responseMessages.length === 0) {
        // 后端未返回消息，清除处理状态
        get().setConversationProcessing(activeConversationId ?? "", false);
        return;
      }
      const hasAssistantReply = responseMessages.some((m) => m.role === "assistant" && m.content.trim());
      const messageLimit = uiMessageLimit(get().config);
      set((current) => {
        // 去重：用后端返回的消息替换本地临时消息
        const backendUserIds = new Set(
          responseMessages.filter((m) => m.role === "user").map((m) => m.id)
        );
        // 移除本地临时 user 消息（id 以 "local-" 开头），保留后端返回的
        const withoutTemp = current.messages.filter(
          (m) => !(m.id.startsWith("local-") && m.role === "user" && backendUserIds.size > 0)
        );
        const existingIds = new Set(withoutTemp.map((m) => m.id));
        const newMessages = responseMessages.filter((m) => !existingIds.has(m.id));
        const merged = [...withoutTemp, ...newMessages];
        return { messages: limitMessages(merged, messageLimit) };
      });
      // 仅当后端确认返回了助手回复时，立即清除处理状态
      // 否则等待轮询（refreshChatData）检测到助手消息后自动清除
      if (hasAssistantReply) {
        get().setConversationProcessing(activeConversationId ?? "", false);
      }
    }).catch((error) => {
      console.error("发送消息失败:", error);
      void get().refreshAgentQueue();
      get().setConversationProcessing(activeConversationId ?? "", false);
    });
  },
  deleteMessage: async (messageId) => {
    await api.deleteMessage(messageId);
    set((state) => ({ messages: state.messages.filter((message) => message.id !== messageId) }));
  },
  saveLlmProviders: async (llmProviders) => {
    await api.saveLlmProviders(llmProviders);
    set({ llmProviders });
  },
  saveProfile: async (profile) => {
    const saved = await api.saveProfile(profile);
    set({ profile: saved });
  },
  uploadProfileAvatar: async (file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const profile = await api.uploadProfileAvatar(file.name, bytes);
    set({ profile });
  },
  clearProfileAvatar: async () => {
    const profile = await api.clearProfileAvatar();
    set({ profile });
  },
  refreshAccounts: async () => {
    const accounts = await api.listAccounts();
    set({ accounts });
  },
  saveAccounts: async (accounts) => {
    await api.saveAccounts(accounts);
    set({ accounts });
  },
  linkWechatAccount: async (personaId, accountId) => {
    const accounts = await api.linkWechatAccount(personaId, accountId);
    set({ accounts });
  },
  unlinkWechatAccount: async (personaId) => {
    const accounts = await api.unlinkWechatAccount(personaId);
    set({ accounts });
  },
  saveImageProviders: async (imageProviders) => {
    await api.saveImageProviders(imageProviders);
    set({ imageProviders });
  },
  saveVideoProviders: async (videoProviders) => {
    await api.saveVideoProviders(videoProviders);
    set({ videoProviders });
  },
  saveSearchProviders: async (searchProviders) => {
    await api.saveSearchProviders(searchProviders);
    set({ searchProviders });
  },
  saveVisionProviders: async (visionProviders) => {
    await api.saveVisionProviders(visionProviders);
    set({ visionProviders });
  },
  saveBrowserProviders: async (browserProviders) => {
    await api.saveBrowserProviders(browserProviders);
    set({ browserProviders });
  },
  saveThemes: async (themes) => {
    await api.saveThemes(themes);
    set({ themes });
  },
  importThemeCss: async (file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const themes = await api.importThemeCss(file.name, bytes);
    set({ themes });
  },
  saveEmojiGroups: async (emojiGroups) => {
    await api.saveEmojiGroups(emojiGroups);
    set({ emojiGroups });
  },
  uploadEmojiImage: async (groupId, emotion, file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const emojiGroups = await api.uploadEmojiImage(groupId, emotion, file.name, bytes);
    set({ emojiGroups });
  },
  refreshMemories: async (personaId) => {
    const memories = await api.listMemories(personaId);
    set({ memories });
  },
  saveMemory: async (memory) => {
    const saved = await api.saveMemory(memory);
    set((state) => ({
      memories: [saved, ...state.memories.filter((item) => item.id !== saved.id)]
        .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
    }));
  },
  deleteMemory: async (id) => {
    await api.deleteMemory(id);
    set((state) => ({ memories: state.memories.filter((memory) => memory.id !== id) }));
  },
  saveWorldbook: async (book) => {
    const saved = await api.saveWorldbook(book);
    set((state) => ({
      worldbooks: [saved, ...state.worldbooks.filter((item) => item.id !== saved.id)]
        .sort((a, b) => a.name.localeCompare(b.name))
    }));
  },
  deleteWorldbook: async (id) => {
    await api.deleteWorldbook(id);
    set((state) => ({ worldbooks: state.worldbooks.filter((book) => book.id !== id) }));
  },
  refreshMoments: async () => {
    const moments = await api.listMoments();
    set({ moments });
  },
  createMoment: async (body) => {
    const post = await api.createMoment(body);
    set((state) => ({ moments: [post, ...state.moments].sort((a, b) => b.createdAt.localeCompare(a.createdAt)) }));
  },
  updateMomentText: async (postId, body) => {
    const post = await api.updateMomentText(postId, body);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  addMomentComment: async (postId, text) => {
    const post = await api.addMomentComment(postId, text);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  updateMomentComment: async (postId, commentId, text) => {
    const post = await api.updateMomentComment(postId, commentId, text);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  deleteMoment: async (postId) => {
    await api.deleteMoment(postId);
    set((state) => ({ moments: state.moments.filter((post) => post.id !== postId) }));
  },
  deleteMomentComment: async (postId, commentId) => {
    const post = await api.deleteMomentComment(postId, commentId);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  toggleMomentLike: async (postId) => {
    const current = get().moments.find((post) => post.id === postId);
    const post = current?.likedBy.includes("user")
      ? await api.unlikeMoment(postId)
      : await api.likeMoment(postId);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  uploadMomentCover: async (postId, file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const post = await api.uploadMomentCover(postId, file.name, bytes);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  clearMomentCover: async (postId) => {
    const post = await api.clearMomentCover(postId);
    set((state) => ({ moments: state.moments.map((item) => (item.id === post.id ? post : item)) }));
  },
  refreshMcpServers: async () => {
    const mcpServers = await api.listMcpServers();
    set({ mcpServers });
  },
  saveMcpServers: async (mcpServers) => {
    await api.saveMcpServers(mcpServers);
    set({ mcpServers });
  },
  refreshCapabilityAdapters: async () => {
    const capabilityAdapters = await api.listCapabilityAdapters();
    set({ capabilityAdapters });
  },
  saveCapabilityAdapters: async (capabilityAdapters) => {
    await api.saveCapabilityAdapters(capabilityAdapters);
    set({ capabilityAdapters });
  },
  refreshAgentConfig: async () => {
    const agentConfig = await api.getAgentConfig();
    set({ agentConfig });
  },
  saveAgentConfig: async (agentConfig) => {
    const saved = await api.saveAgentConfig(agentConfig);
    set((state) => ({
      agentConfig: saved,
      agents: state.agents.map((agent) =>
        agent.isDefault || agent.id === "default"
          ? {
            ...agent,
            enabled: saved.enabled,
            mcpEnabled: saved.mcpEnabled,
            skillsEnabled: saved.skillsEnabled,
            allowShell: saved.allowShell,
            maxSubagents: saved.maxSubagents,
            maxSubagentDepth: saved.maxSubagentDepth,
            maxToolIterations: saved.maxToolIterations,
            skillsDir: saved.skillsDir,
            enabledSkills: saved.enabledSkills,
            enabledMcpServers: saved.enabledMcpServers,
            enabledToolsets: saved.enabledToolsets,
            disabledToolsets: saved.disabledToolsets
          }
          : agent
      )
    }));
  },
  refreshSkills: async () => {
    const skills = await api.listSkills();
    set({ skills: skills as EnhancedSkillSummary[] });
  },
  installBuiltinSkills: async () => {
    const skills = await api.installBuiltinSkills();
    set({ skills: skills as EnhancedSkillSummary[] });
  },
  refreshProactiveStatuses: async () => {
    const proactiveStatuses = await api.listProactiveStatuses();
    set({ proactiveStatuses });
  },
  triggerProactiveOnce: async (personaId) => {
    await api.triggerProactiveOnce(personaId);
    const proactiveStatuses = await api.listProactiveStatuses();
    set({ proactiveStatuses });
  },
  listMcpTools: async (serverId, timeoutSeconds) => {
    const lastMcpToolsResult = await api.listMcpTools(serverId, timeoutSeconds);
    set({ lastMcpToolsResult });
  },
  callMcpTool: async (serverId, toolName, payload, timeoutSeconds) => {
    const lastMcpResult = await api.callMcpTool(serverId, toolName, payload, timeoutSeconds);
    set({ lastMcpResult });
  },
  savePersona: async (persona) => {
    const saved = await api.savePersona(persona);
    set((state) => ({
      personas: [saved, ...state.personas.filter((item) => item.id !== saved.id)]
        .sort((a, b) => a.name.localeCompare(b.name))
    }));
    return saved;
  },
  deletePersona: async (id) => {
    await api.deletePersona(id);
    set((state) => ({ personas: state.personas.filter((persona) => persona.id !== id) }));
  },
  uploadPersonaAvatar: async (personaId, file) => {
    const buffer = await file.arrayBuffer();
    const bytes = Array.from(new Uint8Array(buffer));
    const saved = await api.uploadPersonaAvatar(personaId, file.name, bytes);
    set((state) => ({ personas: state.personas.map((item) => (item.id === saved.id ? saved : item)) }));
    return saved;
  },
  clearPersonaAvatar: async (personaId) => {
    const saved = await api.clearPersonaAvatar(personaId);
    set((state) => ({ personas: state.personas.map((item) => (item.id === saved.id ? saved : item)) }));
    return saved;
  },
  saveConfig: async (config) => {
    await api.saveConfig(config);
    set({ config });
  },
  togglePlugin: async (pluginId, enabled) => {
    const plugins = await api.togglePlugin(pluginId, enabled);
    set({ plugins });
  },
  refreshAgents: async () => {
    const agents = await api.listAgents();
    set({ agents });
  },
  refreshAgentQueue: async () => {
    const agentQueue = await api.listAgentQueue();
    set({ agentQueue });
  },
  refreshAgentRuns: async () => {
    const agentRuns = await api.listAgentRuns();
    set({ agentRuns });
  },
  handleAgentRunEvent: (event) => {
    set((state) => {
      const terminal = event.state === "completed" || event.state === "failed" || event.state === "aborted";
      const activeAgentRuns = { ...state.activeAgentRuns };
      if (terminal || event.parentRunId) {
        delete activeAgentRuns[event.runId];
      } else {
        const prevRun = state.activeAgentRuns[event.runId];
        activeAgentRuns[event.runId] = {
          ...event,
          accumulatedToolEvents: mergeToolRunEvents(prevRun, event),
          accumulatedPhases: mergeRunPhases(prevRun, event)
        };
      }
      const existingIndex = state.agentRuns.findIndex((run) => run.runId === event.runId);
      const fallbackRun: AgentRunRecord = {
        runId: event.runId,
        conversationId: event.conversationId,
        personaId: event.personaId,
        agentId: event.agentId,
        parentRunId: event.parentRunId ?? null,
        subagentIndex: event.subagentIndex ?? null,
        subagentDepth: event.subagentDepth ?? null,
        subagentCanDelegate: event.subagentCanDelegate ?? null,
        subagentRole: event.subagentRole ?? null,
        subagentTask: event.subagentTask ?? null,
        subagentToolsets: event.subagentToolsets ?? [],
        subagentMaxIterations: event.subagentMaxIterations ?? null,
        queueItemId: event.queueItemId ?? null,
        userRequest: "",
        state: event.state,
        startedAt: event.updatedAt,
        updatedAt: event.updatedAt,
        lastActivityAt: event.lastActivityAt ?? event.updatedAt,
        lastActivityDesc: event.lastActivityDesc ?? null,
        completedAt: terminal ? event.updatedAt : null,
        error: event.error ?? null,
        toolEvents: mergeToolEventList([], event.toolEvent),
        phaseEvents: event.phase ? [{ phase: event.phase, detail: event.detail ?? null, updatedAt: event.updatedAt }] : [],
        checkpoints: []
      };
      const agentRuns = existingIndex >= 0
        ? state.agentRuns.map((run, index) => index === existingIndex
          ? {
            ...run,
            parentRunId: event.parentRunId ?? run.parentRunId ?? null,
            subagentIndex: event.subagentIndex ?? run.subagentIndex ?? null,
            subagentDepth: event.subagentDepth ?? run.subagentDepth ?? null,
            subagentCanDelegate: event.subagentCanDelegate ?? run.subagentCanDelegate ?? null,
            subagentRole: event.subagentRole ?? run.subagentRole ?? null,
            subagentTask: event.subagentTask ?? run.subagentTask ?? null,
            subagentToolsets: event.subagentToolsets ?? run.subagentToolsets ?? [],
            subagentMaxIterations: event.subagentMaxIterations ?? run.subagentMaxIterations ?? null,
            queueItemId: event.queueItemId ?? run.queueItemId ?? null,
            state: event.state,
            updatedAt: event.updatedAt,
            lastActivityAt: event.lastActivityAt ?? run.lastActivityAt ?? event.updatedAt,
            lastActivityDesc: event.lastActivityDesc ?? run.lastActivityDesc ?? null,
            completedAt: terminal ? event.updatedAt : run.completedAt,
            error: event.error ?? run.error,
            toolEvents: mergeToolEventList(run.toolEvents, event.toolEvent),
            phaseEvents: event.phase
              ? [...(run.phaseEvents ?? []), { phase: event.phase, detail: event.detail ?? null, updatedAt: event.updatedAt }].slice(-200)
              : run.phaseEvents
          }
          : run)
        : [fallbackRun, ...state.agentRuns];
      const agentQueue = event.queueItemId
        ? state.agentQueue.map((item) => item.id === event.queueItemId
          ? {
            ...item,
            status: terminal ? event.state : "running",
            updatedAt: event.updatedAt,
            startedAt: item.startedAt ?? event.updatedAt,
            completedAt: terminal ? event.updatedAt : item.completedAt,
            error: event.error ?? item.error
          }
          : item)
        : state.agentQueue;
      return { activeAgentRuns, agentRuns, agentQueue };
    });
  },
  handleManagedProcessEvent: (event) => {
    set((state) => {
      const withoutDuplicate = state.managedProcessEvents.filter((item) => {
        if (item.processId !== event.processId || item.type !== event.type) return true;
        if (item.createdAt !== event.createdAt) return true;
        return JSON.stringify(item.detail ?? null) !== JSON.stringify(event.detail ?? null);
      });
      return {
        managedProcessEvents: [event, ...withoutDuplicate]
          .sort((a, b) => b.createdAt.localeCompare(a.createdAt))
          .slice(0, 80)
      };
    });
  },
  saveAgent: async (agent) => {
    const saved = await api.saveAgent(agent);
    set((state) => ({
      agents: [saved, ...state.agents.filter((item) => item.id !== saved.id)]
        .sort((a, b) => (b.isDefault ? 1 : 0) - (a.isDefault ? 1 : 0))
    }));
  },
  deleteAgent: async (id) => {
    await api.deleteAgent(id);
    set((state) => ({ agents: state.agents.filter((agent) => agent.id !== id) }));
  },
  refreshSkillsForAgent: async (agentId) => {
    const skills = await api.listSkillsForAgent(agentId);
    set({ skills: skills });
  },
  saveSkillConfig: async (agentId, skillId, config) => {
    await api.saveSkillConfig(agentId, skillId, config);
  },
  refreshSkillBundles: async () => {
    const skillBundles = await api.listSkillBundles();
    set({ skillBundles });
  },
  installSkillBundle: async (bundleId, agentId) => {
    const skills = await api.installSkillBundle(bundleId, agentId);
    set({ skills });
  },
  refreshMarketplaceSkills: async (query, source) => {
    const marketplaceSkills = source && source !== "local"
      ? await api.searchSkillMarketplace(query, source)
      : await api.listMarketplaceSkills(query);
    set({ marketplaceSkills });
  },
  installMarketplaceSkill: async (skillId, agentId) => {
    const skill = await api.installMarketplaceSkill(skillId, agentId);
    if (skill) {
      set((state) => ({ skills: [...state.skills.filter((item) => item.id !== skill.id), skill] }));
    }
  },
  installExternalSkillUrl: async (url, name, category, agentId, force) => {
    const skill = await api.installExternalSkillUrl(url, name, category, agentId, force);
    if (skill) {
      set((state) => ({
        skills: [...state.skills.filter((item) => item.id !== skill.id), skill as EnhancedSkillSummary]
      }));
    }
  }
}));
