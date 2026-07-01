export type AppSection =
  | "chat"
  | "contacts"
  | "discover"
  | "moments"
  | "personas"
  | "memory"
  | "worldbooks"
  | "plugins"
  | "mcp"
  | "agents"
  | "skills"
  | "settings";

export interface WebConfig {
  port: number;
  password: string;
  publicEnabled: boolean;
  publicPort: number;
  publicSecret: string;
}

export interface WeatherConfig {
  qweatherApiKey: string;
  qweatherApiHost: string;
  defaultLocation: string;
  timeoutSeconds: number;
}

export interface ChatConfig {
  skipEnvCheck?: boolean;
  agentEngine?: "legacy" | "langgraph" | string;
  maxContextRounds: number;
  shortContextMode?: "messages" | "tokens";
  shortContextTokenBudget?: number;
  shortContextAbortOnSummaryFailure?: boolean;
  shortContextSummaryProviderId?: string;
  shortContextSummaryModel?: string;
  busyInputMode?: "queue" | "steer" | "interrupt" | string;
  autoTitleEnabled?: boolean;
  queueWaitSeconds: number;
  messageDedupEnabled?: boolean;
  messageDedupWindowSeconds?: number;
  delegationMaxConcurrentChildren?: number;
  delegationStrategy?: string;
  delegationOrchestratorEnabled?: boolean;
  delegationSubagentAutoApprove?: boolean;
  delegationInheritMcpToolsets?: boolean;
  delegationSubagentProviderId?: string;
  delegationSubagentModel?: string;
  auxiliaryTaskAssignments?: Record<string, Record<string, unknown>>;
  agentRunTimeoutSeconds?: number;
  uiMessageLimit?: number;
  artifactScanLimit?: number;
  uiMessagePreviewChars?: number;
  uiStreamCharsPerSecond?: number;
  thinkingMinVisibleMs?: number;
  petCloudDurationSeconds?: number;
  bottomFollowThresholdPx?: number;
  activePollIntervalMs?: number;
  idlePollIntervalMs?: number;
  intentAnalyzerMode?: "embedding" | "llm" | string;
  intentAnalyzerProviderId?: string;
  intentAnalyzerModel?: string;
  intentEmbeddingMinConfidence?: number;
  intentLlmMinConfidence?: number;
  intentLlmTimeoutSeconds?: number;
  intentLlmMaxTokens?: number;
  intentLlmPrompt?: string;
  toolRouterMode?: "llm_unified" | string;
  toolUseEnforcement?: "auto" | "off" | string;
  toolRouterLlmEnabled?: boolean;
  toolRouterLlmMinConfidence?: number;
  toolRouterLlmTimeoutSeconds?: number;
  toolRouterLlmMaxTokens?: number;
  toolRouterLlmPrompt?: string;
  toolParallelEnabled?: boolean;
  toolParallelLimit?: number;
  sendMessageToolEnabled?: boolean;
  toolApprovalMode?: "risky" | "smart" | "always" | "never" | string;
  cronApprovalMode?: "deny" | "approve" | string;
  trustedToolPatterns?: string[];
  trustedCommandPatterns?: string[];
  hooks?: Record<string, Array<Record<string, unknown>>>;
  hooksAutoAccept?: boolean;
  llmCredentialPoolStrategy?: "fill_first" | "round_robin" | "random" | "least_used" | string;
  toolEnvPassthrough?: string[];
  toolMutationCheckpointEnabled?: boolean;
  llmRetryCount?: number;
  llmRetryBackoffMs?: number;
  responsesReasoningReplayEnabled?: boolean;
  fastModeEnabled?: boolean;
  runtimeFooterEnabled?: boolean;
  statusbarEnabled?: boolean;
  toolProgressDisplay?: "off" | "new" | "all" | "verbose" | string;
  displaySkin?: string;
  busyIndicatorStyle?: "kaomoji" | "emoji" | "unicode" | "ascii" | string;
  codexRuntime?: "auto" | "codex_app_server" | string;
  toolCallRetryCount?: number;
  toolCallRetryBackoffMs?: number;
  toolResultPersistThresholdChars?: number;
  toolResultPreviewChars?: number;
  toolObservationTurnBudgetChars?: number;
  toolObservationTailBudgetChars?: number;
  toolOutputMaxBytes?: number;
  toolOutputMaxLines?: number;
  toolOutputMaxLineLength?: number;
  toolGuardrailWarningsEnabled?: boolean;
  toolGuardrailHardStopEnabled?: boolean;
  toolGuardrailExactFailureWarnAfter?: number;
  toolGuardrailSameToolFailureWarnAfter?: number;
  toolGuardrailNoProgressWarnAfter?: number;
  toolGuardrailExactFailureLimit?: number;
  toolGuardrailSameToolFailureLimit?: number;
  toolGuardrailNoProgressLimit?: number;
  backgroundMemoryReviewEnabled?: boolean;
  backgroundMemoryReviewMinMessages?: number;
  backgroundSkillReviewEnabled?: boolean;
  backgroundSkillReviewAutoCreateEnabled?: boolean;
  backgroundSkillCuratorEnabled?: boolean;
  backgroundSkillCuratorIntervalHours?: number;
  skillHotReloadEnabled?: boolean;
  skillHotReloadIntervalSeconds?: number;
  historyCleanupEnabled?: boolean;
  historyRetentionDays?: number;
  maxStoredMessagesPerConversation?: number;
  maxStoredAgentRuns?: number;
  maxStoredToolTraces?: number;
}

