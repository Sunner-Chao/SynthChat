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
const BOOTSTRAP_CACHE_STORAGE_KEY = "synthchat.bootstrap.cache.v1";
const TERMINAL_AGENT_STATES = new Set(["completed", "failed", "aborted"]);
const ACTIVE_QUEUE_STATES = new Set(["pending", "running"]);

// Module-level ref for pending settings view (not in React state to avoid batching delays)
let pendingSettingsViewRef: string | null = null;

// Grace window guarding against a refresh clearing a "processing" flag that was
// just set (e.g. WeChat/pet emits a processing event before the user message is
// persisted, so a concurrent refresh still sees a stale assistant tail).
const PROCESSING_MARK_GRACE_MS = 1500;
const processingMarkedAtCache = new Map<string, number>();
const processingClearTimerCache = new Map<string, number>();
function withinProcessingGrace(conversationId: string | null): boolean {
  if (!conversationId) return false;
  const markedAt = processingMarkedAtCache.get(conversationId);
  return markedAt !== undefined && Date.now() - markedAt < PROCESSING_MARK_GRACE_MS;
}
export function consumePendingSettingsView(): string | null {
  const v = pendingSettingsViewRef;
  pendingSettingsViewRef = null;
  return v;
}

type BootstrapCacheSnapshot = {
  config: AppConfig | null;
  profile: ProfileConfig;
  llmProviders: LlmProvider[];
  imageProviders: ImageProvider[];
  videoProviders: VideoProvider[];
  searchProviders: SearchProvider[];
  visionProviders: VisionProvider[];
  browserProviders: BrowserProvider[];
  themes: ThemeConfig[];
  emojiGroups: EmojiGroup[];
  accounts: AccountConfig[];
  personas: Persona[];
};

function readBootstrapCache(): BootstrapCacheSnapshot | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(BOOTSTRAP_CACHE_STORAGE_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as BootstrapCacheSnapshot;
  } catch {
    return null;
  }
}

function writeBootstrapCache(snapshot: BootstrapCacheSnapshot) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(BOOTSTRAP_CACHE_STORAGE_KEY, JSON.stringify(snapshot));
  } catch {
    // ignore cache write failures
  }
}

const bootstrapCache = readBootstrapCache();

