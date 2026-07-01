import { convertFileSrc as tauriConvertFileSrc, invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import type {
  ActionResult,
  AddHermesCredentialPoolEntryRequest,
  AgentAuxiliaryTaskAssignment,
  AgentAuxiliaryTaskSummary,
  AgentConfig,
  AgentControlCommand,
  AgentDefinition,
  AgentQueuedRequest,
  AgentRuntimeEventStream,
  AgentRunRecord,
  AgentTodoItem,
  AppBuildInfo,
  AppConfig,
  AppUpdateCheck,
  AppUpdateInstallResult,
  BrowserProvider,
  ChatAttachment,
  ChatMessage,
  Conversation,
  DetectedModelList,
  EmojiGroup,
  EnvCheckResult,
  HermesCredentialPoolEntryStatus,
  ImageProvider,
  KanbanDispatchDrainResult,
  LlmProvider,
  ManagedProcessSnapshot,
  ConversationDeleteMemorySettlingResult,
  MemoryEntry,
  MemoryStatus,
  ModelCatalogEntry,
  ModelCapabilities,
  Persona,
  PluginAuxiliaryTaskSummary,
  ProfileConfig,
  ProactiveStatus,
  SearchProvider,
  ScheduledAgentJob,
  ScheduledJobOutputRecord,
  SendChatRequest,
  StateSnapshotManifest,
  StateSnapshotRestoreResult,
  TokenUsageStats,
  TokenUsageResponse,
  ToolArtifactRecord,
  VideoProvider,
  VisionProvider,
  WorkspaceSnapshotManifest,
  WorkspaceSnapshotRestoreResult,
  AccountConfig,
  WechatConfig,
  WechatInboundResult,
  WechatLinkSummary,
  WechatPollResult,
  WechatQrStartResult,
  WechatQrStatusResult
} from "./types";

const fallbackConfig: AppConfig = {
  logLevel: "info",
  chat: {
    skipEnvCheck: true,
    agentEngine: "standalone-mock",
    maxContextRounds: 10,
    shortContextMode: "messages",
    shortContextTokenBudget: 8000,
    shortContextAbortOnSummaryFailure: false,
    shortContextSummaryProviderId: "",
    shortContextSummaryModel: "",
    busyInputMode: "queue",
    autoTitleEnabled: true,
    queueWaitSeconds: 7,
    delegationMaxConcurrentChildren: 3,
    delegationStrategy: "auto",
    delegationOrchestratorEnabled: true,
    delegationSubagentAutoApprove: false,
    delegationInheritMcpToolsets: true,
    delegationSubagentProviderId: "",
    delegationSubagentModel: "",
    auxiliaryTaskAssignments: {},
    agentRunTimeoutSeconds: 600,
    uiMessageLimit: 180,
    artifactScanLimit: 80,
    uiMessagePreviewChars: 12000,
    uiStreamCharsPerSecond: 36,
    thinkingMinVisibleMs: 1800,
    petCloudDurationSeconds: 10,
    bottomFollowThresholdPx: 180,
    activePollIntervalMs: 1500,
    idlePollIntervalMs: 3000,
    intentAnalyzerMode: "embedding",
    toolRouterMode: "llm_unified",
    toolUseEnforcement: "auto",
    toolParallelEnabled: true,
    toolParallelLimit: 4,
    sendMessageToolEnabled: false,
    toolApprovalMode: "risky",
    cronApprovalMode: "deny",
    trustedToolPatterns: [],
    trustedCommandPatterns: [],
    hooks: {},
    hooksAutoAccept: false,
    llmCredentialPoolStrategy: "fill_first",
    toolEnvPassthrough: [],
    toolMutationCheckpointEnabled: true,
    llmRetryCount: 2,
    llmRetryBackoffMs: 800,
    responsesReasoningReplayEnabled: true,
    fastModeEnabled: false,
    runtimeFooterEnabled: false,
    statusbarEnabled: true,
    toolProgressDisplay: "new",
    displaySkin: "default",
    busyIndicatorStyle: "unicode",
    codexRuntime: "auto",
    toolCallRetryCount: 1,
    toolCallRetryBackoffMs: 300,
    toolGuardrailWarningsEnabled: true,
    toolGuardrailHardStopEnabled: false,
    toolGuardrailExactFailureWarnAfter: 2,
    toolGuardrailSameToolFailureWarnAfter: 3,
    toolGuardrailNoProgressWarnAfter: 2,
    toolGuardrailExactFailureLimit: 5,
    toolGuardrailSameToolFailureLimit: 8,
    toolGuardrailNoProgressLimit: 5,
    backgroundMemoryReviewEnabled: true,
    backgroundMemoryReviewMinMessages: 4,
    backgroundSkillReviewEnabled: true,
    backgroundSkillReviewAutoCreateEnabled: false,
    backgroundSkillCuratorEnabled: true,
    backgroundSkillCuratorIntervalHours: 168,
    skillHotReloadEnabled: true,
    skillHotReloadIntervalSeconds: 3,
    historyCleanupEnabled: true,
    historyRetentionDays: 14,
    maxStoredMessagesPerConversation: 300,
    maxStoredAgentRuns: 50,
    maxStoredToolTraces: 100
  },
  reply: {
    typingDelayEnabled: true,
    typingSpeed: 0.2,
    typingSpeedRandomMin: 0.05,
    typingSpeedRandomMax: 0.1,
    splitByNewline: true,
    showTypingIndicator: true,
    typingIndicatorRefreshSeconds: 2
  },
  web: { port: 62000, password: "", publicEnabled: false, publicPort: 0, publicSecret: "" },
  weather: { defaultLocation: "", qweatherApiHost: "", qweatherApiKey: "", timeoutSeconds: 15 },
  moments: { autoReplyEnabled: false, publishers: [], repliers: [] },
  videoSummary: {
    enabled: false,
    modelsDir: "",
    transcriber: "auto",
    ytDlpCommand: "yt-dlp",
    cookie: "",
    cookieFile: "",
    ffmpegBinPath: "",
    fasterWhisperModel: "small",
    fasterWhisperModelDir: "",
    fasterWhisperDevice: "cpu",
    fasterWhisperComputeType: "int8",
    senseVoiceModelDir: "",
    senseVoiceDevice: "cpu",
    timeoutSeconds: 30,
    ytdlpInfoTimeoutSeconds: 120,
    downloadTimeoutSeconds: 600,
    outputDir: ""
  },
  telemetryEnabled: false
};

const fallbackProfile: ProfileConfig = { name: "用户", avatarPath: "" };

const defaultPersona: Persona = {
  id: "default",
  name: "小可",
  agentId: "default",
  systemPrompt: "你是一个友好、稳定的聊天助手。",
  systemInstructions: "请始终保持角色一致性。",
  characterPrompt: "",
  outputExamples: "",
  llmProvider: "",
  llmModel: "",
  avatarPath: "",
  temperature: 0.8,
  maxTokens: 2048,
  emojiEnabled: true,
  emojiGroup: "default",
  emojiSendProbability: 25,
  toolPolicy: { enabled: true, timeoutSeconds: 30, maxIterations: 90, maxFailureReplans: 2, retryCount: 1, retryBackoffMs: 300 },
  memory: { enabled: true, triggerRounds: 10, maxMemories: 50, includeInPrompt: true },
  proactive: { enabled: false, minIdleHours: 1, maxIdleHours: 3, maxConsecutive: 3, prompt: "", quietHours: { enabled: true, start: "22:00", end: "08:00" } },
  voiceReply: {
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
  },
  imageGeneration: { enabled: false, provider: "", model: "", stylePrefix: "", artStyle: "anime style, masterpiece, best quality", negativePrompt: "low quality, blurry, watermark, text, signature, lowres, bad anatomy, extra fingers, jpeg artifacts", negativeEnabled: true, refMode: "avatar" }
};

const defaultAgent = (): AgentDefinition => ({
  id: "default",
  name: "默认智能体",
  description: "SynthChat Rust 对话智能体",
  workspaceDir: "",
  llmProvider: "",
  llmModel: "",
  enabled: true,
  isDefault: true,
  mcpEnabled: true,
  skillsEnabled: true,
  allowShell: true,
  maxSubagents: 4,
  maxSubagentDepth: 1,
  maxToolIterations: 90,
  skillsDir: "",
  enabledSkills: [],
  enabledMcpServers: [],
  enabledToolsets: [],
  disabledToolsets: [],
  createdAt: new Date().toISOString(),
  updatedAt: new Date().toISOString()
});

export function isTauri(): boolean {
  return typeof window !== "undefined" && ("__TAURI_INTERNALS__" in window || "__TAURI__" in window);
}

async function call<T>(cmd: string, args: Record<string, unknown> = {}, fallback: () => T | Promise<T>): Promise<T> {
  if (isTauri()) {
    return invoke<T>(cmd, args);
  }
  return fallback();
}

function dialogSelectionToPath(selection: unknown): string | null {
  if (typeof selection === "string") return selection;
  if (Array.isArray(selection)) return dialogSelectionToPath(selection[0]);
  if (selection && typeof selection === "object") {
    const path = (selection as { path?: unknown }).path;
    return typeof path === "string" ? path : null;
  }
  return null;
}

function ok(message = "standalone mock"): ActionResult {
  return { success: true, message };
}

export async function getAppBuildInfo(): Promise<AppBuildInfo> {
  return call("get_app_build_info", {}, () => ({
    productName: "SynthChat",
    version: "1.1.0",
    identifier: "cc.synthchat.v1",
    target: "web-preview",
    updateManifestUrl: ""
  }));
}

export async function checkAppUpdate(manifestUrl?: string): Promise<AppUpdateCheck> {
  return call("check_app_update", { manifestUrl }, () => ({
    currentVersion: "1.1.0",
    latestVersion: "1.1.0",
    updateAvailable: false,
    downloadUrl: null,
    releaseUrl: null,
    notes: null,
    publishedAt: null,
    sourceUrl: manifestUrl ?? "",
    checkedAt: new Date().toISOString()
  }));
}

export async function installAppUpdate(downloadUrl: string): Promise<AppUpdateInstallResult> {
  return call("install_app_update", { downloadUrl }, () => ({
    installerPath: "",
    helperScriptPath: "",
    mode: "unavailable",
    message: "Native updater is unavailable in web preview."
  }));
}

export async function openAppUpdateUrl(url: string): Promise<void> {
  return call("open_app_update_url", { url }, () => {
    if (typeof window !== "undefined") {
      window.open(url, "_blank", "noopener,noreferrer");
    }
  });
}

export function convertFileSrc(path: string): string {
  if (!path) return "";
  return isTauri() ? tauriConvertFileSrc(path) : path;
}

export async function getConfig(): Promise<AppConfig> {
  return call("get_config", {}, () => fallbackConfig);
}

export async function saveConfig(config: AppConfig): Promise<void> {
  return call("save_config", { config }, () => undefined);
}

export async function addTrustedToolPattern(pattern: string): Promise<AppConfig> {
  const normalized = pattern.trim();
  return call("add_trusted_tool_pattern", { pattern: normalized }, () => ({
    ...fallbackConfig,
    chat: {
      ...fallbackConfig.chat,
      trustedToolPatterns: Array.from(new Set([...(fallbackConfig.chat.trustedToolPatterns ?? []), normalized].filter(Boolean)))
    }
  }));
}

export async function removeTrustedToolPattern(pattern: string): Promise<AppConfig> {
  return call("remove_trusted_tool_pattern", { pattern }, () => ({
    ...fallbackConfig,
    chat: {
      ...fallbackConfig.chat,
      trustedToolPatterns: (fallbackConfig.chat.trustedToolPatterns ?? []).filter((item) => item !== pattern)
    }
  }));
}

export async function addHermesCredentialPoolEntry(
  request: AddHermesCredentialPoolEntryRequest
): Promise<HermesCredentialPoolEntryStatus> {
  return call("add_hermes_credential_pool_entry", { ...request }, () => ({
    providerId: request.provider,
    index: 1,
    label: request.label?.trim() || "api-key-1",
    authType: request.authType || "api_key",
    source: "manual",
    state: "mock",
    expiresAt: request.expiresAt,
    baseUrl: request.baseUrl
  }));
}

export async function listStateSnapshots(): Promise<StateSnapshotManifest[]> {
  return call("list_state_snapshots", {}, () => []);
}

export async function createStateSnapshot(label: string): Promise<StateSnapshotManifest> {
  return call("create_state_snapshot", { label }, () => ({
    id: `mock-${Date.now()}`,
    label,
    createdAt: new Date().toISOString(),
    statePath: ""
  }));
}

export async function pruneStateSnapshots(keep: number): Promise<number> {
  return call("prune_state_snapshots", { keep }, () => 0);
}

export async function restoreStateSnapshot(snapshotId: string): Promise<StateSnapshotRestoreResult> {
  return call("restore_state_snapshot", { snapshotId }, () => ({
    restored: { id: snapshotId, label: "mock restore", createdAt: new Date().toISOString(), statePath: "" },
    preRestore: { id: `pre-${Date.now()}`, label: `pre-restore ${snapshotId}`, createdAt: new Date().toISOString(), statePath: "" }
  }));
}

export async function listWorkspaceSnapshots(): Promise<WorkspaceSnapshotManifest[]> {
  return call("list_workspace_snapshots", {}, () => []);
}

export async function createWorkspaceSnapshot(label: string): Promise<WorkspaceSnapshotManifest> {
  return call("create_workspace_snapshot", { label }, () => ({
    id: `mock-ws-${Date.now()}`,
    label,
    createdAt: new Date().toISOString(),
    root: "",
    snapshotPath: "",
    fileCount: 0,
    totalBytes: 0,
    skippedFiles: 0,
    skippedDirs: 0
  }));
}

export async function restoreWorkspaceSnapshot(snapshotId: string, deleteNewFiles: boolean): Promise<WorkspaceSnapshotRestoreResult> {
  return call("restore_workspace_snapshot", { snapshotId, deleteNewFiles }, () => ({
    restored: { id: snapshotId, label: "mock workspace restore", createdAt: new Date().toISOString(), root: "" },
    preRestore: { id: `pre-ws-${Date.now()}`, label: `pre-restore ${snapshotId}`, createdAt: new Date().toISOString(), root: "" },
    restoredFiles: 0,
    removedNewFiles: 0,
    deleteNewFiles
  }));
}

export async function getProfile(): Promise<ProfileConfig> {
  return call("get_profile", {}, () => fallbackProfile);
}

export async function saveProfile(profile: ProfileConfig): Promise<ProfileConfig> {
  return call("save_profile", { profile }, () => profile);
}

export async function listPersonas(): Promise<Persona[]> {
  return call("list_personas", {}, () => [defaultPersona]);
}

export async function getPersona(id: string): Promise<Persona> {
  return call("get_persona", { id }, () => ({ ...defaultPersona, id }));
}

export async function savePersona(persona: Persona): Promise<Persona> {
  return call("save_persona", { persona }, () => persona);
}

export async function listConversations(): Promise<Conversation[]> {
  return call("list_conversations", {}, () => []);
}

export async function createConversation(title?: string, personaId?: string): Promise<Conversation> {
  return call("create_conversation", { title, personaId }, () => ({
    id: `conv-${Date.now()}`,
    title: title || "新会话",
    personaId: personaId || "default",
    agentId: "default",
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    lastMessage: ""
  }));
}

export async function deleteConversation(id: string): Promise<ConversationDeleteMemorySettlingResult> {
  return call("delete_conversation", { id }, () => ({
    status: "skipped",
    reason: "desktop api unavailable",
    memoryCount: 0
  }));
}

export async function renameConversation(id: string, title: string): Promise<void> {
  return call("rename_conversation", { id, title }, () => undefined);
}

export async function setConversationAgent(id: string, agentId: string): Promise<Conversation> {
  return call("set_conversation_agent", { id, agentId }, () => ({
    id,
    title: "当前会话",
    personaId: undefined,
    agentId,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    lastMessage: ""
  }));
}

export async function listMessages(conversationId: string, limit?: number, previewChars?: number): Promise<ChatMessage[]> {
  return call("list_messages", { conversationId, limit, previewChars }, () => []);
}

export async function sendChatMessage(request: SendChatRequest): Promise<ChatMessage[]> {
  return call("send_chat_message", { request }, () => {
    const conversationId = request.conversationId || `conv-${Date.now()}`;
    const now = new Date().toISOString();
    return [
      { id: `msg-${Date.now()}`, conversationId, role: "user", content: request.content, createdAt: now, source: "desktop" },
      { id: `msg-${Date.now() + 1}`, conversationId, role: "assistant", content: `收到：${request.content}`, createdAt: now, source: "desktop-stream" }
    ];
  });
}

export async function listLlmProviders(): Promise<LlmProvider[]> {
  return call("list_llm_providers", {}, () => [{
    id: "local-echo",
    name: "本地回显",
    providerType: "echo",
    preset: "echo",
    baseUrl: "",
    appendChatPath: true,
    apiKeyEnv: "",
    apiKey: "",
    model: "echo",
    enabled: true,
    timeoutSeconds: 60,
    promptCacheMode: "auto",
    promptCacheTtl: "5m",
    promptCacheLayout: "auto"
  }]);
}

export async function saveLlmProviders(providers: LlmProvider[]): Promise<void> {
  return call("save_llm_providers", { providers }, () => undefined);
}

export async function getTokenUsageStats(): Promise<TokenUsageResponse> {
  return call("get_token_usage_stats", {}, () => ({ promptTokens: 0, completionTokens: 0, totalTokens: 0, callCount: 0, byProvider: {}, byModel: {} }));
}

export async function listAgenticModels(providerId: string): Promise<ModelCatalogEntry[]> {
  return call("list_agentic_models", { providerId }, () => []);
}

export async function detectProviderModels(provider: LlmProvider): Promise<DetectedModelList> {
  return call("detect_provider_models", { provider }, () => ({
    ok: false,
    source: "catalog",
    providerId: provider.id,
    providerType: provider.providerType || "",
    baseUrl: provider.baseUrl || "",
    models: [],
    error: "model detection unavailable"
  }));
}

export async function inferProviderModelCapabilities(provider: LlmProvider): Promise<ModelCapabilities> {
  return call("infer_provider_model_capabilities", { provider }, () => ({
    provider_id: provider.id,
    model_id: provider.model,
    supports_tools: provider.providerType !== "echo",
    supports_vision: false,
    supports_reasoning: false,
    supports_pdf: false,
    supports_audio_input: false,
    supports_structured_output: provider.providerType !== "echo",
    input_modalities: ["text"],
    output_modalities: ["text"],
    source: "fallback"
  }));
}

export async function environmentCheck(): Promise<EnvCheckResult> {
  return call("environment_check", {}, () => ({
    items: [{ id: "frontend", name: "前端预览", status: "ok", detail: "Standalone mock mode." }],
    allPassed: true
  }));
}

const empty = async <T,>(): Promise<T[]> => [];
const pass = async <T,>(value: T): Promise<T> => value;

export const api: Record<string, any> = {
  getAppBuildInfo,
  checkAppUpdate,
  installAppUpdate,
  openAppUpdateUrl,
  getConfig,
  saveConfig,
  addTrustedToolPattern,
  removeTrustedToolPattern,
  listStateSnapshots,
  createStateSnapshot,
  pruneStateSnapshots,
  restoreStateSnapshot,
  listWorkspaceSnapshots,
  createWorkspaceSnapshot,
  restoreWorkspaceSnapshot,
  getProfile,
  saveProfile,
  listPersonas,
  getPersona,
  savePersona,
  deletePersona: (id: string) => call("delete_persona", { id }, () => undefined),
  listConversations,
  createConversation,
  deleteConversation,
  renameConversation,
  setConversationAgent,
  listMessages,
  sendChatMessage,
  deleteMessage: (messageId: string) => call("delete_message", { messageId }, () => undefined),
  listLlmProviders,
  saveLlmProviders,
  listMcpServers: () => call("list_mcp_servers", {}, () => []),
  saveMcpServers: (servers: unknown[]) => call("save_mcp_servers", { servers }, () => undefined),
  listAgentRuns: () => call<AgentRunRecord[]>("list_agent_runs", {}, () => []),
  listAgentRuntimeEvents: (options: {
    conversationId?: string | null;
    runId?: string | null;
    queueItemId?: string | null;
    taskId?: string | null;
    board?: string | null;
    since?: number;
    limit?: number;
  } = {}) => call<AgentRuntimeEventStream>("list_agent_runtime_events", options, () => ({
    schema: "hermes_kanban_runtime_events_desktop_v1",
    status: "ok",
    action: "kanban-runtime-events",
    events: [],
    cursor: options.since ?? 0,
    count: 0,
    total: 0,
    since: options.since ?? 0,
    limit: options.limit ?? 80,
    pollIntervalMs: 300,
    websocketEmbedded: false,
    nativeRuntimeEventBridge: false,
    sources: []
  })),
  listManagedProcesses: () => call<ManagedProcessSnapshot[]>("list_managed_processes", {}, () => []),
  stopManagedProcess: (processId: string, forget = false) =>
    call<ManagedProcessSnapshot>("stop_managed_process", { processId, forget }, () => ({
      id: processId,
      status: "stopped",
      finishedAt: new Date().toISOString()
    })),
  browserRuntimeStatus: () => call<Record<string, unknown>>("browser_runtime_status", {}, () => ({})),
  computerUseRuntimeStatus: () => call<Record<string, unknown>>("computer_use_runtime_status", {}, () => ({})),
  listAgentControlCommands: () => call<AgentControlCommand[]>("list_agent_control_commands", {}, () => []),
  listAgentQueue: () => call<AgentQueuedRequest[]>("list_agent_queue", {}, () => []),
  cancelAgentQueueItem: (id: string) => call<AgentQueuedRequest>("cancel_agent_queue_item", { id }, () => ({
    id,
    conversationId: "",
    personaId: "",
    userMessageId: "",
    content: "",
    status: "canceled",
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    completedAt: new Date().toISOString(),
    error: "cancel_agent_queue_item unavailable"
  })),
  clearFinishedAgentQueueItems: () => call<AgentQueuedRequest[]>("clear_finished_agent_queue_items", {}, () => []),
  listAgentTodos: () => call<AgentTodoItem[]>("list_agent_todos", {}, () => []),
  listScheduledAgentJobs: () => call<ScheduledAgentJob[]>("list_scheduled_agent_jobs", {}, () => []),
  listScheduledJobOutputs: (jobId: string) => call<ScheduledJobOutputRecord[]>("list_scheduled_job_outputs", { jobId }, () => []),
  saveScheduledAgentJob: (job: ScheduledAgentJob) => call<ScheduledAgentJob>("save_scheduled_agent_job", { job }, () => job),
  deleteScheduledAgentJob: (id: string) => call("delete_scheduled_agent_job", { id }, () => undefined),
  setScheduledAgentJobEnabled: (id: string, enabled: boolean) => call<ScheduledAgentJob>("set_scheduled_agent_job_enabled", { id, enabled }, () => ({
    id,
    name: "",
    personaId: "default",
    prompt: "",
    scheduleKind: "once",
    enabledToolsets: [],
    disabledToolsets: [],
    enabled,
    status: enabled ? "scheduled" : "paused",
    lastCompletedAt: null,
    lastRunStatus: null,
    lastOutput: null,
    lastOutputPath: null,
    lastError: null,
    runCount: 0,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString()
  })),
  tickScheduledAgentJobs: () => call<ScheduledAgentJob[]>("tick_scheduled_agent_jobs", {}, () => []),
  exportAgentRunBundle: (runId: string) => call<string>("export_agent_run_bundle", { runId }, () => ""),
  listToolArtifactsForRun: (runId: string) => call<ToolArtifactRecord[]>("list_tool_artifacts_for_run", { runId }, () => []),
  drainAgentQueue: () => call<AgentQueuedRequest[]>("drain_agent_queue", {}, () => []),
  dispatchKanbanAndDrainAgentQueue: (payload: Record<string, unknown> = {}) =>
    call<KanbanDispatchDrainResult>("dispatch_kanban_and_drain_agent_queue", { payload }, () => ({
      schema: "hermes_kanban_dispatch_drain_desktop_v1",
      status: "unavailable",
      action: "kanban-dispatch-drain",
      dispatch: {},
      drainRequested: false,
      drained: [],
      drainedCount: 0,
      nativeDispatcherDrainBridge: false
    })),
  startMattermostAdapter: () => call<Record<string, unknown>>("start_mattermost_adapter", {}, () => ({ platform: "mattermost", status: "unavailable" })),
  stopMattermostAdapter: () => call<Record<string, unknown>>("stop_mattermost_adapter", {}, () => ({ platform: "mattermost", status: "stopped" })),
  mattermostAdapterStatus: () => call<Record<string, unknown>>("mattermost_adapter_status", {}, () => ({ platform: "mattermost", status: "stopped" })),
  startPlatformAdapter: (platform: string) => call<Record<string, unknown>>("start_platform_adapter", { platform }, () => ({ platform, status: "unavailable" })),
  stopPlatformAdapter: (platform: string) => call<Record<string, unknown>>("stop_platform_adapter", { platform }, () => ({ platform, status: "stopped" })),
  platformAdapterStatus: (platform?: string | null) => call<Record<string, unknown>>("platform_adapter_status", { platform }, () => ({ adapters: [] })),
  resumeAgentRun: (runId: string, checkpointId?: string | null) => call<AgentRunRecord>("resume_agent_run", { runId, checkpointId }, () => ({
    runId,
    conversationId: "",
    personaId: "",
    agentId: "",
    parentRunId: null,
    subagentIndex: null,
    subagentDepth: null,
    subagentCanDelegate: null,
    subagentRole: null,
    subagentTask: null,
    subagentToolsets: [],
    userRequest: "",
    state: "failed",
    toolEvents: [],
    phaseEvents: [],
    checkpoints: [],
    error: "resume_agent_run unavailable",
    startedAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    lastActivityAt: new Date().toISOString(),
    lastActivityDesc: "resume fallback",
    completedAt: null
  })),
  rerunAgentRun: (runId: string) => call<ChatMessage[]>("rerun_agent_run", { runId }, () => []),
  diagnoseAgentRun: (runId: string) => call<ChatMessage>("diagnose_agent_run", { runId }, () => ({
    id: `diagnosis-${Date.now()}`,
    conversationId: "",
    role: "assistant",
    content: "diagnose_agent_run unavailable",
    source: "standalone",
    createdAt: new Date().toISOString()
  })),
  abortAgentRun: (runId: string, reason?: string) => call<AgentRunRecord>("abort_agent_run", { runId, reason }, () => ({
    runId,
    conversationId: "",
    personaId: "",
    agentId: "",
    parentRunId: null,
    subagentIndex: null,
    subagentDepth: null,
    subagentCanDelegate: null,
    subagentRole: null,
    subagentTask: null,
    subagentToolsets: [],
    userRequest: "",
    state: "aborted",
    toolEvents: [],
    phaseEvents: [],
    checkpoints: [],
    error: reason ?? "abort_agent_run unavailable",
    startedAt: new Date().toISOString(),
    updatedAt: new Date().toISOString(),
    lastActivityAt: new Date().toISOString(),
    lastActivityDesc: "abort fallback",
    completedAt: new Date().toISOString()
  })),
  listAgents: () => call<AgentDefinition[]>("list_agents", {}, () => [defaultAgent()]),
  saveAgent: (agent: AgentDefinition) => call("save_agent", { agent }, () => agent),
  autoDescribeAgent: (agentId?: string, overwrite = false) =>
    call<AgentDefinition>("auto_describe_agent", { agentId, overwrite }, () => defaultAgent()),
  deleteAgent: (id: string) => call("delete_agent", { id }, () => undefined),
  getAgentConfig: () => call<AgentConfig>("get_agent_config", {}, () => ({
    enabled: true,
    mcpEnabled: true,
    skillsEnabled: true,
    enabledMcpServers: [],
    enabledToolsets: [],
    disabledToolsets: [],
    enabledSkills: [],
    maxSubagents: 4,
    maxSubagentDepth: 1,
    maxToolIterations: 90,
    allowShell: true,
    skillsDir: ""
  })),
  saveAgentConfig: (config: AgentConfig) => call("save_agent_config", { config }, () => config),
  listSkills: () => call("list_skills", {}, () => []),
  listSkillsForAgent: (agentId: string) => call("list_skills_for_agent", { agentId }, () => []),
  installBuiltinSkills: () => call("install_builtin_skills", {}, () => []),
  listMemories: (personaId?: string) => call<MemoryEntry[]>("list_memories", { personaId }, () => []),
  getMemoryStatus: (personaId?: string) => call<MemoryStatus>("get_memory_status", { personaId }, () => ({
    personaId: personaId || "default",
    personaName: "default",
    enabled: true,
    includeInPrompt: true,
    triggerRounds: 10,
    maxMemories: 50,
    total: 0,
    promptSafe: 0,
    blockedBySecurityScan: 0,
    promptInjected: 0
  })),
  saveMemory: (memory: Partial<MemoryEntry> & { personaId: string; summary: string; importance: number; target?: string }) => call("save_memory", { memory }, () => memory),
  deleteMemory: (id: string) => call("delete_memory", { id }, () => undefined),
  listWorldbooks: () => call("list_worldbooks", {}, () => []),
  saveWorldbook: (book: unknown) => call("save_worldbook", { book }, () => book),
  deleteWorldbook: (id: string) => call("delete_worldbook", { id }, () => undefined),
  listThemes: () => call("list_themes", {}, () => [{
    id: "default-light",
    name: "默认浅色",
    mode: "light",
    css: "",
    active: true,
    createdAt: new Date().toISOString(),
    updatedAt: new Date().toISOString()
  }]),
  saveThemes: (themes: unknown[]) => call("save_themes", { themes }, () => themes),
  getTokenUsageStats,
  resetTokenUsage: () => ok("已重置"),
  listAgenticModels,
  detectProviderModels,
  inferProviderModelCapabilities,
  environmentCheck,
  checkEnvironment: environmentCheck,
  getShortContextState: (conversationId: string) => call("get_short_context_state", { conversationId }, () => ({
    conversationId,
    boundaryId: null,
    summary: "",
    summaryTokens: 0,
    summaryMessages: 0
  })),
  transcribeChatAudio: (dataUrl: string, mimeType?: string): Promise<{ ok: boolean; transcript: string; provider?: unknown; providerId?: unknown; mimeType?: string }> =>
    call("transcribe_chat_audio", { dataUrl, mimeType }, () => ({ ok: false, transcript: "" })),
  speakChatText: (text: string, options?: { providerId?: string; language?: string; voice?: string; volume?: string; pitch?: string; format?: string; engine?: string; speedScale?: string; speed?: number; modelDir?: string; pythonPath?: string; sampleRate?: number; oral?: number; laugh?: number; breakLevel?: number; speakerSeed?: number; speakerEmbedding?: string; temperature?: number; topP?: number; topK?: number; refineTextEnabled?: boolean; refinePrompt?: string; refineTemperature?: number }): Promise<{ ok: boolean; dataUrl: string; mimeType?: string; provider?: unknown; providerId?: unknown; voice?: unknown; format?: string; actualFormat?: string; voiceCompatible?: boolean; mediaTag?: string; conversion?: unknown; artifact?: { path?: string; sizeBytes?: number } }> =>
    call("speak_chat_text", {
      text,
      providerId: options?.providerId,
      language: options?.language,
      voice: options?.voice,
      volume: options?.volume,
      pitch: options?.pitch,
      format: options?.format,
      engine: options?.engine,
      speedScale: options?.speedScale,
      speed: options?.speed,
      modelDir: options?.modelDir,
      pythonPath: options?.pythonPath,
      sampleRate: options?.sampleRate,
      oral: options?.oral,
      laugh: options?.laugh,
      breakLevel: options?.breakLevel,
      speakerSeed: options?.speakerSeed,
      speakerEmbedding: options?.speakerEmbedding,
      temperature: options?.temperature,
      topP: options?.topP,
      topK: options?.topK,
      refineTextEnabled: options?.refineTextEnabled,
      refinePrompt: options?.refinePrompt,
      refineTemperature: options?.refineTemperature
    }, () => ({ ok: false, dataUrl: "", artifact: {} })),
  playChatAudio: (path: string): Promise<Record<string, unknown>> =>
    call("play_chat_audio", { path }, () => ({ action: "voice_playback", status: "unavailable", path })),
  stopChatAudio: (): Promise<Record<string, unknown>> =>
    call("stop_chat_audio", {}, () => ({ action: "voice_playback", status: "stopped", stopped: false })),
  getMessageContent: async (messageId: string) => messageId,
  assetUrl: convertFileSrc,
  convertFileSrc,
  openLocalFile: (path: string) => call("open_local_file", { path }, () => undefined),
  revealLocalFile: (path: string) => call("reveal_local_file", { path }, () => undefined),
  openPetWindow: () => call("open_pet_window", {}, () => undefined),
  uploadChatAttachment: (fileName: string, mimeType: string, bytes: number[]): Promise<ChatAttachment> => call<ChatAttachment>("upload_chat_attachment", { fileName, mimeType, bytes }, () => ({
    id: `att-${Date.now()}`,
    fileName,
    mimeType,
    fileSize: bytes.length,
    path: fileName
  })),
  uploadChatAttachmentFromPath: (path: string): Promise<ChatAttachment> => call<ChatAttachment>("upload_chat_attachment_from_path", { path }, () => ({
    id: `att-${Date.now()}`,
    fileName: path.split(/[\\/]/).pop() || "attachment",
    mimeType: "application/octet-stream",
    fileSize: 0,
    path
  })),
  listMoments: () => empty(),
  createMoment: (body: string) => pass({ id: `moment-${Date.now()}`, personaId: "default", body, likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  updateMomentText: (_postId: string, body: string) => pass({ id: _postId, personaId: "default", body, likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  addMomentComment: (postId: string, text: string) => pass({ id: postId, personaId: "default", body: "", likedBy: [], comments: [{ id: `comment-${Date.now()}`, personaId: "default", text, createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  updateMomentComment: (postId: string) => pass({ id: postId, personaId: "default", body: "", likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  deleteMoment: async () => undefined,
  deleteMomentComment: (postId: string) => pass({ id: postId, personaId: "default", body: "", likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  likeMoment: (postId: string) => pass({ id: postId, personaId: "default", body: "", likedBy: ["user"], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  unlikeMoment: (postId: string) => pass({ id: postId, personaId: "default", body: "", likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  uploadMomentCover: (postId: string) => pass({ id: postId, personaId: "default", body: "", coverPath: "", likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  clearMomentCover: (postId: string) => pass({ id: postId, personaId: "default", body: "", likedBy: [], comments: [], createdAt: new Date().toISOString(), updatedAt: new Date().toISOString() }),
  listCapabilityAdapters: () => call("list_capability_adapters", {}, () => []),
  saveCapabilityAdapters: (adapters: unknown[]) => call("save_capability_adapters", { adapters }, () => adapters),
  listProactiveStatuses: () => call<ProactiveStatus[]>("list_proactive_statuses", {}, () => []),
  triggerProactiveOnce: (personaId: string) =>
    call<ProactiveStatus>("trigger_proactive_once", { personaId }, () => ({
      personaId,
      personaName: "",
      enabled: false,
      conversationId: null,
      lastUserAt: 0,
      secondsSinceLastUser: 0,
      lastReplyAt: 0,
      secondsSinceLastReply: 0,
      waitSeconds: 0,
      readyInSeconds: 0,
      consecutiveCount: 0,
      maxConsecutive: 1,
      inQuietHours: false,
      petVisionSuspended: false,
      canFire: false,
      blockedReason: "主动消息后端不可用"
    })),
  listAccounts: () => call<AccountConfig[]>("list_accounts", {}, () => []),
  saveAccounts: (accounts: AccountConfig[]) => call("save_accounts", { accounts }, () => undefined),
  linkWechatAccount: (personaId: string, accountId: string) =>
    call<AccountConfig[]>("link_wechat_account", { personaId, accountId }, () => []),
  unlinkWechatAccount: (personaId: string) =>
    call<AccountConfig[]>("unlink_wechat_account", { personaId }, () => []),
  listImageProviders: () => call<ImageProvider[]>("list_image_providers", {}, () => []),
  saveImageProviders: (providers: ImageProvider[]) => call("save_image_providers", { providers }, () => undefined),
  listVideoProviders: () => call<VideoProvider[]>("list_video_providers", {}, () => []),
  saveVideoProviders: (providers: VideoProvider[]) => call("save_video_providers", { providers }, () => undefined),
  listSearchProviders: () => call<SearchProvider[]>("list_search_providers", {}, () => []),
  saveSearchProviders: (providers: SearchProvider[]) => call("save_search_providers", { providers }, () => undefined),
  listVisionProviders: () => call<VisionProvider[]>("list_vision_providers", {}, () => []),
  saveVisionProviders: (providers: VisionProvider[]) => call("save_vision_providers", { providers }, () => undefined),
  listBrowserProviders: () => call<BrowserProvider[]>("list_browser_providers", {}, () => []),
  saveBrowserProviders: (providers: BrowserProvider[]) => call("save_browser_providers", { providers }, () => undefined),
  listEmojiGroups: () => call<EmojiGroup[]>("list_emoji_groups", {}, () => []),
  saveEmojiGroups: (groups: EmojiGroup[]) => call("save_emoji_groups", { groups }, () => undefined),
  uploadEmojiImage: (groupId: string, emotion: string, fileName: string, bytes: number[]) =>
    call<EmojiGroup[]>("upload_emoji_image", { groupId, emotion, fileName, bytes }, () => []),
  createEmojiGroup: (name: string) => call<EmojiGroup[]>("create_emoji_group", { name }, () => []),
  renameEmojiGroup: (groupId: string, newName: string) =>
    call<EmojiGroup[]>("rename_emoji_group", { groupId, newName }, () => []),
  deleteEmojiGroup: (groupId: string) => call<EmojiGroup[]>("delete_emoji_group", { groupId }, () => []),
  createEmojiEmotion: (groupId: string, emotion: string) =>
    call<EmojiGroup[]>("create_emoji_emotion", { groupId, emotion }, () => []),
  renameEmojiEmotion: (groupId: string, emotion: string, newName: string) =>
    call<EmojiGroup[]>("rename_emoji_emotion", { groupId, emotion, newName }, () => []),
  deleteEmojiEmotion: (groupId: string, emotion: string) =>
    call<EmojiGroup[]>("delete_emoji_emotion", { groupId, emotion }, () => []),
  renameEmojiImage: (groupId: string, emotion: string, fileName: string, newName: string) =>
    call<EmojiGroup[]>("rename_emoji_image", { groupId, emotion, fileName, newName }, () => []),
  deleteEmojiImage: (groupId: string, emotion: string, fileName: string) =>
    call<EmojiGroup[]>("delete_emoji_image", { groupId, emotion, fileName }, () => []),
  cleanupHistoricalResources: () => call("cleanup_historical_resources", {}, () => ({
    skipped: true,
    removedConversations: 0,
    removedMessages: 0,
    removedRuns: 0,
    removedPlannerTraces: 0,
    removedToolRouterTraces: 0,
    removedToolTraces: 0,
    removedStateSnapshots: 0,
    removedWorkspaceSnapshots: 0
  })),
  listPlugins: () => call("list_plugins", {}, () => []),
  listPluginAuxiliaryTasks: () => call<PluginAuxiliaryTaskSummary[]>("list_plugin_auxiliary_tasks", {}, () => []),
  listAgentAuxiliaryTasks: () => call<AgentAuxiliaryTaskSummary[]>("list_agent_auxiliary_tasks", {}, () => []),
  agentAuxiliaryTaskDefaults: (key: string) => call<Record<string, unknown>>("agent_auxiliary_task_defaults", { key }, () => ({})),
  listAgentAuxiliaryTaskAssignments: () => call<AgentAuxiliaryTaskAssignment[]>("list_agent_auxiliary_task_assignments", {}, () => []),
  saveAgentAuxiliaryTaskAssignment: (assignment: Pick<AgentAuxiliaryTaskAssignment, "key" | "provider" | "model" | "baseUrl" | "apiKey" | "timeout" | "extraBody">) => call<AgentAuxiliaryTaskAssignment[]>("save_agent_auxiliary_task_assignment", assignment, () => []),
  resetAgentAuxiliaryTaskAssignments: () => call<AgentAuxiliaryTaskAssignment[]>("reset_agent_auxiliary_task_assignments", {}, () => []),
  judgeAgentGoal: (goal: string, response: string, subgoals?: string[]) => call<{ done: boolean; reason: string; parseFailed: boolean; model: string }>("judge_agent_goal", { goal, response, subgoals }, () => ({ done: false, reason: "unavailable", parseFailed: false, model: "" })),
  agentGoalStatus: (conversationId: string) => call("agent_goal_status", { conversationId }, () => ({ ok: true, goal: null, continuationPrompt: null })),
  setAgentGoal: (conversationId: string, goal: string, maxTurns?: number) => call("set_agent_goal", { conversationId, goal, maxTurns }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  pauseAgentGoal: (conversationId: string, reason?: string) => call("pause_agent_goal", { conversationId, reason }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  resumeAgentGoal: (conversationId: string, resetBudget = true) => call("resume_agent_goal", { conversationId, resetBudget }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  clearAgentGoal: (conversationId: string) => call("clear_agent_goal", { conversationId }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  addAgentSubgoal: (conversationId: string, text: string) => call("add_agent_subgoal", { conversationId, text }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  removeAgentSubgoal: (conversationId: string, index: number) => call("remove_agent_subgoal", { conversationId, index }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  clearAgentSubgoals: (conversationId: string) => call("clear_agent_subgoals", { conversationId }, () => ({ ok: false, goal: null, continuationPrompt: null })),
  togglePlugin: (pluginId: string, enabled: boolean) => call("toggle_plugin", { pluginId, enabled }, () => []),
  listSkillBundles: () => call("list_skill_bundles", {}, () => []),
  installSkillBundle: (bundleId: string, agentId?: string) => call("install_skill_bundle", { bundleId, agentId }, () => []),
  listMarketplaceSkills: (query?: string) => call("list_marketplace_skills", { query }, () => []),
  installMarketplaceSkill: (skillId: string, agentId?: string) => call("install_marketplace_skill", { skillId, agentId }, () => null),
  auditSkills: (selector?: string) => call("audit_skills", { selector }, () => []),
  curateSkills: () => call("curate_skills", {}, () => null),
  getSkillCuratorState: () => call("get_skill_curator_state", {}, () => null),
  setSkillCuratorPaused: (paused: boolean) => call("set_skill_curator_paused", { paused }, () => null),
  pinSkillForCurator: (selector: string) => call("pin_skill_for_curator", { selector }, () => null),
  unpinSkillForCurator: (selector: string) => call("unpin_skill_for_curator", { selector }, () => null),
  archiveSkillForCurator: (selector: string, reason?: string) => call("archive_skill_for_curator", { selector, reason }, () => null),
  restoreSkillForCurator: (selector: string) => call("restore_skill_for_curator", { selector }, () => null),
  installExternalSkillFile: (sourcePath: string, name?: string, category?: string, agentId?: string, force?: boolean) =>
    call("install_external_skill_file", { sourcePath, name, category, agentId, force }, () => null),
  installExternalSkillUrl: (url: string, name?: string, category?: string, agentId?: string, force?: boolean) =>
    call("install_external_skill_url", { url, name, category, agentId, force }, () => null),
  listSkillInstallRecords: () => call("list_skill_install_records", {}, () => []),
  listSkillAuditLog: (limit?: number) => call("list_skill_audit_log", { limit }, () => []),
  listSkillTaps: () => call("list_skill_taps", {}, () => []),
  addSkillTap: (repo: string, path?: string) => call("add_skill_tap", { repo, path }, () => null),
  removeSkillTap: (repo: string) => call("remove_skill_tap", { repo }, () => false),
  listSkillTapMarketplace: (query?: string) => call("list_skill_tap_marketplace", { query }, () => []),
  searchSkillMarketplace: (query?: string, source?: string) => call("search_skill_marketplace", { query, source }, () => []),
  checkSkillTaps: () => call("check_skill_taps", {}, () => []),
  checkSkillUpdates: (selector?: string) => call("check_skill_updates", { selector }, () => []),
  updateSkillsFromSources: (selector?: string, agentId?: string, force?: boolean) =>
    call("update_skills_from_sources", { selector, agentId, force }, () => []),
  checkRemoteSkillUpdates: (selector?: string) => call("check_remote_skill_updates", { selector }, () => []),
  updateRemoteSkillsFromSources: (selector?: string, agentId?: string, force?: boolean) =>
    call("update_remote_skills_from_sources", { selector, agentId, force }, () => []),
  uninstallExternalSkills: (selector?: string, removeFiles?: boolean) =>
    call("uninstall_external_skills", { selector, removeFiles }, () => []),
  exportSkillSnapshot: (path: string) => call("export_skill_snapshot", { path }, () => ""),
  importSkillSnapshot: (path: string) => call("import_skill_snapshot", { path }, () => 0),
  saveSkillConfig: (agentId: string, skillId: string, config: Record<string, string>) => call("save_skill_config", { agentId, skillId, config }, () => undefined),
  listMcpTools: (serverId: string, timeoutSeconds?: number) => call("list_mcp_tools", { serverId, timeoutSeconds }, () => ({ ok: true, timedOut: false, elapsedMs: 0, tools: [] })),
  getMcpStatus: () => call("get_mcp_status", {}, () => ({ ok: true, success: true, servers: [] })),
  resetMcpPersistentSession: (serverId?: string) => call("reset_mcp_persistent_session", { serverId }, () => ({ ok: true, success: true, serverId: serverId ?? "", closed: [], missing: [] })),
  removeMcpOauthTokens: (serverId: string) => call("remove_mcp_oauth_tokens", { serverId }, () => ({ ok: true, success: true, serverId, removed: [], missing: [] })),
  refreshMcpOauthTokens: (serverId: string) => call("refresh_mcp_oauth_tokens", { serverId }, () => ({ ok: true, success: true, serverId })),
  startMcpOauthLogin: (serverId: string) => call("start_mcp_oauth_login", { serverId }, () => ({ ok: true, success: true, serverId, authorizationUrl: "", redirectUri: "" })),
  finishMcpOauthLogin: (serverId: string, codeOrCallbackUrl: string) => call("finish_mcp_oauth_login", { serverId, codeOrCallbackUrl }, () => ({ ok: true, success: true, serverId })),
  callMcpTool: (serverId: string, toolName: string, payload: unknown, timeoutSeconds?: number) => call("call_mcp_tool", { serverId, toolName, payload, timeoutSeconds }, () => ({ ok: true, timedOut: false, elapsedMs: 0, stdout: "", stderr: "" })),
  listPlannerTraces: () => call("list_planner_traces", {}, () => []),
  listToolRouterTraces: () => call("list_tool_router_traces", {}, () => []),
  listToolTraces: () => call("list_tool_traces", {}, () => []),
  listToolDefinitions: () => call("list_tool_definitions", {}, () => []),
  listToolApprovals: () => call("list_tool_approvals", {}, () => []),
  approveToolCall: (approvalId: string, timeoutSeconds?: number) => call("approve_tool_call", { approvalId, timeoutSeconds }, () => null),
  approveToolCallAlways: (approvalId: string, timeoutSeconds?: number) => call("approve_tool_call_always", { approvalId, timeoutSeconds }, () => null),
  approveToolCallServer: (approvalId: string, timeoutSeconds?: number) => call("approve_tool_call_server", { approvalId, timeoutSeconds }, () => null),
  denyToolCall: (approvalId: string, reason?: string) => call("deny_tool_call", { approvalId, reason }, () => null),
  refreshToolRegistry: () => call("refresh_tool_registry", {}, () => []),
  saveProfileAvatar: async () => fallbackProfile,
  uploadProfileAvatar: (fileName: string, bytes: number[]) =>
    call<ProfileConfig>("upload_profile_avatar", { fileName, bytes }, () => fallbackProfile),
  clearProfileAvatar: () => call<ProfileConfig>("clear_profile_avatar", {}, () => fallbackProfile),
  uploadPersonaAvatar: (personaId: string, fileName: string, bytes: number[]) =>
    call<Persona>("upload_persona_avatar", { personaId, fileName, bytes }, () => ({ ...defaultPersona, id: personaId })),
  clearPersonaAvatar: (personaId: string) =>
    call<Persona>("clear_persona_avatar", { personaId }, () => ({ ...defaultPersona, id: personaId, avatarPath: "" })),
  importThemeCss: async () => [],
  exportThemesCss: async () => "",
  pickFile: async (title?: string, filterName?: string, extensions?: string[]) => {
    try {
      const selected = await openDialog({
        title,
        multiple: false,
        directory: false,
        filters: extensions?.length ? [{ name: filterName || "Files", extensions }] : undefined
      });
      return dialogSelectionToPath(selected);
    } catch (error) {
      console.warn("plugin file dialog failed, falling back to native command:", error);
    }
    try {
      const selected = await call<string | null>(
        "pick_path",
        { title, directory: false, filterName, extensions },
        () => null
      );
      return selected;
    } catch (error) {
      console.error("native file dialog command failed:", error);
      return null;
    }
  },
  pickFolder: async (title?: string) => {
    try {
      const selected = await openDialog({
        title,
        multiple: false,
        directory: true
      });
      return dialogSelectionToPath(selected);
    } catch (error) {
      console.warn("plugin folder dialog failed, falling back to native command:", error);
    }
    try {
      const selected = await call<string | null>(
        "pick_path",
        { title, directory: true, filterName: null, extensions: null },
        () => null
      );
      return selected;
    } catch (error) {
      console.error("native folder dialog command failed:", error);
      return null;
    }
  },
  installDocker: async () => ok(),
  startDockerDesktop: async () => ok(),
  setupWsl2: async () => ok(),
  installOllama: async () => ok(),
  installPython: async () => ok(),
  setupSearxng: async () => ok(),
  startOllamaService: async () => ok(),
  pullVisionModel: async () => ok(),
  installChatttsDeps: async () => ok(),
  installAllMissing: async () => ok(),
  cancelEnvironmentAction: async () => ok(),
  getWechatConfig: () => call<WechatConfig>("get_wechat_config", {}, () => ({ baseUrl: "", timeoutSeconds: 35 })),
  saveWechatConfig: (config: WechatConfig) =>
    call<WechatConfig>("save_wechat_config", { config }, () => config),
  startWechatQr: (baseUrl?: string) =>
    call<WechatQrStartResult>("start_wechat_qr", { baseUrl: baseUrl || null }, () => ({ qrcode: "", baseUrl: baseUrl || "", raw: null })),
  checkWechatQrStatus: (qrcode: string, baseUrl?: string) =>
    call<WechatQrStatusResult>("check_wechat_qr_status", { qrcode, baseUrl: baseUrl || null }, () => ({ status: "idle", raw: null })),
  listWechatLinks: () => call<WechatLinkSummary[]>("list_wechat_links", {}, () => []),
  wechatInboundText: (accountId: string, userId: string, text: string, contextToken?: string, rawMessage?: unknown, attachments?: unknown[]) =>
    call<WechatInboundResult>("wechat_inbound_text", { accountId, userId, text, contextToken: contextToken || null, rawMessage: rawMessage ?? null, attachments: attachments ?? null }, () => ({ messages: [], delivered: false, deliveryError: "wechat runtime unavailable" })),
  wechatPollOnce: (accountId: string, timeoutSeconds?: number) =>
    call<WechatPollResult>("wechat_poll_once", { accountId, timeoutSeconds: timeoutSeconds ?? null }, () => ({ account: null as unknown as AccountConfig, processed: [], receivedCount: 0, skippedCount: 0, updatedBuffer: false, raw: null })),
  openai: {},
  anthropic: {},
  deepseek: {},
  siliconflow: {},
  qweather: {},
  example: {}
};