export interface VideoSummaryConfig {
  enabled: boolean;
  modelsDir: string;
  transcriber: string;
  ytDlpCommand: string;
  cookie: string;
  cookieFile: string;
  ffmpegBinPath: string;
  fasterWhisperModel: string;
  fasterWhisperModelDir: string;
  fasterWhisperDevice: string;
  fasterWhisperComputeType: string;
  senseVoiceModelDir: string;
  senseVoiceDevice: string;
  timeoutSeconds: number;
  ytdlpInfoTimeoutSeconds: number;
  downloadTimeoutSeconds: number;
  outputDir: string;
}

export interface ReplyConfig {
  typingDelayEnabled?: boolean;
  typingSpeed: number;
  typingSpeedRandomMin: number;
  typingSpeedRandomMax: number;
  splitByNewline: boolean;
  showTypingIndicator: boolean;
  typingIndicatorRefreshSeconds?: number;
}

export interface MomentsConfig {
  publishers: string[];
  repliers: string[];
  autoReplyEnabled: boolean;
}

export interface PlatformRuntimeConfig {
  enabled?: boolean;
  autoStart?: boolean;
  timeoutSeconds?: number;
  [key: string]: unknown;
}

export interface AppConfig {
  logLevel: "debug" | "info" | "warning" | "error";
  chat: ChatConfig;
  reply: ReplyConfig;
  web: WebConfig;
  weather: WeatherConfig;
  homeassistant?: PlatformRuntimeConfig;
  feishu?: PlatformRuntimeConfig;
  yuanbao?: PlatformRuntimeConfig;
  telegram?: PlatformRuntimeConfig;
  slack?: PlatformRuntimeConfig;
  mattermost?: PlatformRuntimeConfig;
  matrix?: PlatformRuntimeConfig;
  signal?: PlatformRuntimeConfig;
  email?: PlatformRuntimeConfig;
  sms?: PlatformRuntimeConfig;
  dingtalk?: PlatformRuntimeConfig;
  whatsapp?: PlatformRuntimeConfig;
  qqbot?: PlatformRuntimeConfig;
  bluebubbles?: PlatformRuntimeConfig;
  messagingGateway?: PlatformRuntimeConfig;
  spotify?: PlatformRuntimeConfig;
  webhook?: PlatformRuntimeConfig;
  discord?: PlatformRuntimeConfig;
  moments: MomentsConfig;
  videoSummary: VideoSummaryConfig;
  telemetryEnabled: boolean;
}

export interface HermesCredentialPoolEntryStatus {
  providerId: string;
  index: number;
  id?: string;
  label: string;
  authType?: string;
  source?: string;
  state: string;
  expiresAt?: string;
  baseUrl?: string;
}

export interface AddHermesCredentialPoolEntryRequest {
  provider: string;
  label?: string;
  apiKey: string;
  baseUrl?: string;
  authType?: string;
  expiresAt?: string;
}

export interface StateSnapshotManifest {
  id: string;
  label?: string;
  createdAt?: string;
  statePath?: string;
}