function withBootstrapTimeout<T>(promise: Promise<T>, fallback: T, label: string, timeoutMs = 5000): Promise<T> {
  let timeoutId: number | null = null;
  const timeoutPromise = new Promise<T>((resolve) => {
    timeoutId = window.setTimeout(() => {
      console.warn(`${label} timed out during bootstrap; using fallback`);
      resolve(fallback);
    }, timeoutMs);
  });
  return Promise.race([
    promise
      .then((value) => {
        if (timeoutId !== null) window.clearTimeout(timeoutId);
        return value;
      })
      .catch((error) => {
        if (timeoutId !== null) window.clearTimeout(timeoutId);
        console.warn(`${label} failed during bootstrap`, error);
        return fallback;
      }),
    timeoutPromise
  ]);
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

function isVisibleChatMessage(message: ChatMessage) {
  return !(message.role === "user" && message.source === "proactive-internal");
}

function visibleChatMessages(messages: ChatMessage[]) {
  return messages.filter(isVisibleChatMessage);
}

function isLocalUiMessage(message: ChatMessage) {
  return message.id.startsWith("local-");
}

function isLocalStatusMessage(message: ChatMessage) {
  return message.source?.startsWith("desktop-local-") ?? false;
}

function messageTime(message: ChatMessage) {
  const timestamp = Date.parse(message.createdAt);
  return Number.isFinite(timestamp) ? timestamp : 0;
}

function mergeLocalUiMessages(backendMessages: ChatMessage[], currentMessages: ChatMessage[], conversationId: string | null, limit: number) {
  if (!conversationId) return limitMessages(backendMessages, limit);
  const backendIds = new Set(backendMessages.map((message) => message.id));
  const localMessages = currentMessages.filter((message) => {
    if (message.conversationId !== conversationId || !isLocalUiMessage(message) || backendIds.has(message.id)) {
      return false;
    }
    if (message.role === "user") {
      const localContent = message.content.trim();
      return !backendMessages.some((backend) => backend.role === "user" && backend.content.trim() === localContent);
    }
    if (isLocalStatusMessage(message)) {
      const localCreatedAt = messageTime(message);
      return !backendMessages.some((backend) => backend.role === "assistant" && messageTime(backend) >= localCreatedAt - 1000);
    }
    return false;
  });
  if (localMessages.length === 0) return limitMessages(backendMessages, limit);
  return limitMessages([...backendMessages, ...localMessages].sort((left, right) => messageTime(left) - messageTime(right)), limit);
}

function hasPendingAgentWork(state: AppState, conversationId: string | null) {
  if (!conversationId) return false;
  return Object.values(state.activeAgentRuns).some((run) =>
    run.conversationId === conversationId
    && !run.parentRunId
    && !TERMINAL_AGENT_STATES.has(run.state)
  )
    || state.agentQueue.some((item) =>
      item.conversationId === conversationId
      && ACTIVE_QUEUE_STATES.has(item.status)
    )
    || state.agentRuns.some((run) =>
      run.conversationId === conversationId
      && !run.parentRunId
      && !TERMINAL_AGENT_STATES.has(run.state)
    );
}

function appendLocalAssistantNotice(
  setState: (updater: (current: AppState) => Partial<AppState> | AppState) => void,
  conversationId: string | null,
  content: string,
  source = "desktop-local-status"
) {
  if (!conversationId || !content.trim()) return;
  setState((current) => {
    if (current.messages.some((message) =>
      message.conversationId === conversationId
      && message.role === "assistant"
      && message.source === source
      && message.content === content
    )) {
      return current;
    }
    const now = new Date().toISOString();
    const message: ChatMessage = {
      id: `local-status-${crypto.randomUUID()}`,
      conversationId,
      role: "assistant",
      content,
      createdAt: now,
      source,
      accountId: null
    };
    return {
      messages: limitMessages([...current.messages, message], uiMessageLimit(current.config)),
      conversations: current.conversations.map((conversation) =>
        conversation.id === conversationId
          ? { ...conversation, lastMessage: content, updatedAt: now }
          : conversation
      )
    };
  });
}

function compactErrorMessage(error: unknown) {
  const raw = error instanceof Error
    ? error.message
    : typeof error === "string"
      ? error
      : String(error ?? "");
  const text = raw.replace(/^bad request:\s*/i, "").trim();
  if (!text) return "发送失败。";
  return `发送失败：${text.length > 90 ? `${text.slice(0, 90)}...` : text}`;
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
      && item.agentId === other.agentId
      && item.wechatAccountId === other.wechatAccountId;
  });
}