export interface StateSnapshotRestoreResult {
  restored: StateSnapshotManifest;
  preRestore: StateSnapshotManifest;
}

export interface WorkspaceSnapshotManifest {
  id: string;
  label?: string;
  createdAt?: string;
  root?: string;
  snapshotPath?: string;
  filesPath?: string;
  fileCount?: number;
  totalBytes?: number;
  skippedFiles?: number;
  skippedDirs?: number;
  truncated?: boolean;
}

export interface WorkspaceSnapshotRestoreResult {
  restored: WorkspaceSnapshotManifest;
  preRestore: WorkspaceSnapshotManifest;
  restoredFiles?: number;
  removedNewFiles?: number;
  deleteNewFiles?: boolean;
  note?: string;
}

export interface Conversation {
  id: string;
  title: string;
  updatedAt: string;
  lastMessage: string;
  personaId?: string;
  wechatAccountId?: string | null;
  agentId?: string;
}

export interface ChatMessage {
  id: string;
  conversationId: string;
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  createdAt: string;
  source?: string;
  accountId?: string | null;
  providerData?: unknown | null;
}

export interface ChatAttachment {
  id: string;
  fileName: string;
  mimeType: string;
  fileSize: number;
  path: string;
}

export interface ToolEvent {
  status?: "running" | "completed" | "failed" | "canceled" | string;
  referenceId?: string;
  callId?: string;
  runId?: string;
  checkpointId?: string | null;
  dependsOn?: string[];
  mutexGroup?: string;
  eventType: string;
  serverId: string;
  toolName: string;
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  kind?: "read" | "edit" | "execute" | "search" | "fetch" | "think" | "other" | string;
  title: string;
  summary: string;
  path?: string | null;
  exists?: boolean | null;
  mimeType?: string | null;
  text?: string | null;
  error?: string | null;
  raw?: unknown;
}

export interface ToolEventEnvelope {
  type: "toolEvent";
  reference?: {
    referenceId?: string;
    callId?: string;
    tool?: string;
    ok?: boolean;
    path?: string | null;
    checkpointId?: string | null;
  };
  modelSummary?: string;
  event: ToolEvent;
}

export interface AgentRunRecord {
  runId: string;
  conversationId: string;
  personaId: string;
  agentId: string;
  parentRunId?: string | null;
  subagentIndex?: number | null;
  subagentDepth?: number | null;
  subagentCanDelegate?: boolean | null;
  subagentRole?: string | null;
  subagentTask?: string | null;
  subagentToolsets?: string[];
  subagentMaxIterations?: number | null;
  queueItemId?: string | null;
  userRequest?: string;
  state: string;
  startedAt: string;
  updatedAt: string;
  lastActivityAt?: string | null;
  lastActivityDesc?: string | null;
  completedAt?: string | null;
  error?: string | null;
  toolEvents: ToolEvent[];
  phaseEvents?: AgentRunPhase[];
  checkpoints?: AgentCheckpointRecord[];
  pendingSteers?: string[];
}

export interface ToolArtifactRecord {
  runId: string;
  fileName: string;
  path: string;
  sizeBytes: number;
  modifiedAt?: string | null;
  contentPreview?: string | null;
}

export interface AgentCheckpointRecord {
  checkpointId: string;
  runId: string;
  iteration: number;
  createdAt: string;
  state: string;
  completedCallIds?: string[];
  eventRefs?: string[];
  summary: string;
}

export interface AgentRunEvent {
  runId: string;
  conversationId: string;
  personaId: string;
  agentId: string;
  parentRunId?: string | null;
  subagentIndex?: number | null;
  subagentDepth?: number | null;
  subagentCanDelegate?: boolean | null;
  subagentRole?: string | null;
  subagentTask?: string | null;
  subagentToolsets?: string[];
  subagentMaxIterations?: number | null;
  queueItemId?: string | null;
  state: string;
  message?: ChatMessage | null;
  toolEvent?: ToolEvent | null;
  accumulatedToolEvents?: ToolEvent[];
  accumulatedPhases?: AgentRunPhase[];
  phase?: string | null;
  detail?: unknown | null;
  error?: string | null;
  updatedAt: string;
  lastActivityAt?: string | null;
  lastActivityDesc?: string | null;
}