function normalizeFocusedAgentId(agents: AgentDefinition[], preferred?: string | null) {
  const trimmed = preferred?.trim() ?? "";
  if (trimmed && agents.some((agent) => agent.id === trimmed)) return trimmed;
  return agents.find((agent) => agent.isDefault)?.id ?? agents[0]?.id ?? null;
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
  focusedAgentId: string | null;
  skillsPanelMode: "local" | "global";
  mcpPanelMode: "local" | "global";
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
  setFocusedAgentId: (agentId: string | null) => void;
  setSkillsPanelMode: (mode: "local" | "global") => void;
  setMcpPanelMode: (mode: "local" | "global") => void;
  goBack: () => void;
  bootstrap: () => Promise<void>;
  refreshChatData: (preferredConversationId?: string | null, preferredPersonaId?: string | null) => Promise<void>;
  setConversationProcessing: (conversationId: string, processing: boolean) => void;
  incrementConversationUnread: (conversationId: string, amount?: number) => void;
  markConversationRead: (conversationId: string) => void;
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
  saveAgent: (agent: AgentDefinition) => Promise<AgentDefinition>;
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
  focusedAgentId: null,
  skillsPanelMode: "global",
  mcpPanelMode: "global",
  config: bootstrapCache?.config ?? null,
  conversations: [],
  activeConversationId: null,
  messages: [],
  processingConversationIds: [],
  conversationUnreadCounts: {},
  llmProviders: bootstrapCache?.llmProviders ?? [],
  profile: bootstrapCache?.profile ?? { name: "我", avatarPath: null },
  accounts: bootstrapCache?.accounts ?? [],
  imageProviders: bootstrapCache?.imageProviders ?? [],
  videoProviders: bootstrapCache?.videoProviders ?? [],
  searchProviders: bootstrapCache?.searchProviders ?? [],
  visionProviders: bootstrapCache?.visionProviders ?? [],
  browserProviders: bootstrapCache?.browserProviders ?? [],
  themes: bootstrapCache?.themes ?? [],
  emojiGroups: bootstrapCache?.emojiGroups ?? [],
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
  moments: [],
  personas: bootstrapCache?.personas ?? [],
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
  setFocusedAgentId: (agentId) => {
    set((state) => ({
      focusedAgentId: normalizeFocusedAgentId(state.agents, agentId)
    }));
  },
  setSkillsPanelMode: (skillsPanelMode) => set({ skillsPanelMode }),
  setMcpPanelMode: (mcpPanelMode) => set({ mcpPanelMode }),
  goBack: () => {
    const { previousSection } = get();
    if (previousSection) {
      set({ activeSection: previousSection, previousSection: null });
    }
  },
  bootstrap: async () => {
    set({ loading: true });
    const config = await withBootstrapTimeout(
      api.getConfig(),
      get().config ?? bootstrapCache?.config ?? null,
      "config bootstrap",
      3000
    );
    const profile = await withBootstrapTimeout(
      api.getProfile(),
      get().profile,
      "profile bootstrap",
      3000
    );
    set({ config, profile, loading: false });
    await api.cleanupHistoricalResources().catch((error) => {
      console.warn("historical resource cleanup failed", error);
    });
    const results = await Promise.allSettled([
      withBootstrapTimeout(api.listConversations(), [] as Conversation[], "conversations bootstrap"),
      withBootstrapTimeout(api.listMoments(), [] as MomentPost[], "moments bootstrap"),
      withBootstrapTimeout(api.listPersonas(), get().personas, "personas bootstrap"),
      withBootstrapTimeout(api.listMcpServers(), [] as McpServer[], "mcp servers bootstrap"),
      withBootstrapTimeout(api.listCapabilityAdapters(), [] as CapabilityAdapter[], "capability adapters bootstrap"),
      withBootstrapTimeout(api.getAgentConfig(), null as AgentConfig | null, "agent config bootstrap"),
      withBootstrapTimeout(api.listSkills(), [] as EnhancedSkillSummary[], "skills bootstrap"),
      withBootstrapTimeout(api.listProactiveStatuses(), [] as ProactiveStatus[], "proactive statuses bootstrap"),
      withBootstrapTimeout(api.listLlmProviders(), get().llmProviders, "llm providers bootstrap"),
      withBootstrapTimeout(api.listAccounts(), get().accounts, "accounts bootstrap"),
      withBootstrapTimeout(api.listImageProviders(), get().imageProviders, "image providers bootstrap"),
      withBootstrapTimeout(api.listVideoProviders(), get().videoProviders, "video providers bootstrap"),
      withBootstrapTimeout(api.listSearchProviders(), get().searchProviders, "search providers bootstrap"),
      withBootstrapTimeout(api.listVisionProviders(), get().visionProviders, "vision providers bootstrap"),
      withBootstrapTimeout(api.listBrowserProviders(), get().browserProviders, "browser providers bootstrap"),
      withBootstrapTimeout(api.listThemes(), get().themes, "themes bootstrap"),
      withBootstrapTimeout(api.listEmojiGroups(), get().emojiGroups, "emoji groups bootstrap"),
      withBootstrapTimeout(api.listMemories(), [] as MemoryEntry[], "memories bootstrap"),
      withBootstrapTimeout(api.listWorldbooks(), [] as Worldbook[], "worldbooks bootstrap"),
      withBootstrapTimeout(api.listPlugins(), [] as PluginSummary[], "plugins bootstrap"),
      withBootstrapTimeout(api.listAgents(), [] as AgentDefinition[], "agents bootstrap"),
      withBootstrapTimeout(api.listAgentRuns(), [] as AgentRunRecord[], "agent runs bootstrap"),
      withBootstrapTimeout(api.listAgentQueue(), [] as AgentQueuedRequest[], "agent queue bootstrap"),
      withBootstrapTimeout(api.listSkillBundles(), [] as SkillBundle[], "skill bundles bootstrap")
    ]);
    const pick = <T,>(index: number, fallback: T): T => {
      const result = results[index];
      if (result.status === "fulfilled") return result.value as T;
      console.warn(`bootstrap item ${index} failed`, result.reason);
      return fallback;
    };
    const conversations = pick<Conversation[]>(0, []);
    const moments = pick<MomentPost[]>(1, []);
    const personas = pick<Persona[]>(2, []);
    const mcpServers = pick<McpServer[]>(3, []);
    const capabilityAdapters = pick<CapabilityAdapter[]>(4, []);
    const agentConfig = pick<AgentConfig | null>(5, null);
    const skills = pick<EnhancedSkillSummary[]>(6, []);
    const proactiveStatuses = pick<ProactiveStatus[]>(7, []);
    const llmProviders = pick<LlmProvider[]>(8, []);
    const accounts = pick<AccountConfig[]>(9, []);
    const imageProviders = pick<ImageProvider[]>(10, []);
    const videoProviders = pick<VideoProvider[]>(11, []);
    const searchProviders = pick<SearchProvider[]>(12, []);
    const visionProviders = pick<VisionProvider[]>(13, []);
    const browserProviders = pick<BrowserProvider[]>(14, []);
    const themes = pick<ThemeConfig[]>(15, []);
    const emojiGroups = pick<EmojiGroup[]>(16, []);
    const memories = pick<MemoryEntry[]>(17, []);
    const worldbooks = pick<Worldbook[]>(18, []);
    const plugins = pick<PluginSummary[]>(19, []);
    const agents = pick<AgentDefinition[]>(20, []);
    const agentRuns = pick<AgentRunRecord[]>(21, []);
    const agentQueue = pick<AgentQueuedRequest[]>(22, []);
    const skillBundles = pick<SkillBundle[]>(23, []);
    const currentActive = get().activeConversationId;
    const activeConversationId = currentActive && conversations.some((item) => item.id === currentActive)
      ? currentActive
      : conversations[0]?.id ?? null;
    const messageLimit = uiMessageLimit(config);
    const previewChars = uiMessagePreviewChars(config);
    const messages = activeConversationId
      ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars).catch((error) => {
        console.warn("message bootstrap failed", error);
        return [];
      }))
      : [];
    set({
      config,
      conversations,
      activeConversationId,
      focusedAgentId: normalizeFocusedAgentId(agents, get().focusedAgentId),
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
    writeBootstrapCache({
      config,
      profile,
      llmProviders,
      imageProviders,
      videoProviders,
      searchProviders,
      visionProviders,
      browserProviders,
      themes,
      emojiGroups,
      accounts,
      personas
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
        ? visibleChatMessages(await api.listMessages(state.activeConversationId, messageLimit, previewChars))
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
      (preferredConversationId && conversations.some((item) => item.id === preferredConversationId)
        ? preferredConversationId
        : null)
      ?? (currentActive && conversations.some((item) => item.id === currentActive)
        ? currentActive
        : null)
      ?? (preferredPersonaId
        ? conversations.find((item) => item.personaId === preferredPersonaId)?.id ?? null
        : null)
      ?? conversations[0]?.id
      ?? null;
    const messageLimit = uiMessageLimit(state.config);
    const previewChars = uiMessagePreviewChars(state.config);
    const backendMessages = activeConversationId
      ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars))
      : [];
    const messages = mergeLocalUiMessages(backendMessages, state.messages, activeConversationId, messageLimit);
    const latestMessage = messages.at(-1);
    const shouldClearProcessing =
      Boolean(activeConversationId && latestMessage?.role === "assistant")
      && !withinProcessingGrace(activeConversationId);
    if (
      state.activeConversationId === activeConversationId
      && sameConversations(state.conversations, conversations)
      && sameMessages(state.messages, messages)
    ) {
      set((current) => ({
        agentQueue,
        processingConversationIds: shouldClearProcessing
          ? current.processingConversationIds.filter((id) => id !== activeConversationId)
          : current.processingConversationIds
      }));
      return;
    }
    set((current) => ({
      conversations,
      agentQueue,
      activeConversationId,
      messages,
      conversationUnreadCounts: current.conversationUnreadCounts,
      processingConversationIds: shouldClearProcessing
        ? current.processingConversationIds.filter((id) => id !== activeConversationId)
        : current.processingConversationIds
    }));
  },
  setConversationProcessing: (conversationId, processing) => {
    if (!conversationId) return;
    if (processing) {
      const clearTimer = processingClearTimerCache.get(conversationId);
      if (clearTimer !== undefined) {
        window.clearTimeout(clearTimer);
        processingClearTimerCache.delete(conversationId);
      }
      processingMarkedAtCache.set(conversationId, Date.now());
      set((state) => {
        if (state.processingConversationIds.includes(conversationId)) return state;
        return { processingConversationIds: [...state.processingConversationIds, conversationId] };
      });
      return;
    }
    const markedAt = processingMarkedAtCache.get(conversationId);
    const elapsed = markedAt === undefined ? PROCESSING_MARK_GRACE_MS : Date.now() - markedAt;
    const remaining = Math.max(0, PROCESSING_MARK_GRACE_MS - elapsed);
    const clearProcessing = () => {
      processingMarkedAtCache.delete(conversationId);
      processingClearTimerCache.delete(conversationId);
      set((state) => {
        if (!state.processingConversationIds.includes(conversationId)) return state;
        return {
          processingConversationIds: state.processingConversationIds.filter((id) => id !== conversationId)
        };
      });
    };
    const pendingTimer = processingClearTimerCache.get(conversationId);
    if (pendingTimer !== undefined) {
      window.clearTimeout(pendingTimer);
      processingClearTimerCache.delete(conversationId);
    }
    if (remaining <= 0) {
      clearProcessing();
      return;
    }
    const timer = window.setTimeout(clearProcessing, remaining);
    processingClearTimerCache.set(conversationId, timer);
  },
  incrementConversationUnread: (conversationId, amount = 1) => {
    if (!conversationId || amount <= 0) return;
    set((state) => ({
      conversationUnreadCounts: {
        ...state.conversationUnreadCounts,
        [conversationId]: (state.conversationUnreadCounts[conversationId] ?? 0) + amount
      }
    }));
  },
  markConversationRead: (conversationId) => {
    if (!conversationId) return;
    set((state) => {
      if (!(conversationId in state.conversationUnreadCounts)) return state;
      const unreadCounts = { ...state.conversationUnreadCounts };
      delete unreadCounts[conversationId];
      return { conversationUnreadCounts: unreadCounts };
    });
  },
  upsertIncomingMessage: (message) => {
    if (!isVisibleChatMessage(message)) return;
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
    const messages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
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
    const messages = visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars));
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
    const messages = activeConversationId ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars)) : [];
    const { [conversationId]: _, ...unreadCounts } = get().conversationUnreadCounts;
    set({ conversations, activeConversationId, messages, conversationUnreadCounts: unreadCounts });
  },
  selectConversation: async (conversationId) => {
    set((state) => {
      if (state.activeConversationId === conversationId) return state;
      const unreadCounts = { ...state.conversationUnreadCounts };
      delete unreadCounts[conversationId];
      return {
        activeConversationId: conversationId,
        messages: [],
        conversationUnreadCounts: unreadCounts
      };
    });
    const messageLimit = uiMessageLimit(get().config);
    const previewChars = uiMessagePreviewChars(get().config);
    const messages = visibleChatMessages(await api.listMessages(conversationId, messageLimit, previewChars));
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
        messages: visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars)),
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
        const unreadCounts = { ...state.conversationUnreadCounts };
        delete unreadCounts[conversation.id];
        set({
          conversations,
          activeConversationId: conversation.id,
          messages: visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars)),
          conversationUnreadCounts: unreadCounts,
          processingConversationIds: state.processingConversationIds.filter((id) => id !== state.activeConversationId)
        });
        return;
      }
    }
    let activeConversationId = state.activeConversationId;
    let activeConversation = state.conversations.find((item) => item.id === activeConversationId) ?? null;
    if (!activeConversationId || !activeConversation || (personaId && activeConversation.personaId !== personaId)) {
      const persona = personaId ? state.personas.find((item) => item.id === personaId) : null;
      const conversation = await api.createConversation(persona?.name, personaId);
      activeConversationId = conversation.id;
      activeConversation = conversation;
      const conversations = await api.listConversations();
      const messageLimit = uiMessageLimit(state.config);
      const previewChars = uiMessagePreviewChars(state.config);
      set({ conversations, activeConversationId, messages: visibleChatMessages(await api.listMessages(conversation.id, messageLimit, previewChars)) });
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
    const conversationIdForSend = activeConversationId;
    const personaIdForSend = personaId ?? activeConversation?.personaId ?? null;
    const requestedAgentId = agentId ?? activeConversation?.agentId ?? null;
    const agentIdForSend = requestedAgentId && state.agents.some((item) => item.id === requestedAgentId)
      ? requestedAgentId
      : null;
    // 异步发送消息，不阻塞 UI
    void (async () => {
      try {
        const responseMessages = await api.sendChatMessage({
          conversationId: conversationIdForSend,
          personaId: personaIdForSend,
          agentId: agentIdForSend,
          content: cleanContent
        });
        await Promise.allSettled([
          get().refreshAgentQueue(),
          get().refreshAgentRuns()
        ]);
        const visibleResponseMessages = visibleChatMessages(responseMessages ?? []);
        const hasAssistantReply = visibleResponseMessages.some((m) => m.role === "assistant" && m.content.trim());
        if (visibleResponseMessages.length > 0) {
          const messageLimit = uiMessageLimit(get().config);
          set((current) => {
            const backendUserIds = new Set(
              visibleResponseMessages.filter((m) => m.role === "user").map((m) => m.id)
            );
            const withoutTemp = current.messages.filter((m) => {
              if (m.conversationId !== conversationIdForSend) return true;
              if (m.role === "user" && isLocalUiMessage(m) && backendUserIds.size > 0) return false;
              if (hasAssistantReply && isLocalStatusMessage(m)) return false;
              return true;
            });
            const existingIds = new Set(withoutTemp.map((m) => m.id));
            const newMessages = visibleResponseMessages.filter((m) => !existingIds.has(m.id));
            const merged = [...withoutTemp, ...newMessages];
            return { messages: limitMessages(merged, messageLimit) };
          });
        }
        if (get().activeConversationId === conversationIdForSend) {
          await get().refreshChatData(conversationIdForSend, personaIdForSend);
        }
        const current = get();
        const hasNewerLocalUser = conversationIdForSend
          ? current.messages.some((message) =>
            message.conversationId === conversationIdForSend
            && message.role === "user"
            && isLocalUiMessage(message)
            && messageTime(message) > messageTime(temporaryMessage)
          )
          : false;
        const hasVisibleAssistant = conversationIdForSend
          ? current.messages.some((message) =>
            message.conversationId === conversationIdForSend
            && message.role === "assistant"
            && message.content.trim()
            && messageTime(message) >= messageTime(temporaryMessage) - 1000
          )
          : false;
        const pendingWork = hasPendingAgentWork(current, conversationIdForSend);
        if (!hasNewerLocalUser && (hasAssistantReply || hasVisibleAssistant || !pendingWork)) {
          current.setConversationProcessing(conversationIdForSend ?? "", false);
        }
        if (!hasNewerLocalUser && !hasAssistantReply && !hasVisibleAssistant && !pendingWork) {
          appendLocalAssistantNotice(set, conversationIdForSend, "本轮对话没有返回。", "desktop-local-empty");
        }
      } catch (error) {
        console.error("发送消息失败:", error);
        await Promise.allSettled([
          get().refreshAgentQueue(),
          get().refreshAgentRuns()
        ]);
        const current = get();
        const hasNewerLocalUser = conversationIdForSend
          ? current.messages.some((message) =>
            message.conversationId === conversationIdForSend
            && message.role === "user"
            && isLocalUiMessage(message)
            && messageTime(message) > messageTime(temporaryMessage)
          )
          : false;
        if (!hasNewerLocalUser) {
          current.setConversationProcessing(conversationIdForSend ?? "", false);
        }
        appendLocalAssistantNotice(set, conversationIdForSend, compactErrorMessage(error), "desktop-local-error");
      }
    })();
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
    const [agents, conversations] = await Promise.all([
      api.listAgents(),
      api.listConversations()
    ]);
    set((state) => ({
      personas: [saved, ...state.personas.filter((item) => item.id !== saved.id)]
        .sort((a, b) => a.name.localeCompare(b.name)),
      agents: agents
        .slice()
        .sort((a, b) => (b.isDefault ? 1 : 0) - (a.isDefault ? 1 : 0)),
      focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId),
      conversations
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
    set((state) => ({
      agents,
      focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId)
    }));
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
        .sort((a, b) => (b.isDefault ? 1 : 0) - (a.isDefault ? 1 : 0)),
      focusedAgentId: saved.id
    }));
    return saved;
  },
  deleteAgent: async (id) => {
    await api.deleteAgent(id);
    const state = get();
    const [agents, personas, conversations] = await Promise.all([
      api.listAgents(),
      api.listPersonas(),
      api.listConversations()
    ]);
    const currentActive = state.activeConversationId;
    const activeConversationId = currentActive && conversations.some((item) => item.id === currentActive)
      ? currentActive
      : conversations[0]?.id ?? null;
    const messageLimit = uiMessageLimit(state.config);
    const previewChars = uiMessagePreviewChars(state.config);
    const messages = activeConversationId
      ? visibleChatMessages(await api.listMessages(activeConversationId, messageLimit, previewChars))
      : [];
    set({
      agents,
      focusedAgentId: normalizeFocusedAgentId(agents, state.focusedAgentId === id ? null : state.focusedAgentId),
      personas,
      conversations,
      activeConversationId,
      messages
    });
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