export interface AgentRunPhase {
  phase: string;
  detail?: unknown | null;
  updatedAt: string;
}

export interface ManagedProcessEvent {
  type: "completed" | "stopped" | "watch_match" | "watch_disabled" | string;
  processId: string;
  label?: string | null;
  command?: string | null;
  cwd?: string | null;
  pid?: number | null;
  conversationId?: string | null;
  runId?: string | null;
  detail?: Record<string, unknown> | null;
  createdAt: string;
}

export interface ManagedProcessSnapshot {
  id: string;
  sessionId?: string;
  session_id?: string;
  label?: string | null;
  command?: string | null;
  cwd?: string | null;
  pid?: number | null;
  backend?: string | null;
  envType?: string | null;
  env_type?: string | null;
  status?: "running" | "exited" | "stopped" | string;
  conversationId?: string | null;
  conversation_id?: string | null;
  runId?: string | null;
  run_id?: string | null;
  startedAt?: string | null;
  started_at?: string | null;
  finishedAt?: string | null;
  finished_at?: string | null;
  exitCode?: number | null;
  exit_code?: number | null;
  notifyOnComplete?: boolean;
  notify_on_complete?: boolean;
  watchPatterns?: string[];
  watch_patterns?: string[];
  watchStats?: Record<string, unknown>;
  watch_stats?: Record<string, unknown>;
  stdoutTail?: string[];
  stdout_tail?: string[];
  stderrTail?: string[];
  stderr_tail?: string[];
  [key: string]: unknown;
}

export interface PlannerTraceRecord {
  id: string;
  runId: string;
  conversationId: string;
  personaId: string;
  agentId: string;
  iteration: number;
  createdAt: string;
  input: string;
  output: string;
  parsedStep: string;
  error?: string | null;
}

export interface ToolRouterTraceRecord {
  id: string;
  createdAt: string;
  conversationId: string;
  personaId: string;
  semanticIntent: string;
  userRequest: string;
  prompt: string;
  output: string;
  decision?: unknown | null;
  status: string;
  error?: string | null;
}

export interface AgentQueuedRequest {
  id: string;
  conversationId: string;
  personaId: string;
  userMessageId: string;
  content: string;
  status: "pending" | "running" | "completed" | "failed" | "canceled" | string;
  createdAt: string;
  updatedAt: string;
  startedAt?: string | null;
  completedAt?: string | null;
  error?: string | null;
}

export interface AgentRuntimeEvent {
  id: number;
  kind: string;
  source: string;
  status?: string | null;
  conversationId?: string | null;
  conversation_id?: string | null;
  runId?: string | null;
  run_id?: string | null;
  queueItemId?: string | null;
  queue_item_id?: string | null;
  taskId?: string | null;
  task_id?: string | null;
  processId?: string | null;
  process_id?: string | null;
  payload?: unknown;
  createdAt?: string | null;
  created_at?: string | null;
}

export interface AgentRuntimeEventStream {
  schema: "hermes_kanban_runtime_events_desktop_v1" | string;
  status: string;
  action: string;
  events: AgentRuntimeEvent[];
  cursor: number;
  count: number;
  total: number;
  since: number;
  limit: number;
  pollIntervalMs?: number;
  websocketEmbedded?: boolean;
  nativeRuntimeEventBridge?: boolean;
  sources?: string[];
}

export interface KanbanDispatchDrainResult {
  schema: "hermes_kanban_dispatch_drain_desktop_v1" | string;
  status: string;
  action: "kanban-dispatch-drain" | string;
  dispatch: Record<string, unknown>;
  drainRequested: boolean;
  drain_requested?: boolean;
  drained: AgentQueuedRequest[];
  drainedCount: number;
  drained_count?: number;
  nativeDispatcherDrainBridge?: boolean;
  boundary?: string;
}

export interface AgentControlCommand {
  name: string;
  aliases: string[];
  argsHint: string;
  category: string;
  description: string;
}

export interface ScheduledAgentJob {
  id: string;
  name: string;
  conversationId?: string | null;
  personaId: string;
  prompt: string;
  scheduleKind: "once" | "interval" | "cron" | string;
  intervalMinutes?: number | null;
  cronExpr?: string | null;
  runAt?: string | null;
  enabledToolsets: string[];
  disabledToolsets: string[];
  enabled: boolean;
  status: "scheduled" | "paused" | "completed" | "failed" | string;
  nextRunAt?: string | null;
  lastRunAt?: string | null;
  lastCompletedAt?: string | null;
  lastRunStatus?: string | null;
  lastOutput?: string | null;
  lastOutputPath?: string | null;
  lastError?: string | null;
  runCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface ScheduledJobOutputRecord {
  fileName: string;
  path: string;
  modifiedAt: string;
  sizeBytes: number;
  status: string;
}

export interface AgentTodoItem {
  id: string;
  runId: string;
  conversationId: string;
  content: string;
  status: "pending" | "in_progress" | "completed" | "blocked" | string;
  createdAt: string;
  updatedAt: string;
}

export interface ToolTraceEntry {
  id: string;
  createdAt: string;
  serverId: string;
  toolName: string;
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  payload: unknown;
  event: ToolEvent;
  error?: string | null;
}

export interface ToolDefinition {
  name: string;
  displayName: string;
  description: string;
  source: string;
  serverId: string;
  toolName: string;
  inputSchema: unknown;
  requiresApproval: boolean;
}

export interface ToolApprovalRequest {
  id: string;
  createdAt: string;
  updatedAt: string;
  status: "pending" | "approved" | "completed" | "failed" | "denied" | string;
  conversationId?: string | null;
  personaId?: string | null;
  agentId?: string | null;
  runId?: string | null;
  serverId: string;
  toolName: string;
  payload: unknown;
  reason: string;
  result?: unknown | null;
  error?: string | null;
}

export interface SendChatRequest {
  conversationId?: string | null;
  personaId?: string | null;
  agentId?: string | null;
  content: string;
  providerData?: unknown | null;
}

export interface LlmProvider {
  id: string;
  name: string;
  providerType?: "openai_compatible" | "openai_responses" | "anthropic" | "gemini" | string;
  preset?: string;
  baseUrl: string;
  appendChatPath?: boolean;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
  promptCacheMode?: "auto" | "on" | "off" | string;
  promptCacheTtl?: "5m" | "1h" | string;
  promptCacheLayout?: "auto" | "native" | "envelope" | string;
  models?: Record<string, Record<string, unknown>>;
}

export interface ModelCapabilities {
  provider_id?: string;
  model_id?: string;
  models_dev_provider_id?: string;
  supports_tools?: boolean;
  supports_vision?: boolean;
  supports_reasoning?: boolean;
  supports_pdf?: boolean;
  supports_audio_input?: boolean;
  supports_structured_output?: boolean;
  open_weights?: boolean;
  input_modalities?: string[];
  output_modalities?: string[];
  context_window?: number | null;
  max_output_tokens?: number | null;
  model_family?: string;
  status?: string;
  knowledge_cutoff?: string;
  source?: string;
}

export interface ProfileConfig {
  name: string;
  avatarPath?: string | null;
}

export interface AccountConfig {
  id: string;
  note: string;
  linkedPersona: string;
  online: boolean;
  createdAt: string;
  botToken?: string;
  ilinkUserId?: string;
  getUpdatesBuf?: string;
  loginBaseUrl?: string;
  lastLoginAt?: string;
  lastWechatUserId?: string;
  lastContextToken?: string;
  lastInboundAt?: string;
  rawLoginStatus?: unknown;
}

export interface ImageProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
  useSystemProxy?: boolean;
}

export interface VideoProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
  submitPath: string;
  statusPath: string;
  idPath: string;
  statusField: string;
  resultPath: string;
  completedStatuses: string[];
  failedStatuses: string[];
  pollIntervalSeconds: number;
  maxPollSeconds: number;
  downloadResult: boolean;
}

export interface SearchProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  enabled: boolean;
  timeoutSeconds: number;
}

export interface VisionProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  model: string;
  enabled: boolean;
  timeoutSeconds: number;
}

export interface BrowserProvider {
  id: string;
  name: string;
  providerType: string;
  baseUrl: string;
  apiKeyEnv: string;
  apiKey?: string | null;
  projectId?: string;
  recordSessions?: boolean;
  enabled: boolean;
  timeoutSeconds: number;
}

export interface ThemeConfig {
  id: string;
  name: string;
  mode: "light" | "dark" | "auto";
  active: boolean;
  css: string;
  createdAt: string;
  updatedAt: string;
}

export interface EmojiGroup {
  id: string;
  name: string;
  emotions: string[];
  images: string[];
  emotionImages?: Record<string, string[]>;
}

export interface WechatConfig {
  baseUrl: string;
  timeoutSeconds: number;
}

export interface WechatQrStartResult {
  qrcode: string;
  qrImage?: string | null;
  baseUrl: string;
  raw: unknown;
}

export interface WechatQrStatusResult {
  status: string;
  message?: string | null;
  account?: AccountConfig | null;
  host?: string | null;
  raw: unknown;
}

export interface WechatLinkSummary {
  accountId: string;
  personaId: string;
  personaName: string;
  accountNote: string;
  online: boolean;
}

export interface WechatInboundResult {
  messages: ChatMessage[];
  delivered: boolean;
  deliveryError?: string | null;
}

export interface WechatProcessedInbound {
  userId: string;
  text: string;
  conversationId?: string | null;
  delivered: boolean;
  deliveryError?: string | null;
}

export interface WechatPollResult {
  account: AccountConfig;
  processed: WechatProcessedInbound[];
  receivedCount: number;
  skippedCount: number;
  updatedBuffer: boolean;
  raw: unknown;
}

export interface MomentComment {
  id: string;
  personaId: string;
  text: string;
  replyTo?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface MomentPost {
  id: string;
  personaId: string;
  body: string;
  coverPath?: string;
  likedBy: string[];
  comments: MomentComment[];
  createdAt: string;
  updatedAt: string;
}

export interface Persona {
  id: string;
  name: string;
  agentId?: string;
  avatarPath?: string | null;
  systemPrompt: string;
  characterPrompt: string;
  outputExamples: string;
  systemInstructions: string;
  llmProvider: string;
  llmModel: string;
  temperature: number;
  maxTokens: number;
  toolPolicy: {
    enabled: boolean;
    timeoutSeconds: number;
    maxIterations: number;
    maxFailureReplans: number;
    retryCount?: number;
    retryBackoffMs?: number;
  };
  emojiEnabled?: boolean;
  emojiGroup?: string;
  emojiSendProbability?: number;
  memory?: {
    enabled: boolean;
    triggerRounds: number;
    maxMemories: number;
    includeInPrompt: boolean;
  };
  proactive?: {
    enabled: boolean;
    minIdleHours: number;
    maxIdleHours: number;
    maxConsecutive: number;
    prompt: string;
    quietHours: {
      enabled: boolean;
      start: string;
      end: string;
    };
  };
  voiceReply?: {
    enabled: boolean;
    engine: string;
    language: string;
    voice: string;
    volume: string;
    pitch: string;
    pythonPath: string;
    modelDir: string;
    sampleRate: number;
    speed: number;
    oral: number;
    laugh: number;
    breakLevel: number;
    speakerSeed: number;
    speakerEmbedding: string;
    temperature: number;
    topP: number;
    topK: number;
    refineTextEnabled: boolean;
    refinePrompt: string;
    refineTemperature: number;
  };
  imageGeneration?: {
    enabled: boolean;
    provider: string;
    model: string;
    stylePrefix: string;
    artStyle: string;
    negativePrompt: string;
    negativeEnabled: boolean;
    refMode: "avatar" | "custom" | "none";
  };
}

export interface MemoryEntry {
  id: string;
  personaId: string;
  target?: "memory" | "user" | "session" | string;
  summary: string;
  importance: number;
  createdAt: string;
  updatedAt: string;
}

export interface MemoryStatus {
  personaId: string;
  personaName: string;
  enabled: boolean;
  includeInPrompt: boolean;
  triggerRounds: number;
  maxMemories: number;
  total: number;
  promptSafe: number;
  blockedBySecurityScan: number;
  promptInjected: number;
}

export interface ConversationDeleteMemorySettlingResult {
  status: "scheduled" | "settled" | "skipped" | "failed" | string;
  reason?: string | null;
  memoryCount: number;
}

export interface WorldbookSection {
  id: string;
  key: string;
  content: string;
  enabled: boolean;
}

export interface Worldbook {
  id: string;
  name: string;
  description: string;
  boundPersonas: string[];
  sections: WorldbookSection[];
  createdAt: string;
  updatedAt: string;
}

export interface PluginSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  providedTools: string[];
  providedCapabilities?: string[];
  providedHooks?: string[];
  requiresEnv?: string[];
  missingEnv?: string[];
  envConfigured?: boolean;
  version?: string;
  author?: string;
  source?: string;
  homepageUrl?: string;
  kind?: string;
  path?: string;
  manifestPath?: string;
  entryPoint?: string;
}

export interface PluginAuxiliaryTaskSummary {
  pluginId: string;
  pluginName: string;
  key: string;
  displayName: string;
  description: string;
  defaults?: Record<string, unknown>;
}

export interface AgentAuxiliaryTaskSummary {
  key: string;
  displayName: string;
  description: string;
  source: string;
  pluginId?: string;
  pluginName?: string;
  defaults?: Record<string, unknown>;
}

export interface AgentAuxiliaryTaskAssignment {
  key: string;
  displayName: string;
  description: string;
  source: string;
  pluginId?: string;
  pluginName?: string;
  provider: string;
  model: string;
  baseUrl: string;
  apiKey: string;
  timeout: number;
  extraBody?: Record<string, unknown>;
}

export interface McpServer {
  id: string;
  name: string;
  transport?: "stdio" | "streamable_http" | "sse";
  command: string;
  args: string[];
  env?: Record<string, string>;
  url?: string;
  protocol: "oneShotJson" | "mcpJsonRpc" | "mcpJsonRpcLine";
  enabled: boolean;
  timeoutSeconds: number;
  supportsParallelToolCalls?: boolean;
  persistentSession?: boolean;
  keepAlive?: boolean;
  keepAliveIntervalSeconds?: number;
  keepAliveTimeoutSeconds?: number;
}

export interface CapabilityAdapter {
  name: string;
  description: string;
  mcpServer: string;
  mcpTool: string;
  parameters: unknown;
  paramMapping: Record<string, string>;
  injectFields: Record<string, string>;
  enabled: boolean;
}

export interface AgentConfig {
  enabled: boolean;
  mcpEnabled: boolean;
  skillsEnabled: boolean;
  allowShell: boolean;
  maxSubagents: number;
  maxSubagentDepth: number;
  maxToolIterations: number;
  skillsDir: string;
  enabledSkills: string[];
  enabledMcpServers: string[];
  enabledToolsets: string[];
  disabledToolsets: string[];
}

export interface SkillSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  path: string;
}

export interface ProactiveStatus {
  personaId: string;
  personaName: string;
  enabled: boolean;
  conversationId?: string | null;
  lastUserAt: number;
  secondsSinceLastUser: number;
  lastReplyAt: number;
  secondsSinceLastReply: number;
  waitSeconds: number;
  readyInSeconds: number;
  consecutiveCount: number;
  maxConsecutive: number;
  inQuietHours: boolean;
  petVisionSuspended?: boolean;
  canFire: boolean;
  blockedReason: string;
}

export interface McpCallResult {
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  stdout: string;
  stderr: string;
  error?: string | null;
}

export interface McpToolInfo {
  name: string;
  description?: string | null;
  inputSchema?: unknown;
}

export interface McpListToolsResult {
  ok: boolean;
  timedOut: boolean;
  elapsedMs: number;
  tools: McpToolInfo[];
  raw?: unknown;
  error?: string | null;
}

export interface AgentDefinition {
  id: string;
  name: string;
  description: string;
  workspaceDir: string;
  llmProvider: string;
  llmModel: string;
  enabled: boolean;
  isDefault: boolean;
  mcpEnabled: boolean;
  skillsEnabled: boolean;
  allowShell: boolean;
  maxSubagents: number;
  maxSubagentDepth: number;
  maxToolIterations: number;
  skillsDir: string;
  enabledSkills: string[];
  enabledMcpServers: string[];
  enabledToolsets: string[];
  disabledToolsets: string[];
  createdAt: string;
  updatedAt: string;
}

export interface EnhancedSkillSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  path: string;
  version: string;
  author: string;
  icon: string;
  isCore: boolean;
  isBundled: boolean;
  source: string;
  agentId: string;
  config: Record<string, string>;
}

export interface SkillBundle {
  id: string;
  name: string;
  description: string;
  skillIds: string[];
}

export interface MarketplaceSkill {
  id: string;
  name: string;
  description: string;
  version: string;
  author: string;
  downloadUrl: string;
  icon: string;
  tags: string[];
}

export interface SkillAuditFinding {
  severity: string;
  category: string;
  message: string;
  file: string;
  line?: number | null;
}

export interface SkillAuditReport {
  skillId: string;
  name: string;
  path: string;
  status: string;
  checkedFiles: number;
  findings: SkillAuditFinding[];
}

export interface SkillInstallRecord {
  skillId: string;
  name: string;
  source: string;
  identifier: string;
  installPath: string;
  auditStatus: string;
  installedAt: string;
}

export interface SkillAuditLogEntry {
  type?: string;
  createdAt?: string;
  skillId?: string;
  name?: string;
  source?: string;
  identifier?: string;
  installPath?: string;
  auditStatus?: string;
  findingCount?: number;
  removedFiles?: boolean;
  [key: string]: unknown;
}

export interface SkillTap {
  repo: string;
  path: string;
}

export interface SkillTapStatus {
  repo: string;
  path: string;
  status: string;
  entryCount: number;
  detail: string;
}

export interface SkillUpdateCheck {
  skillId: string;
  name: string;
  status: string;
  detail: string;
}

export interface SkillCuratorOverlap {
  umbrella: string;
  skillIds: string[];
  reason: string;
}

export interface SkillCuratorArchiveCandidate {
  skillId: string;
  name: string;
  reason: string;
}

export interface SkillCuratorReport {
  generatedAt: string;
  reportPath: string;
  totalSkills: number;
  externalSkills: number;
  bundledSkills: number;
  auditAttention: number;
  overlapClusters: SkillCuratorOverlap[];
  archiveCandidates: SkillCuratorArchiveCandidate[];
  recommendations: string[];
}

export interface SkillCuratorArchiveRecord {
  archiveId: string;
  skillId: string;
  name: string;
  originalPath: string;
  archivePath: string;
  reason: string;
  archivedAt: string;
  restoredAt?: string | null;
  installRecord: SkillInstallRecord;
}

export interface SkillCuratorState {
  paused: boolean;
  pinnedSkillIds: string[];
  archived: SkillCuratorArchiveRecord[];
  lastRunAt?: string | null;
  lastReportPath?: string | null;
  runCount: number;
  updatedAt: string;
}

export interface EnhancedPluginSummary {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
  providedTools: string[];
  version: string;
  author: string;
  icon: string;
  source: string;
  homepageUrl: string;
}

export interface TokenUsageStats {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  callCount: number;
}

export interface TokenUsageResponse {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
  callCount: number;
  byProvider?: Record<string, TokenUsageStats>;
  byModel?: Record<string, TokenUsageStats>;
}

export interface AppBuildInfo {
  productName: string;
  version: string;
  identifier: string;
  target: string;
  updateManifestUrl: string;
}

export interface AppUpdateCheck {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  downloadUrl?: string | null;
  releaseUrl?: string | null;
  notes?: string | null;
  publishedAt?: string | null;
  sourceUrl: string;
  checkedAt: string;
}

export interface AppUpdateInstallResult {
  installerPath: string;
  helperScriptPath: string;
  mode: string;
  message: string;
}

export interface ModelCatalogEntry {
  id: string;
  name: string;
  family?: string;
  capabilities?: ModelCapabilities;
}

export interface DetectedModelList {
  ok: boolean;
  source: "live" | "catalog" | string;
  providerId: string;
  providerType: string;
  baseUrl: string;
  models: ModelCatalogEntry[];
  error?: string | null;
}

// ── Environment Check Types ──

export interface CheckItem {
  id: string;
  name: string;
  status: "ok" | "missing" | "not_running" | "installing" | "starting" | "error";
  detail: string;
  fixAction?: string | null;
  fixLabel?: string | null;
}

export interface EnvCheckResult {
  items: CheckItem[];
  allPassed: boolean;
}

export interface ActionResult {
  success: boolean;
  message: string;
  detail?: string | null;
}

export interface InstallProgressEvent {
  id: string;
  stage: string;
  message: string;
  percent?: number;
}
