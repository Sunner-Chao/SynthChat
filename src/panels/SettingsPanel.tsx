import { ChangeEvent, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import {
  AlertTriangle,
  Bot,
  CheckCircle2,
  ChevronRight,
  Edit3,
  Globe,
  ImagePlus,
  Info,
  Loader2,
  Plus,
  Puzzle,
  RefreshCw,
  Search,
  Settings,
  Smartphone,
  Smile,
  Sparkles,
  Terminal,
  Upload,
  Video,
  Wand2,
  Wifi,
  XCircle,
  Palette,
  PlugZap
} from "lucide-react";
import { api } from "../lib/api";
import { filterSkillsByQuery } from "../lib/skillSearch";
import { useAppStore, consumePendingSettingsView } from "../lib/store";
import type {
  AccountConfig,
  AppBuildInfo,
  AppSection,
  AppUpdateCheck,
  AgentConfig,
  AgentDefinition,
  BrowserProvider,
  ChatConfig,
  EmojiGroup,
  ImageProvider,
  LlmProvider,
  ModelCatalogEntry,
  McpServer,
  ModelCapabilities,
  Persona,
  ProfileConfig,
  SearchProvider,
  SkillSummary,
  ThemeConfig,
  VideoProvider,
  VideoSummaryConfig,
  VisionProvider,
  WechatConfig,
  WechatQrStartResult,
  WechatQrStatusResult,
  TokenUsageStats
} from "../lib/types";
import { Avatar, MenuRow } from "../components/common";

const UPDATE_MANIFEST_STORAGE_KEY = "synthchat.update.manifest.url.v1";

function formatTime(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

function isSilentInstallAssetUrl(value?: string | null) {
  return /\.(exe|msi|msix)(?:[?#].*)?$/i.test(value ?? "");
}

function normalizeQrBaseUrl(value?: string | null) {
  const text = value?.trim() ?? "";
  if (!text) return "";
  const trimmed = text.replace(/\/+$/, "");
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  if (trimmed.startsWith("//")) return `https:${trimmed}`;
  return `https://${trimmed.replace(/^\/+/, "")}`;
}

function maskSecret(value?: string | null) {
  const text = value?.trim() ?? "";
  if (!text) return "未记录";
  if (text.length <= 10) return `${text.slice(0, 2)}***`;
  return `${text.slice(0, 6)}...${text.slice(-4)}`;
}

function formatTokenK(tokens: number) {
  if (tokens < 1000) return `${tokens}`;
  if (tokens >= 1_000_000) {
    const m = tokens / 1_000_000;
    return `${Number.isInteger(m) ? m.toFixed(0) : m.toFixed(1)}M`;
  }
  const value = tokens / 1000;
  return `${Number.isInteger(value) ? value.toFixed(0) : value.toFixed(1)}K`;
}

type ModelCapabilityOverrideValue = "auto" | "on" | "off";

function normalizeProviderModels(value: LlmProvider["models"] | undefined): Record<string, Record<string, unknown>> {
  if (!value || typeof value !== "object" || Array.isArray(value)) return {};
  return value as Record<string, Record<string, unknown>>;
}

function currentModelConfig(provider: LlmProvider | null): Record<string, unknown> {
  if (!provider || !provider.model?.trim()) return {};
  const models = normalizeProviderModels(provider.models);
  const config = models[provider.model.trim()];
  return config && typeof config === "object" && !Array.isArray(config) ? config : {};
}

function currentModelCapabilityOverrides(provider: LlmProvider | null): Record<string, unknown> {
  const config = currentModelConfig(provider);
  const capabilities = config.capabilities;
  return capabilities && typeof capabilities === "object" && !Array.isArray(capabilities)
    ? capabilities as Record<string, unknown>
    : {};
}

function capabilityOverrideValue(provider: LlmProvider | null, key: string): ModelCapabilityOverrideValue {
  const value = currentModelCapabilityOverrides(provider)[key];
  if (value === true) return "on";
  if (value === false) return "off";
  return "auto";
}

function setCapabilityOverride(
  provider: LlmProvider,
  key: string,
  next: ModelCapabilityOverrideValue
): LlmProvider {
  const model = provider.model.trim();
  if (!model) return provider;
  const models = { ...normalizeProviderModels(provider.models) };
  const currentConfig = currentModelConfig(provider);
  const nextConfig: Record<string, unknown> = { ...currentConfig };
  const currentCapabilities = currentModelCapabilityOverrides(provider);
  const nextCapabilities: Record<string, unknown> = { ...currentCapabilities };
  if (next === "auto") {
    delete nextCapabilities[key];
  } else {
    nextCapabilities[key] = next === "on";
  }
  if (Object.keys(nextCapabilities).length > 0) {
    nextConfig.capabilities = nextCapabilities;
  } else {
    delete nextConfig.capabilities;
  }
  if (Object.keys(nextConfig).length > 0) {
    models[model] = nextConfig;
  } else {
    delete models[model];
  }
  return { ...provider, models };
}

function providerThinkingEnabled(provider: LlmProvider | null | undefined): boolean {
  const models = normalizeProviderModels(provider?.models);
  return Boolean(models.__provider?.thinkingEnabled);
}

function setProviderThinkingEnabled(provider: LlmProvider, enabled: boolean): LlmProvider {
  const models = { ...normalizeProviderModels(provider.models) };
  const meta = {
    ...(models.__provider && typeof models.__provider === "object" && !Array.isArray(models.__provider)
      ? models.__provider
      : {})
  };
  if (enabled) {
    meta.thinkingEnabled = true;
  } else {
    delete meta.thinkingEnabled;
  }
  if (Object.keys(meta).length > 0) {
    models.__provider = meta;
  } else {
    delete models.__provider;
  }
  return { ...provider, models };
}

function formatCapabilitySource(source?: string) {
  if (!source) return "unknown";
  if (source === "configured") return "手动覆盖";
  if (source === "live") return "实时模型端点";
  if (source === "models.dev") return "models.dev";
  if (source === "curated") return "内置兼容规则";
  if (source === "heuristic") return "名称兜底推断";
  return source;
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

function imageProviderTypeLabel(id: string) {
  const labels: Record<string, string> = {
    openai_image: "OpenAI Image",
    gemini_image: "Gemini Image",
    novelai: "NovelAI"
  };
  return labels[id] ?? id;
}

function readUpdateManifestUrl() {
  if (typeof window === "undefined") return "";
  try {
    return window.localStorage.getItem(UPDATE_MANIFEST_STORAGE_KEY) ?? "";
  } catch {
    return "";
  }
}

function writeUpdateManifestUrl(value: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(UPDATE_MANIFEST_STORAGE_KEY, value.trim());
  } catch {
    // ignore storage errors
  }
}

function cleanNativeError(error: unknown) {
  return String(error).replace(/^bad request:\s*/i, "").trim();
}

type SettingsView =
  | "menu"
  | "profile"
  | "accounts"
  | "providers"
  | "imageProviders"
  | "videoProviders"
  | "searchProviders"
  | "visionProviders"
  | "browserProviders"
  | "videoSummary"
  | "chat"
  | "reply"
  | "theme"
  | "emoji"
  | "agent"
  | "network"
  | "about"
  | "privacy"
  | "statement";

export function SettingsPanel() {
  const {
    config,
    llmProviders,
    mcpServers,
    agentConfig,
    agents,
    skills,
    plugins,
    profile,
    accounts,
    personas,
    imageProviders,
    videoProviders,
    searchProviders,
    visionProviders,
    browserProviders,
    themes,
    emojiGroups,
    saveLlmProviders,
    saveConfig,
    saveProfile,
    uploadProfileAvatar,
    clearProfileAvatar,
    refreshAccounts,
    saveAccounts,
    saveImageProviders,
    saveVideoProviders,
    saveSearchProviders,
    saveVisionProviders,
    saveBrowserProviders,
    saveThemes,
    importThemeCss,
    saveEmojiGroups,
    uploadEmojiImage,
    saveAgentConfig,
    installBuiltinSkills,
    refreshSkills,
    setSection,
    focusedAgentId,
    setSkillsPanelMode,
    setMcpPanelMode
  } = useAppStore();
  const [view, setView] = useState<SettingsView>("menu");
  const [menuAppVersion, setMenuAppVersion] = useState("v1.1.0");
  const focusedAgent = agents.find((agent) => agent.id === focusedAgentId) ?? agents.find((agent) => agent.isDefault) ?? agents[0] ?? null;

  useEffect(() => {
    void getVersion().then((v) => setMenuAppVersion(`v${v}`)).catch(() => {});
  }, []);

  // Read pendingSettingsView once on mount (before paint)
  useLayoutEffect(() => {
    const pending = consumePendingSettingsView();
    if (pending) {
      setView(pending as SettingsView);
    }
  }, []);

  if (!config) {
    return (
      <section className="simple-page">
        <div className="empty-state compact"><Settings size={32} /><p>设置加载中...</p></div>
      </section>
    );
  }

  const saveChat = async (patch: Partial<typeof config.chat>) => {
    await saveConfig({ ...config, chat: { ...config.chat, ...patch } });
  };
  const saveReply = async (patch: Partial<typeof config.reply>) => {
    await saveConfig({ ...config, reply: { ...config.reply, ...patch } });
  };
  const saveWeb = async (patch: Partial<typeof config.web>) => {
    await saveConfig({ ...config, web: { ...config.web, ...patch } });
  };

  if (view !== "menu") {
    const goBackToMenu = () => setView("menu");
    return (
      <section className="simple-page">
        {view === "profile" ? (
          <ProfileSettings
            onBack={goBackToMenu}
            clearAvatar={clearProfileAvatar}
            profile={profile}
            saveProfile={saveProfile}
            uploadAvatar={uploadProfileAvatar}
          />
        ) : null}
        {view === "accounts" ? (
          <AccountsSettings onBack={goBackToMenu} accounts={accounts} personas={personas} refreshAccounts={refreshAccounts} saveAccounts={saveAccounts} />
        ) : null}
        {view === "providers" ? (
          <ProviderSettings onBack={goBackToMenu} providers={llmProviders} saveProviders={saveLlmProviders} />
        ) : null}
        {view === "imageProviders" ? (
          <ImageProviderSettings onBack={goBackToMenu} providers={imageProviders} saveProviders={saveImageProviders} />
        ) : null}
        {view === "videoProviders" ? (
          <VideoProviderSettings onBack={goBackToMenu} providers={videoProviders} saveProviders={saveVideoProviders} />
        ) : null}
        {view === "searchProviders" ? (
          <SearchProviderSettings onBack={goBackToMenu} providers={searchProviders} saveProviders={saveSearchProviders} />
        ) : null}
        {view === "visionProviders" ? (
          <VisionProviderSettings onBack={goBackToMenu} providers={visionProviders} saveProviders={saveVisionProviders} />
        ) : null}
        {view === "browserProviders" ? (
          <BrowserProviderSettings onBack={goBackToMenu} providers={browserProviders} saveProviders={saveBrowserProviders} />
        ) : null}
        {view === "videoSummary" ? (
          <VideoSummarySettings onBack={goBackToMenu} config={config.videoSummary ?? defaultVideoSummaryConfig()} onSave={(patch) => saveConfig({ ...config, videoSummary: { ...(config.videoSummary ?? defaultVideoSummaryConfig()), ...patch } })} />
        ) : null}
        {view === "chat" ? (
          <ChatSettings onBack={goBackToMenu} config={config.chat} llmProviders={llmProviders} onSave={saveChat} />
        ) : null}
        {view === "reply" ? (
          <ReplySettings onBack={goBackToMenu} config={config.reply} onSave={saveReply} />
        ) : null}
        {view === "theme" ? (
          <ThemeSettings onBack={goBackToMenu} importThemeCss={importThemeCss} saveThemes={saveThemes} themes={themes} />
        ) : null}
        {view === "emoji" ? (
          <EmojiSettings onBack={goBackToMenu} groups={emojiGroups} saveGroups={saveEmojiGroups} uploadImage={uploadEmojiImage} />
        ) : null}
        {view === "agent" ? (
          <AgentSettingsRedirect
            agents={agents}
            serversCount={mcpServers.length}
            setSection={setSection}
            skillsCount={skills.length}
          />
        ) : null}
        {view === "network" ? (
          <NetworkSettings onBack={goBackToMenu} config={config.web} weather={config.weather} onSave={saveWeb} onSaveWeather={(patch) => saveConfig({ ...config, weather: { ...config.weather, ...patch } })} />
        ) : null}
        {view === "about" ? (
          <AboutSettings onBack={goBackToMenu} setView={setView} />
        ) : null}
        {view === "privacy" ? (
          <InfoDocument onBack={goBackToMenu}
            title="隐私说明及设置"
            body={[
              "SynthChat 原版包含匿名使用统计开关，用于判断版本使用情况和功能优先级。",
              "重构版默认关闭遥测；不会上传聊天记录、图片、文件、角色设定、世界书、记忆、API Key 或本机文件。",
              `当前状态：${config.telemetryEnabled ? "已开启" : "已关闭"}`
            ]}
          />
        ) : null}
        {view === "statement" ? (
          <InfoDocument onBack={goBackToMenu}
            title="软件声明"
            body={[
              "SynthChat 兼容 SillyTavern 角色卡和世界书格式，是为了方便迁移已有创作。",
              "导入、传播或商用他人角色卡、世界书、图片素材前，请自行确认授权。",
              "重构版目前以本地桌面端为主，公网面板、微信传输和自动更新会逐步补齐。"
            ]}
          />
        ) : null}
      </section>
    );
  }

  return (
    <section className="simple-page">
      <button className="status-card clickable-card" onClick={() => setView("profile")} type="button">
        <Avatar name={profile.name} src={profile.avatarPath ? api.assetUrl(profile.avatarPath) : ""} />
        <div className="info">
          <div className="app-name">{profile.name}</div>
          <div className="version">SynthChat {menuAppVersion}</div>
        </div>
        <ChevronRight size={18} />
      </button>

      <div className="menu-card">
        <MenuRow icon={Smartphone} label="微信账号" value={`${accounts.length} 个`} onClick={() => setView("accounts")} iconColor="green" />
        <MenuRow icon={Wand2} label="对话服务商" value={`${llmProviders.length} 个`} onClick={() => setView("providers")} iconColor="orange" />
        <MenuRow icon={ImagePlus} label="生图服务商" value={`${imageProviders.length} 个`} onClick={() => setView("imageProviders")} iconColor="peach" />
        <MenuRow icon={Video} label="视频生成服务商" value={`${videoProviders.length} 个`} onClick={() => setView("videoProviders")} iconColor="purple" />
        <MenuRow icon={Globe} label="搜索服务" value={`${searchProviders.length} 个`} onClick={() => setView("searchProviders")} iconColor="cyan" />
        <MenuRow icon={Bot} label="识图服务" value={`${visionProviders.length} 个`} onClick={() => setView("visionProviders")} iconColor="indigo" />
        <MenuRow icon={Globe} label="浏览器服务" value={`${browserProviders.length} 个`} onClick={() => setView("browserProviders")} iconColor="blue" />
        <MenuRow icon={Wand2} label="视频总结" value={config.videoSummary?.enabled === false ? "已关闭" : config.videoSummary?.transcriber || "auto"} onClick={() => setView("videoSummary")} iconColor="purple" />
        <MenuRow
          icon={Puzzle}
          label="MCP 扩展"
          value={mcpServers.length ? `${mcpServers.length} 个全局服务` : "未配置"}
          onClick={() => {
            setMcpPanelMode("global");
            setSection("mcp");
          }}
          iconColor="blue"
        />
        <MenuRow
          icon={Bot}
          label="Agent 配置"
          value={focusedAgent?.name || `${agents.length} 个智能体`}
          onClick={() => setSection("agents")}
          iconColor="indigo"
        />
        <MenuRow
          icon={Wand2}
          label="Skills 技能包"
          value={`${skills.length} 个全局技能`}
          onClick={() => {
            setSkillsPanelMode("global");
            setSection("skills");
          }}
          iconColor="purple"
        />
        <MenuRow icon={PlugZap} label="插件管理" value={`${plugins.length} 个`} onClick={() => setSection("plugins")} iconColor="red" />
        <MenuRow icon={Settings} label="对话设置" onClick={() => setView("chat")} iconColor="primary" />
        <MenuRow icon={Edit3} label="回复设置" onClick={() => setView("reply")} iconColor="indigo" />
        <MenuRow icon={Palette} label="主题" value={`${themes.filter((theme) => theme.active).length} 个已应用`} onClick={() => setView("theme")} iconColor="purple" />
        <MenuRow icon={Smile} label="表情包管理" value={`${emojiGroups.length} 个分组`} onClick={() => setView("emoji")} iconColor="yellow" />
        <MenuRow icon={Globe} label="网络设置" onClick={() => setView("network")} iconColor="cyan" />
        <MenuRow
          icon={Terminal}
          label="环境检查"
          value={config.chat?.skipEnvCheck ? "已跳过" : "启动时检查"}
          onClick={async () => {
            const newValue = !(config.chat?.skipEnvCheck ?? false);
            await saveChat({ skipEnvCheck: newValue });
          }}
          iconColor="cyan"
        />
        <MenuRow icon={Info} label="关于 SynthChat" onClick={() => setView("about")} iconColor="neutral" />
      </div>
    </section>
  );
}

function settingsIcon(view: SettingsView) {
  const icons: Partial<Record<SettingsView, typeof Smartphone>> = {
    profile: Smartphone,
    accounts: Smartphone,
    providers: Wand2,
    imageProviders: ImagePlus,
    videoProviders: Video,
    searchProviders: Globe,
    visionProviders: Bot,
    browserProviders: Globe,
    videoSummary: Wand2,
    chat: Settings,
    reply: Edit3,
    theme: Palette,
    emoji: Smile,
    agent: Bot,
    network: Globe,
    about: Info,
    privacy: Info,
    statement: Info
  };
  return icons[view];
}

function settingsTitle(view: SettingsView) {
  const titles: Record<SettingsView, string> = {
    menu: "设置",
    profile: "个人资料",
    accounts: "微信账号",
    providers: "对话服务商",
    imageProviders: "生图服务商",
    videoProviders: "视频生成服务商",
    searchProviders: "搜索服务",
    visionProviders: "识图服务",
    browserProviders: "浏览器服务",
    videoSummary: "视频总结",
    chat: "对话设置",
    reply: "回复设置",
    theme: "主题",
    emoji: "表情包管理",
    agent: "Agent 与 Skills",
    network: "网络设置",
    about: "关于 SynthChat",
    privacy: "隐私说明及设置",
    statement: "软件声明"
  };
  return titles[view];
}

function AgentSettingsRedirect({
  agents,
  serversCount,
  setSection,
  skillsCount
}: {
  agents: AgentDefinition[];
  serversCount: number;
  setSection: (section: AppSection, settingsView?: string) => void;
  skillsCount: number;
}) {
  const enabledAgents = agents.filter((agent) => agent.enabled).length;
  const defaultAgent = agents.find((agent) => agent.isDefault) ?? agents[0] ?? null;
  return (
    <div className="primary-panel embedded-panel" style={{ padding: 0 }}>
      <div className="agent-settings-hero">
        <div className="agent-settings-hero-info">
          <span className="agent-settings-hero-icon"><Bot size={26} /></span>
          <div className="agent-settings-hero-text">
            <strong>Agent 配置</strong>
            <small>{enabledAgents}/{agents.length} 个启用 · {skillsCount} 个 Skills · {serversCount} 个 MCP 服务</small>
          </div>
        </div>
        <button className="btn-primary" type="button" onClick={() => setSection("agents")} style={{ fontSize: 13, padding: "8px 18px", width: "auto", minWidth: "auto", marginLeft: "auto", flexShrink: 0 }}>
          打开 Agent 管理
        </button>
      </div>
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>统一配置位置</strong>
        </div>
        <p className="form-hint">
          Agent 管理页负责模型 fallback、MCP/Skills、Shell 权限和子 Agent 限制；最大工具迭代由通讯录/角色编辑里的工具策略主导。
        </p>
        <div className="agent-summary-grid" style={{ marginTop: 12 }}>
          {defaultAgent ? (
            <div className="agent-summary-item">
              <span className="agent-summary-icon indigo"><Bot size={18} /></span>
              <div className="agent-summary-text">
                <strong style={{ fontSize: 14 }}>{defaultAgent.name}{defaultAgent.isDefault ? " ★" : ""}</strong>
                <small>Agent fallback 预算：{defaultAgent.maxToolIterations ?? 90} 次；实际以角色工具策略为准</small>
              </div>
            </div>
          ) : (
            <p className="form-hint">暂无 Agent，请先创建一个。</p>
          )}
        </div>
      </div>
    </div>
  );
}

function AgentSettings({
  config,
  agents,
  saveConfig,
  skills,
  servers,
  installBuiltinSkills,
  refreshSkills
}: {
  config: AgentConfig;
  agents: AgentDefinition[];
  saveConfig: (config: AgentConfig) => Promise<void>;
  skills: SkillSummary[];
  servers: McpServer[];
  installBuiltinSkills: () => Promise<void>;
  refreshSkills: () => Promise<void>;
}) {
  const [draft, setDraft] = useState(config);
  const [skillSearch, setSkillSearch] = useState("");
  useEffect(() => setDraft(config), [config]);
  const filteredSkills = useMemo(() => filterSkillsByQuery(skills, skillSearch), [skillSearch, skills]);
  const toggleSkill = (id: string) => {
    setDraft((current) => ({
      ...current,
      enabledSkills: current.enabledSkills.includes(id)
        ? current.enabledSkills.filter((item) => item !== id)
        : [...current.enabledSkills, id]
    }));
  };
  const toggleServer = (id: string) => {
    setDraft((current) => ({
      ...current,
      enabledMcpServers: current.enabledMcpServers.includes(id)
        ? current.enabledMcpServers.filter((item) => item !== id)
        : [...current.enabledMcpServers, id]
    }));
  };
  const toggleSetting = (key: "enabled" | "mcpEnabled" | "skillsEnabled" | "allowShell") => {
    setDraft((current) => ({ ...current, [key]: !current[key] }));
  };
  return (
    <div className="primary-panel embedded-panel" style={{ padding: 0 }}>
      {/* Hero Banner */}
      <div className="agent-settings-hero">
        <div className="agent-settings-hero-info">
          <span className="agent-settings-hero-icon"><Bot size={26} /></span>
          <div className="agent-settings-hero-text">
            <strong>Agent 与 Skills</strong>
            <small>{agents.filter((a) => a.enabled).length} 个活跃智能体 · {skills.length} 个技能 · {servers.length} 个 MCP 服务器</small>
          </div>
        </div>
        <button className="btn-primary" type="button" onClick={() => void saveConfig(draft)} style={{ fontSize: 13, padding: "8px 18px", width: "auto", minWidth: "auto", marginLeft: "auto", flexShrink: 0 }}>保存设置</button>
      </div>

      {/* Agent Summary Cards */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>智能体概览</strong>
          <small>{agents.length} 个智能体</small>
        </div>
        <div className="agent-summary-grid">
          {agents.map((agent) => (
            <div className="agent-summary-item" key={agent.id}>
              <span className="agent-summary-icon indigo"><Bot size={18} /></span>
              <div className="agent-summary-text">
                <strong style={{ fontSize: 14 }}>{agent.name}{agent.isDefault ? " ★" : ""}</strong>
                <small>{agent.llmProvider || "跟随角色"} · {agent.llmModel || "未指定模型"}</small>
              </div>
            </div>
          ))}
        </div>
        <div style={{ marginTop: 12 }}>
          <button className="btn-secondary" type="button" onClick={() => useAppStore.getState().setSection("agents", "agent")} style={{ fontSize: 13, padding: "8px 16px" }}>
            管理智能体
          </button>
        </div>
      </div>

      {/* Capability Toggles */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>功能开关</strong>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>Agent 能力</strong>
            <small>启用智能体自主规划和执行</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.enabled} onChange={() => toggleSetting("enabled")} />
            <span className="switch-track" />
          </label>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>MCP 工具</strong>
            <small>允许 Agent 调用 MCP 工具</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.mcpEnabled} onChange={() => toggleSetting("mcpEnabled")} />
            <span className="switch-track" />
          </label>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>Skills 加载</strong>
            <small>启用技能系统</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.skillsEnabled} onChange={() => toggleSetting("skillsEnabled")} />
            <span className="switch-track" />
          </label>
        </div>
        <div className="settings-toggle-row">
          <div className="settings-toggle-info">
            <strong>Shell 工具</strong>
            <small>允许执行 Shell 命令</small>
          </div>
          <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
            <input type="checkbox" checked={draft.allowShell} onChange={() => toggleSetting("allowShell")} />
            <span className="switch-track" />
          </label>
        </div>
      </div>

      {/* Limits */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <RefreshCw size={16} /><strong>Agent 调度限制</strong>
        </div>
        <div className="agent-form-row">
          <div className="agent-field">
            <label>最大子 Agent</label>
            <input min={1} max={32} type="number" value={draft.maxSubagents} onChange={(event) => setDraft((current) => ({ ...current, maxSubagents: Number(event.target.value) }))} />
          </div>
          <div className="agent-field">
            <label>最大子层级</label>
            <input min={1} max={4} type="number" value={draft.maxSubagentDepth ?? 1} onChange={(event) => setDraft((current) => ({ ...current, maxSubagentDepth: Number(event.target.value) }))} />
          </div>
        </div>
        <div className="agent-form-row single" style={{ marginTop: 12 }}>
          <div className="agent-field">
            <label>Skills 目录</label>
            <input value={draft.skillsDir} onChange={(event) => setDraft((current) => ({ ...current, skillsDir: event.target.value }))} placeholder="留空使用内置 skills（项目目录或打包资源目录）" />
          </div>
        </div>
        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <button className="btn-secondary" type="button" onClick={() => void installBuiltinSkills()} style={{ fontSize: 13, padding: "8px 16px" }}>安装默认 Skills</button>
          <button className="btn-secondary-outline" type="button" onClick={() => void refreshSkills()} style={{ fontSize: 13, padding: "8px 16px", border: "1px solid var(--divider)", borderRadius: "var(--radius-sm)", background: "transparent", color: "var(--text-2)", cursor: "pointer" }}>刷新 Skills</button>
        </div>
      </div>

      {/* MCP Servers */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <PlugZap size={16} /><strong>MCP 服务器白名单</strong>
          <small>{draft.enabledMcpServers.length}/{servers.length} 已启用</small>
        </div>
        {servers.length === 0 ? (
          <p className="form-hint">暂无 MCP Server</p>
        ) : (
          <div className="agent-toggle-grid">
            {servers.map((server) => (
              <button className={`agent-toggle-item ${draft.enabledMcpServers.includes(server.id) ? "active" : ""}`} key={server.id} type="button" onClick={() => toggleServer(server.id)}>
                <span className="agent-toggle-item-label"><PlugZap size={16} /><span>{server.name}</span></span>
                <span className="agent-toggle-dot" />
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Skills */}
      <div className="agent-settings-section">
        <div className="agent-settings-section-title">
          <Sparkles size={16} /><strong>Skills</strong>
          <small>
            {draft.enabledSkills.length}/{skills.length} 已启用
            {skillSearch.trim() ? ` · ${filteredSkills.length} 匹配` : ""}
          </small>
        </div>
        {skills.length === 0 ? (
          <p className="form-hint">暂无 Skills</p>
        ) : (
          <>
            <div className="search-bar" style={{ marginBottom: 12 }}>
              <Search size={16} />
              <input
                value={skillSearch}
                onChange={(event) => setSkillSearch(event.target.value)}
                placeholder="搜索技能名称 / ID / 描述"
              />
            </div>
            {filteredSkills.length === 0 ? (
              <p className="form-hint">没有匹配的 Skills</p>
            ) : (
              <div className="agent-toggle-grid">
                {filteredSkills.map((skill) => (
                  <button className={`agent-toggle-item ${draft.enabledSkills.includes(skill.id) ? "active" : ""}`} key={skill.id} type="button" onClick={() => toggleSkill(skill.id)}>
                    <span className="agent-toggle-item-label"><Sparkles size={16} /><span>{skill.name}</span></span>
                    <span className="agent-toggle-dot" />
                  </button>
                ))}
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function BackBtn({ onBack }: { onBack?: () => void }) {
  if (!onBack) return null;
  return (
    <button className="icon-only-btn" onClick={onBack} title="返回" type="button">
      <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
    </button>
  );
}

function ProfileSettings({
  onBack,
  profile,
  saveProfile,
  uploadAvatar,
  clearAvatar
}: {
  onBack?: () => void;
  profile: ProfileConfig;
  saveProfile: (profile: ProfileConfig) => Promise<void>;
  uploadAvatar: (file: File) => Promise<void>;
  clearAvatar: () => Promise<void>;
}) {
  const [name, setName] = useState(profile.name);
  useEffect(() => setName(profile.name), [profile.name]);
  const avatarInput = useRef<HTMLInputElement | null>(null);
  const onAvatar = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) await uploadAvatar(file);
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Profile</span><strong>个人资料</strong></div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="profile-detail">
          <Avatar name={profile.name} src={profile.avatarPath ? api.assetUrl(profile.avatarPath) : ""} size="large" />
          <h2>{profile.name}</h2>
          <p>本地用户资料</p>
          <input accept="image/*" className="hidden-input" onChange={onAvatar} ref={avatarInput} type="file" />
          <div className="inline-actions">
            <button className="btn-primary-outline" onClick={() => avatarInput.current?.click()} type="button">上传头像</button>
            {profile.avatarPath ? <button className="btn-secondary-outline" onClick={() => void clearAvatar()} type="button">清除头像</button> : null}
          </div>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">基本信息</div>
        <div className="form-group">
          <div className="form-row">
            <label>昵称</label>
            <input value={name} onChange={(event) => setName(event.target.value)} />
          </div>
        </div>
        <div className="form-hint">修改后点击保存按钮生效</div>
      </div>
      <div style={{ padding: "0 16px 12px" }}>
        <button className="btn-primary" onClick={() => void saveProfile({ ...profile, name })} type="button">保存资料</button>
      </div>
    </div>
  );
}

function AccountsSettings({
  onBack,
  accounts,
  personas,
  refreshAccounts,
  saveAccounts
}: {
  onBack?: () => void;
  accounts: AccountConfig[];
  personas: Persona[];
  refreshAccounts: () => Promise<void>;
  saveAccounts: (accounts: AccountConfig[]) => Promise<void>;
}) {
  const [wechatConfig, setWechatConfig] = useState<WechatConfig>({ baseUrl: "", timeoutSeconds: 35 });
  const [qr, setQr] = useState<WechatQrStartResult | null>(null);
  const [qrError, setQrError] = useState("");
  const [busy, setBusy] = useState(false);
  const [showQrSheet, setShowQrSheet] = useState(false);
  const [pendingNoteId, setPendingNoteId] = useState("");
  const [noteDraft, setNoteDraft] = useState("");
  const [detailId, setDetailId] = useState("");
  const [bindDraft, setBindDraft] = useState("");
  const [pollStatus, setPollStatus] = useState("");
  const [qrStatusText, setQrStatusText] = useState("");
  const [checking, setChecking] = useState(false);
  const [scanSuccess, setScanSuccess] = useState(false);
  const [pollingDetail, setPollingDetail] = useState(false);
  const qrPollingRef = useRef(false);
  const detail = accounts.find((account) => account.id === detailId) ?? null;

  useEffect(() => {
    void api.getWechatConfig().then(setWechatConfig);
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ accountId?: string; error?: string }>("synthchat-wechat-poll-error", (event) => {
      const accountId = event.payload?.accountId ?? "";
      if (detailId && accountId && accountId !== detailId) return;
      const error = event.payload?.error || "微信后台连接失败";
      setPollStatus(`后台轮询失败：${cleanNativeError(error)}`);
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, [detailId]);

  const pollAccountOnce = async (account: AccountConfig, options?: { quietEmpty?: boolean }) => {
    if (!account.linkedPersona?.trim()) {
      setPollStatus("已登录，但还没有绑定角色；保存角色后微信端才能连接。");
      return;
    }
    setPollingDetail(true);
    setPollStatus("正在测试微信连接...");
    try {
      const result = await api.wechatPollOnce(account.id);
      await refreshAccounts();
      const failed = result.processed.filter((item) => !item.delivered || item.deliveryError);
      if (failed.length > 0) {
        const firstError = failed.find((item) => item.deliveryError)?.deliveryError;
        setPollStatus(firstError ? `微信已连接，但回复发送失败：${firstError}` : "微信已连接，但有消息处理失败。");
        return;
      }
      if (result.receivedCount) {
        setPollStatus(`微信连接正常，收到 ${result.receivedCount} 条，已处理 ${result.processed.length} 条，跳过 ${result.skippedCount} 条。`);
      } else if (!options?.quietEmpty) {
        setPollStatus("微信连接正常，暂无新消息。");
      } else {
        setPollStatus("微信连接正常。");
      }
    } catch (error) {
      await refreshAccounts().catch(() => {});
      setPollStatus(`微信连接失败：${cleanNativeError(error)}`);
    } finally {
      setPollingDetail(false);
    }
  };

  const checkQrOnce = async () => {
    if (!qr?.qrcode || qrPollingRef.current || scanSuccess) return;
    const activeBaseUrl = normalizeQrBaseUrl(qr.baseUrl);
    qrPollingRef.current = true;
    setChecking(true);
    try {
      const status = await api.checkWechatQrStatus(qr.qrcode, activeBaseUrl || qr.baseUrl);
      const redirectedBaseUrl = normalizeQrBaseUrl(status.host);
      if (redirectedBaseUrl && redirectedBaseUrl !== activeBaseUrl) {
        setQr((current) => (
          current?.qrcode === qr.qrcode
            ? { ...current, baseUrl: redirectedBaseUrl }
            : current
        ));
      }
      const normalizedStatus = (status.status || "").trim().toLowerCase();
      if (status.account) {
        const account = status.account;
        setQrError("");
        setQrStatusText("登录成功");
        setScanSuccess(true);
        await refreshAccounts();
        setTimeout(() => {
          setShowQrSheet(false);
          setScanSuccess(false);
          setDetailId(account.id);
          setBindDraft(account.linkedPersona || "");
          if (account.linkedPersona) {
            void pollAccountOnce(account, { quietEmpty: true });
          } else {
            setPollStatus("已登录，但还没有绑定角色；保存角色后微信端才能连接。");
          }
        }, 1200);
      } else if (normalizedStatus === "wait") {
        setQrError("");
        setQrStatusText("等待扫码");
      } else if (normalizedStatus === "scaned") {
        setQrError("");
        setQrStatusText("已扫码，待确认");
      } else if (normalizedStatus === "scaned_but_redirect") {
        setQrError("");
        setQrStatusText("已扫码，正在确认");
      } else if (normalizedStatus === "expired") {
        setQrError("二维码已过期");
        setQrStatusText("二维码已过期");
      } else if (status.message?.trim()) {
        setQrError(status.message.trim());
        setQrStatusText("状态异常");
      }
    } catch (error) {
      const message = String(error);
      if (!message.includes("failed to request wechat QR status") && !message.includes("error sending request for url")) {
        setQrError(message);
        setQrStatusText("状态异常");
      }
    } finally {
      qrPollingRef.current = false;
      setChecking(false);
    }
  };

  useEffect(() => {
    if (!showQrSheet || !qr?.qrcode || scanSuccess) return;
    void checkQrOnce();
    const timer = window.setInterval(() => {
      void checkQrOnce();
    }, 2500);
    return () => window.clearInterval(timer);
  }, [showQrSheet, qr?.qrcode, qr?.baseUrl, scanSuccess]);

  const saveWechat = async (patch: Partial<WechatConfig>) => {
    const saved = await api.saveWechatConfig({ ...wechatConfig, ...patch });
    setWechatConfig(saved);
  };

  const startQr = async () => {
    setBusy(true);
    setQrError("");
    setQr(null);
    setQrStatusText("正在获取二维码");
    setScanSuccess(false);
    setShowQrSheet(true);
    try {
      const saved = await api.saveWechatConfig(wechatConfig);
      setWechatConfig(saved);
      setQr(await api.startWechatQr(saved.baseUrl));
      setQrStatusText("等待扫码");
    } catch (error) {
      setQrStatusText("");
      setQrError(String(error));
    } finally {
      setBusy(false);
    }
  };

  const add = () => {
    void saveAccounts([
      ...accounts,
      {
        id: crypto.randomUUID(),
        note: "未命名账号",
        linkedPersona: "",
        online: false,
        createdAt: new Date().toISOString(),
        botToken: "",
        ilinkUserId: "",
        getUpdatesBuf: "",
        loginBaseUrl: "",
        lastLoginAt: ""
      }
    ]);
  };
  const savePendingNote = async () => {
    if (!pendingNoteId) return;
    const latestAccounts = await api.listAccounts();
    await saveAccounts(latestAccounts.map((account) => (
      account.id === pendingNoteId ? { ...account, note: noteDraft.trim() || account.note } : account
    )));
    setPendingNoteId("");
    setNoteDraft("");
  };
  const saveDetailNote = async () => {
    if (!detail) return;
    const input = document.getElementById("acct-note") as HTMLInputElement | null;
    const note = input?.value.trim() ?? "";
    const latestAccounts = await api.listAccounts();
    const nextAccounts = latestAccounts.map((account) => {
      if (account.id === detail.id) return { ...account, note, linkedPersona: bindDraft };
      if (bindDraft && account.linkedPersona === bindDraft) return { ...account, linkedPersona: "" };
      return account;
    });
    await saveAccounts(nextAccounts);
    setPollStatus("");
    setDetailId("");
  };
  const pollDetailOnce = async () => {
    if (!detail) return;
    await pollAccountOnce(detail);
  };

  if (detail) {
    return (
      <div className="primary-panel embedded-panel">
        <div className="panel-title action-title">
          <button className="icon-only-btn" onClick={() => setDetailId("")} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
          <div className="panel-title-text"><span>Account</span><strong>账号详情</strong></div>
        </div>
        {/* Status + Test Connection */}
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <span>连接状态</span>
            <button className="btn-primary-outline" disabled={pollingDetail} onClick={() => void pollDetailOnce()} type="button" style={{ padding: "4px 12px", fontSize: 12 }}>
              {pollingDetail ? "测试中..." : "测试连接"}
            </button>
          </div>
          <div className="form-group" style={{ padding: "8px 16px 12px" }}>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "8px 16px" }}>
              <div className="detail-row"><span>Bot ID</span><strong>{detail.id}</strong></div>
              <div className="detail-row">
                <span>状态</span>
                <strong className={detail.online ? "status-online" : "status-offline"}>
                  {detail.online ? "● 在线" : "● 离线"}
                </strong>
              </div>
              <div className="detail-row"><span>链接角色</span><strong>{personas.find((persona) => persona.id === detail.linkedPersona)?.name || detail.linkedPersona || "未链接"}</strong></div>
              <div className="detail-row"><span>iLink 用户</span><strong>{detail.ilinkUserId || "未记录"}</strong></div>
              <div className="detail-row"><span>创建时间</span><strong>{detail.createdAt ? formatTime(detail.createdAt) : "未知"}</strong></div>
              <div className="detail-row"><span>最后登录</span><strong>{detail.lastLoginAt ? formatTime(detail.lastLoginAt) : "未记录"}</strong></div>
            </div>
            {pollStatus ? (
              <div style={{
                marginTop: 10,
                padding: "8px 12px",
                borderRadius: "var(--radius-md)",
                fontSize: 13,
                display: "flex",
                alignItems: "center",
                gap: 8,
                background: pollStatus.includes("失败") ? "rgba(239, 68, 68, 0.08)" : pollStatus.includes("正常") || pollStatus.includes("收到") ? "rgba(34, 197, 94, 0.08)" : pollStatus.includes("测试") ? "var(--primary-light)" : "rgba(234, 179, 8, 0.08)",
                border: `1px solid ${pollStatus.includes("失败") ? "rgba(239, 68, 68, 0.15)" : pollStatus.includes("正常") || pollStatus.includes("收到") ? "rgba(34, 197, 94, 0.15)" : pollStatus.includes("测试") ? "rgba(8, 145, 178, 0.15)" : "rgba(234, 179, 8, 0.15)"}`,
                color: pollStatus.includes("失败") ? "var(--danger)" : pollStatus.includes("正常") || pollStatus.includes("收到") ? "#16a34a" : pollStatus.includes("测试") ? "var(--primary)" : "#a16207",
              }}>
                {pollStatus.includes("失败") ? <XCircle size={15} /> : pollStatus.includes("正常") || pollStatus.includes("收到") ? <CheckCircle2 size={15} /> : pollStatus.includes("测试") ? <Loader2 size={15} className="spin" /> : <AlertTriangle size={15} />}
                <span>{pollStatus}</span>
              </div>
            ) : null}
          </div>
        </div>

        {/* Edit Config */}
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header">配置</div>
          <div className="form-group">
            <div className="form-row">
              <label>备注名</label>
              <input id="acct-note" defaultValue={detail.note || ""} placeholder="为账号设置一个备注" />
            </div>
            <div className="form-row">
              <label>链接角色</label>
              <select value={bindDraft} onChange={(event) => setBindDraft(event.target.value)}>
                <option value="">未链接</option>
                {personas.map((persona) => <option key={persona.id} value={persona.id}>{persona.name}</option>)}
              </select>
            </div>
          </div>
          <div className="form-hint" style={{ padding: "0 16px 10px" }}>
            保存链接角色后，后台会自动轮询该微信账号并把手机消息送入对应角色会话。
          </div>
        </div>

        {/* Actions */}
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="form-actions">
            <button className="btn-primary" onClick={() => void saveDetailNote()} type="button">保存配置</button>
            <button className="btn-danger" onClick={() => { if (window.confirm("确定要删除此账号吗？")) { void saveAccounts(accounts.filter((account) => account.id !== detail.id)); setDetailId(""); } }} type="button">删除账号</button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Accounts</span><strong>微信账号</strong></div>
        <button className="icon-only-btn" onClick={() => void startQr()} title="添加账号" type="button" disabled={busy}><Plus size={19} /></button>
      </div>
      {accounts.length === 0 ? (
        <div className="empty-state compact">
          <div className="empty-icon-wrap"><Smartphone size={48} strokeWidth={1.5} /></div>
          <p>没有已登录的微信账号</p>
          <button className="btn-primary" onClick={() => void startQr()} type="button">添加账号</button>
        </div>
      ) : (
        <div className="account-list">
          {accounts.map((account) => (
            <div className="card account-card" key={account.id}>
              <button className="card-row clickable-row" onClick={() => { setDetailId(account.id); setBindDraft(account.linkedPersona || ""); }} type="button">
                <span className="row-icon green"><Smartphone size={18} /></span>
                <div className="account-info">
                  <div className="account-name">
                    <strong>{account.note || `Bot: ${account.id.slice(0, 12)}...`}</strong>
                    <span className={`status-dot ${account.online ? "online" : "offline"}`}>●</span>
                    <span className="status-text">{account.online ? "在线" : "离线"}</span>
                  </div>
                  {account.note ? <div className="account-id">{account.id.slice(0, 20)}...</div> : null}
                  {account.linkedPersona ? <div className="account-linked">已链接到：{personas.find((persona) => persona.id === account.linkedPersona)?.name || account.linkedPersona}</div> : null}
                  {account.createdAt ? <div className="account-time">创建于 {formatTime(account.createdAt)}</div> : null}
                </div>
                <ChevronRight size={18} className="row-arrow" />
              </button>
            </div>
          ))}
        </div>
      )}
      <div style={{ padding: "0 16px 16px" }}>
        <details className="card" style={{ margin: 0, overflow: "hidden" }}>
          <summary className="card-header" style={{ cursor: "pointer", userSelect: "none", display: "flex", alignItems: "center", justifyContent: "flex-start", gap: 6 }}>
            <Settings size={14} />
            <span>高级接口设置</span>
          </summary>
          <div className="form-group" style={{ padding: "4px 16px 8px" }}>
            <div className="form-row">
              <label>微信接口 Base URL</label>
              <input value={wechatConfig.baseUrl} onChange={(event) => setWechatConfig({ ...wechatConfig, baseUrl: event.target.value })} placeholder="http://localhost:3000" />
            </div>
            <div className="form-row">
              <label>轮询超时（秒）</label>
              <input min={5} type="number" value={wechatConfig.timeoutSeconds} onChange={(event) => setWechatConfig({ ...wechatConfig, timeoutSeconds: Number(event.target.value) })} />
            </div>
          </div>
          <div className="form-actions" style={{ padding: "0 16px 12px" }}>
            <button className="btn-secondary" onClick={() => void saveWechat({})} type="button">保存接口</button>
            <button className="btn-secondary" onClick={add} type="button">手动添加测试账号</button>
          </div>
        </details>
      </div>
      {showQrSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowQrSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">扫码登录微信</div>
            {busy ? <div className="empty-state compact"><RefreshCw size={30} /><p>正在获取二维码...</p></div> : null}
            {scanSuccess ? (
              <div className="qr-success-wrap">
                <div className="qr-success-check">
                  <svg viewBox="0 0 52 52" className="qr-success-svg">
                    <circle className="qr-success-circle" cx="26" cy="26" r="24" fill="none" />
                    <path className="qr-success-path" fill="none" d="M14 27l8 8 16-16" />
                  </svg>
                </div>
                <div className="qr-success-text">登录成功</div>
              </div>
            ) : (
              <>
                {qr?.qrImage ? <img className="qr-sheet-img" alt="QR Code" src={qr.qrImage} /> : null}
                {qrError ? <div className="qr-error">{qrError}</div> : null}
                {!qr?.qrImage && qr?.qrcode ? (
                  <div className="qr-raw">
                    <span>接口已返回二维码内容，但图片未生成</span>
                    <code>{qr.qrcode}</code>
                  </div>
                ) : null}
                <div className="qr-status">{qrError ? "二维码状态异常" : qrStatusText || (qr?.qrImage ? "等待扫码" : "正在获取二维码")}</div>
                {!busy && !qr?.qrImage ? <button className="qr-check-btn" onClick={() => void startQr()} type="button">重新获取二维码</button> : null}
              </>
            )}
            <button className="btn-text" onClick={() => setShowQrSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
      {pendingNoteId ? (
        <div className="sheet-backdrop">
          <div className="action-sheet note-sheet">
            <div className="sheet-title">设置账号备注</div>
            <p className="form-hint">为新添加的账号设置一个易于识别的名称</p>
            <input value={noteDraft} onChange={(event) => setNoteDraft(event.target.value)} placeholder="输入备注名" />
            <div className="inline-actions">
              <button onClick={() => setPendingNoteId("")} type="button">跳过</button>
              <button onClick={() => void savePendingNote()} type="button">保存</button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function ProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: LlmProvider[];
  saveProviders: (providers: LlmProvider[]) => Promise<void>;
}) {
  const messages = useAppStore((state) => state.messages);
  const config = useAppStore((state) => state.config);
  const conversations = useAppStore((state) => state.conversations);
  const activeConversationId = useAppStore((state) => state.activeConversationId);
  const personas = useAppStore((state) => state.personas);
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<LlmProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const selected = providers.find((provider) => provider.id === selectedId);
  const [tokenStats, setTokenStats] = useState<Record<string, TokenUsageStats>>({});
  const emptyTokenStats: TokenUsageStats = { promptTokens: 0, completionTokens: 0, totalTokens: 0, callCount: 0 };
  useEffect(() => {
    api.getTokenUsageStats().then((response) => setTokenStats(response.byProvider ?? {})).catch(() => {});
  }, [messages]);
  const resetTokenStats = async (providerId?: string) => {
    await api.resetTokenUsage(providerId).catch(() => {});
    const response = await api.getTokenUsageStats().catch(() => ({ byProvider: {} }));
    setTokenStats(response.byProvider ?? {});
  };
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);
  const [catalogLoading, setCatalogLoading] = useState(false);
  const [catalogSource, setCatalogSource] = useState("");
  const [catalogBaseUrl, setCatalogBaseUrl] = useState("");
  const [catalogError, setCatalogError] = useState("");
  const [draftCapabilities, setDraftCapabilities] = useState<ModelCapabilities | null>(null);
  const [capabilityLoading, setCapabilityLoading] = useState(false);
  const fetchCatalogModels = async (provider: LlmProvider) => {
    setCatalogLoading(true);
    try {
      const result = await api.detectProviderModels(provider);
      setCatalogModels(result.models ?? []);
      setCatalogSource(result.source ?? "");
      setCatalogBaseUrl(result.baseUrl ?? "");
      setCatalogError(result.error ?? "");
    } catch (error) {
      setCatalogModels([]);
      setCatalogSource("");
      setCatalogBaseUrl("");
      setCatalogError(String(error));
    } finally {
      setCatalogLoading(false);
    }
  };
  const fetchDraftCapabilities = async (provider: LlmProvider) => {
    if (!provider.model?.trim()) {
      setDraftCapabilities(null);
      return;
    }
    setCapabilityLoading(true);
    try {
      const result = await api.inferProviderModelCapabilities(provider);
      setDraftCapabilities(result);
    } catch {
      setDraftCapabilities(null);
    } finally {
      setCapabilityLoading(false);
    }
  };
  useEffect(() => {
    if (draft) void fetchCatalogModels(draft);
  }, [draft?.id, draft?.providerType, draft?.baseUrl, draft?.apiKeyEnv, draft?.apiKey]);
  useEffect(() => {
    if (draft) {
      void fetchDraftCapabilities(draft);
    } else {
      setDraftCapabilities(null);
    }
  }, [draft?.id, draft?.providerType, draft?.baseUrl, draft?.apiKeyEnv, draft?.apiKey, draft?.model, JSON.stringify(draft?.models ?? {})]);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((p) => p.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const addProvider = async (preset = "custom") => {
    const defaults = providerPresetDefaults(preset);
    const provider = {
      id: `provider-${crypto.randomUUID()}`,
      name: preset === "custom" ? "自定义服务商" : providerPresetLabel(preset),
      providerType: defaults.providerType,
      preset,
      baseUrl: defaults.baseUrl,
      appendChatPath: defaults.appendChatPath,
      apiKeyEnv: "SYNTHCHAT_LLM_API_KEY",
      apiKey: null,
      model: "",
      enabled: false,
      timeoutSeconds: 60,
      promptCacheMode: "auto",
      promptCacheTtl: "5m",
      promptCacheLayout: "auto"
    };
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders([
      ...providers,
      provider
    ]);
    setShowTypeSheet(false);
  };
  const remove = async (provider: LlmProvider) => {
    await saveProviders(providers.filter((item) => item.id !== provider.id));
    setSelectedId("");
    setDraft(null);
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Providers</span><strong>对话服务商</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加对话服务商" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Wand2 size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的对话服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加对话服务商</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => {
              const stats = tokenStats[provider.id] ?? emptyTokenStats;
              const typeLabel = provider.providerType === "anthropic" ? "Anthropic" :
                provider.providerType === "gemini" ? "Gemini" :
                provider.providerType === "openai_responses" ? "Responses" : "OpenAI";
              return (
                <div className="card provider-item-card" key={provider.id}>
                  <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                    <div className="provider-card-header">
                      <div className="provider-card-left">
                        <div className="provider-badge-stack">
                          <span className={`provider-type-badge ${provider.providerType ?? "openai_compatible"}`}>
                            {typeLabel}
                          </span>
                        </div>
                        <div className="provider-card-info">
                          <strong className="provider-card-name">{provider.name}</strong>
                          <span className="provider-card-model">{provider.model || "未配置模型"}</span>
                        </div>
                      </div>
                      <div className="provider-card-right">
                        <span className={`provider-status-dot ${provider.enabled ? "enabled" : "disabled"}`} />
                        <ChevronRight size={16} className="provider-card-arrow" />
                      </div>
                    </div>
                    {stats.totalTokens > 0 ? (
                      <div className="provider-card-stats">
                        <div className="provider-stats-row">
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon tokens">T</span>
                            <span className="provider-stat-num">{formatTokenK(stats.totalTokens)}</span>
                          </div>
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon prompt">P</span>
                            <span className="provider-stat-num">{formatTokenK(stats.promptTokens)}</span>
                          </div>
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon completion">C</span>
                            <span className="provider-stat-num">{formatTokenK(stats.completionTokens)}</span>
                          </div>
                          <div className="provider-stat-chip">
                            <span className="provider-stat-icon calls">#</span>
                            <span className="provider-stat-num">{stats.callCount}</span>
                          </div>
                          <button
                            className="token-reset-btn"
                            onClick={(e) => { e.stopPropagation(); void resetTokenStats(provider.id); }}
                            title="重置计数"
                            type="button"
                          >
                            <RefreshCw size={11} />
                          </button>
                        </div>
                        <div className="provider-stats-bar">
                          <div
                            className="provider-stats-fill"
                            style={{ width: `${Math.min(100, Math.max(3, Math.round(Math.log10(stats.totalTokens + 1) * 18)))}%` }}
                          />
                        </div>
                      </div>
                    ) : (
                      <div className="provider-card-empty-stats">
                        <span>暂无 Token 消耗记录</span>
                      </div>
                    )}
                  </button>
                </div>
              );
            })}
          </div>
        )
      ) : draft ? (
          <div className="settings-form provider-card">
            <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
            <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((d) => d ? { ...d, enabled: event.target.checked } : d)} type="checkbox" />启用当前服务商</label>
            <label className="checkbox-row"><input checked={providerThinkingEnabled(draft)} onChange={(event) => setDraft((d) => d ? setProviderThinkingEnabled(d, event.target.checked) : d)} type="checkbox" />启用思考卡片</label>
            <small className="form-hint">该服务商被通讯录/角色实际选中时，才会请求 reasoning/thinking，并把返回内容展示为折叠卡片。</small>
            <label>名称<input value={draft.name} onChange={(event) => setDraft((d) => d ? { ...d, name: event.target.value } : d)} /></label>
            <label>类型<select value={draft.providerType ?? "openai_compatible"} onChange={(event) => setDraft((d) => d ? { ...d, providerType: event.target.value } : d)}>
              <option value="openai_compatible">OpenAI Compatible</option>
              <option value="openai_responses">OpenAI Responses</option>
              <option value="anthropic">Anthropic</option>
              <option value="gemini">Google Gemini</option>
            </select></label>
            <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((d) => d ? { ...d, baseUrl: event.target.value } : d)} placeholder={(draft.providerType ?? "openai_compatible") === "gemini" ? "https://generativelanguage.googleapis.com/v1beta" : "https://api.example.com/v1"} /></label>
            {(draft.providerType ?? "openai_compatible") === "openai_compatible" ? (
              <label className="checkbox-row"><input checked={draft.appendChatPath ?? true} onChange={(event) => setDraft((d) => d ? { ...d, appendChatPath: event.target.checked } : d)} type="checkbox" />拼接 /chat/completions</label>
            ) : (draft.providerType ?? "openai_compatible") === "openai_responses" ? (
              <label className="checkbox-row"><input checked={draft.appendChatPath ?? true} onChange={(event) => setDraft((d) => d ? { ...d, appendChatPath: event.target.checked } : d)} type="checkbox" />拼接 /responses</label>
            ) : null}
            <label>模型
              <div className="model-select-row">
                {catalogModels.length > 0 ? (
                  <select
                    value={catalogModels.some((model) => model.id === draft.model) ? draft.model : ""}
                    onChange={(event) => {
                      const value = event.target.value;
                      if (value) setDraft((d) => d ? { ...d, model: value } : d);
                    }}
                  >
                    <option value="">{catalogLoading ? "加载中..." : "从目录选择模型"}</option>
                    {catalogModels.map((model) => (
                      <option key={model.id} value={model.id}>{model.name || model.id}{model.family ? ` (${model.family})` : ""}</option>
                    ))}
                  </select>
                ) : null}
                <input
                  value={draft.model}
                  onChange={(event) => setDraft((d) => d ? { ...d, model: event.target.value } : d)}
                  placeholder={catalogModels.length > 0 ? "或手动输入模型 ID" : "模型 ID"}
                />
                <button
                  className="model-refresh-btn"
                  disabled={catalogLoading}
                  onClick={() => void fetchCatalogModels(draft)}
                  title="刷新模型目录"
                  type="button"
                >
                  {catalogLoading ? "..." : "↻"}
                </button>
              </div>
              {catalogSource || catalogError ? (
                <small className="form-hint">
                  {catalogSource === "live" ? `已通过模型目录端点/API Key 检测模型${catalogBaseUrl ? `（${catalogBaseUrl}）` : ""}` : "使用内置模型目录"}
                  {catalogError ? `：${catalogError}` : ""}
                </small>
              ) : null}
            </label>
            <div className="two-column">
              <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((d) => d ? { ...d, apiKeyEnv: event.target.value } : d)} /></label>
              <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((d) => d ? { ...d, timeoutSeconds: Number(event.target.value) } : d)} /></label>
            </div>
            <div className="two-column">
              <label>Prompt Cache<select value={draft.promptCacheMode ?? "auto"} onChange={(event) => setDraft((d) => d ? { ...d, promptCacheMode: event.target.value } : d)}>
                <option value="auto">自动</option>
                <option value="on">强制开启</option>
                <option value="off">关闭</option>
              </select></label>
              <label>Cache TTL<select value={draft.promptCacheTtl ?? "5m"} onChange={(event) => setDraft((d) => d ? { ...d, promptCacheTtl: event.target.value } : d)}>
                <option value="5m">5 分钟</option>
                <option value="1h">1 小时</option>
              </select></label>
            </div>
            <label>Cache Layout<select value={draft.promptCacheLayout ?? "auto"} onChange={(event) => setDraft((d) => d ? { ...d, promptCacheLayout: event.target.value } : d)}>
              <option value="auto">自动匹配服务商</option>
              <option value="native">Anthropic native content block</option>
              <option value="envelope">OpenAI/OpenRouter envelope</option>
            </select></label>
            <label>API Key（可选）<input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft((d) => d ? { ...d, apiKey: event.target.value || null } : d)} /></label>
            {draft.model?.trim() ? (
              <div className="settings-subpanel">
                <div className="settings-subpanel-head">
                  <strong>模型能力覆盖</strong>
                  <small>{capabilityLoading ? "检测中..." : `当前来源：${formatCapabilitySource(draftCapabilities?.source)}`}</small>
                </div>
                <div className="two-column">
                  <label>原生识图
                    <select
                      value={capabilityOverrideValue(draft, "supportsVision")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsVision", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                  <label>推理模式
                    <select
                      value={capabilityOverrideValue(draft, "supportsReasoning")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsReasoning", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                </div>
                <div className="two-column">
                  <label>工具调用
                    <select
                      value={capabilityOverrideValue(draft, "supportsTools")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsTools", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                  <label>结构化输出
                    <select
                      value={capabilityOverrideValue(draft, "supportsStructuredOutput")}
                      onChange={(event) => setDraft((d) => d ? setCapabilityOverride(d, "supportsStructuredOutput", event.target.value as ModelCapabilityOverrideValue) : d)}
                    >
                      <option value="auto">自动判断</option>
                      <option value="on">强制开启</option>
                      <option value="off">强制关闭</option>
                    </select>
                  </label>
                </div>
                {draftCapabilities ? (
                  <small className="form-hint">
                    当前生效：识图 {draftCapabilities.supports_vision ? "开启" : "关闭"} ·
                    工具 {draftCapabilities.supports_tools ? "开启" : "关闭"} ·
                    推理 {draftCapabilities.supports_reasoning ? "开启" : "关闭"} ·
                    结构化 {draftCapabilities.supports_structured_output ? "开启" : "关闭"}
                    {draftCapabilities.input_modalities?.length ? ` · 输入 ${draftCapabilities.input_modalities.join("/")}` : ""}
                  </small>
                ) : (
                  <small className="form-hint">选择模型后可对当前模型单独指定能力；留在“自动判断”时将使用后端发现结果。</small>
                )}
              </div>
            ) : null}
            <button className="btn-danger-outline" onClick={() => void remove(draft)} type="button">删除服务商</button>
          </div>
        ) : (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Wand2 size={48} strokeWidth={1.5} /></div>
            <p>没有对话服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加</button>
          </div>
        )}
      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">选择对话服务商类型</div>
            {["openai", "openaiResponses", "anthropic", "google", "deepseek", "siliconflow", "custom"].map((preset) => (
              <button className="sheet-item" key={preset} onClick={() => void addProvider(preset)} type="button">{providerPresetLabel(preset)}</button>
            ))}
            <button className="sheet-cancel" onClick={() => setShowTypeSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function ImageProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: ImageProvider[];
  saveProviders: (providers: ImageProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<ImageProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const selected = providers.find((provider) => provider.id === selectedId);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((p) => p.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((p) => p.id === id ? { ...p, enabled: !p.enabled } : p));
  };
  const add = (providerType = "openai_image") => {
    const provider = {
      id: `image-provider-${crypto.randomUUID()}`,
      name: imageProviderTypeLabel(providerType),
      providerType,
      baseUrl: "",
      apiKeyEnv: "SYNTHCHAT_IMAGE_API_KEY",
      apiKey: null,
      model: providerType === "openai_image" ? "gpt-image-2" : "",
      enabled: false,
      timeoutSeconds: 300,
      useSystemProxy: true
    };
    setDraft({ ...provider });
    setSelectedId(provider.id);
    void saveProviders([
      ...providers,
      provider
    ]);
    setShowTypeSheet(false);
  };
  const remove = () => {
    if (!draft) return;
    void saveProviders(providers.filter((provider) => provider.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };
  const imageTypeBadgeClass = (t: string) => {
    if (t === "openai_image") return "openai_image";
    if (t === "gemini_image") return "gemini_image";
    if (t === "novelai") return "novelai";
    return "openai_image";
  };
  const imageTypeLabel = (t: string) => {
    if (t === "openai_image") return "DALL·E";
    if (t === "gemini_image") return "Gemini";
    if (t === "novelai") return "Nai";
    return t;
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Image</span><strong>生图服务商</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加生图服务商" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><ImagePlus size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的生图服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加生图服务商</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className={`provider-type-badge image ${imageTypeBadgeClass(provider.providerType)}`}>
                        {imageTypeLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.model || "未配置模型"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input
                          type="checkbox"
                          checked={provider.enabled}
                          onChange={() => toggleEnabled(provider.id)}
                        />
                        <span className="switch-track" />
                      </label>
                      <ChevronRight size={16} className="provider-card-arrow" />
                    </div>
                  </div>
                </button>
              </div>
            ))}
          </div>
        )
      ) : draft ? (
          <div className="settings-form provider-card">
            <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
            <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((d) => d ? { ...d, enabled: event.target.checked } : d)} type="checkbox" />启用当前服务商</label>
            <label>名称<input value={draft.name} onChange={(event) => setDraft((d) => d ? { ...d, name: event.target.value } : d)} /></label>
            <label>类型<select value={draft.providerType} onChange={(event) => setDraft((d) => d ? { ...d, providerType: event.target.value } : d)}><option value="openai_image">OpenAI Image</option><option value="gemini_image">Gemini Image</option><option value="novelai">NovelAI</option></select></label>
            <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((d) => d ? { ...d, baseUrl: event.target.value } : d)} /></label>
            <div className="two-column">
              <label>模型<input value={draft.model} onChange={(event) => setDraft((d) => d ? { ...d, model: event.target.value } : d)} /></label>
              <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((d) => d ? { ...d, apiKeyEnv: event.target.value } : d)} /></label>
            </div>
            <label>API Key（可选）<input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft((d) => d ? { ...d, apiKey: event.target.value || null } : d)} /></label>
            <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((d) => d ? { ...d, timeoutSeconds: Number(event.target.value) } : d)} /></label>
            <label className="checkbox-row"><input checked={draft.useSystemProxy ?? true} onChange={(event) => setDraft((d) => d ? { ...d, useSystemProxy: event.target.checked } : d)} type="checkbox" />使用系统/环境代理</label>
            <button className="btn-danger-outline" onClick={remove} type="button">删除服务商</button>
          </div>
        ) : (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><ImagePlus size={48} strokeWidth={1.5} /></div>
            <p>没有生图服务商</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加</button>
          </div>
        )}
      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">选择生图服务商类型</div>
            {["openai_image", "gemini_image", "novelai"].map((type) => (
              <button className="sheet-item" key={type} onClick={() => add(type)} type="button">{imageProviderTypeLabel(type)}</button>
            ))}
            <button className="sheet-cancel" onClick={() => setShowTypeSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function VideoProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: VideoProvider[];
  saveProviders: (providers: VideoProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<VideoProvider | null>(null);
  const selected = providers.find((provider) => provider.id === selectedId);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((item) => item.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((p) => p.id === id ? { ...p, enabled: !p.enabled } : p));
  };
  const add = async () => {
    const provider: VideoProvider = {
      id: `video-provider-${crypto.randomUUID()}`,
      name: "视频生成服务",
      providerType: "http-json",
      baseUrl: "",
      apiKeyEnv: "SYNTHCHAT_VIDEO_API_KEY",
      apiKey: null,
      model: "",
      enabled: false,
      timeoutSeconds: 120,
      submitPath: "/generate",
      statusPath: "",
      idPath: "id",
      statusField: "status",
      resultPath: "video.url",
      completedStatuses: ["completed", "succeeded", "success", "ready", "done"],
      failedStatuses: ["failed", "error", "canceled", "cancelled"],
      pollIntervalSeconds: 3,
      maxPollSeconds: 300,
      downloadResult: false
    };
    const nextProviders = [...providers, provider];
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders(nextProviders);
  };
  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((provider) => provider.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };
  const setStatusList = (key: "completedStatuses" | "failedStatuses", value: string) => {
    setDraft((item) => item ? {
      ...item,
      [key]: value.split(",").map((part) => part.trim()).filter(Boolean)
    } : item);
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Video</span><strong>视频生成服务商</strong></div>
        <button className="icon-only-btn" onClick={() => void add()} title="添加视频生成服务商" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Video size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的视频生成服务商</p>
            <button className="btn-primary" onClick={() => void add()} type="button">添加视频生成服务商</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className="provider-type-badge image openai_image">Video</span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.model || provider.baseUrl || "未配置"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input checked={provider.enabled} onChange={() => toggleEnabled(provider.id)} type="checkbox" />
                        <span className="switch-track" />
                      </label>
                      <ChevronRight size={16} className="provider-card-arrow" />
                    </div>
                  </div>
                </button>
              </div>
            ))}
          </div>
        )
      ) : draft ? (
        <div className="settings-form provider-card">
          <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
          <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((item) => item ? { ...item, enabled: event.target.checked } : item)} type="checkbox" />启用当前服务商</label>
          <label>名称<input value={draft.name} onChange={(event) => setDraft((item) => item ? { ...item, name: event.target.value } : item)} /></label>
          <div className="two-column">
            <label>类型<input value={draft.providerType} onChange={(event) => setDraft((item) => item ? { ...item, providerType: event.target.value } : item)} /></label>
            <label>模型<input value={draft.model} onChange={(event) => setDraft((item) => item ? { ...item, model: event.target.value } : item)} /></label>
          </div>
          <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((item) => item ? { ...item, baseUrl: event.target.value } : item)} /></label>
          <div className="two-column">
            <label>提交路径<input value={draft.submitPath} onChange={(event) => setDraft((item) => item ? { ...item, submitPath: event.target.value } : item)} /></label>
            <label>状态路径<input value={draft.statusPath} onChange={(event) => setDraft((item) => item ? { ...item, statusPath: event.target.value } : item)} /></label>
          </div>
          <div className="two-column">
            <label>任务 ID 路径<input value={draft.idPath} onChange={(event) => setDraft((item) => item ? { ...item, idPath: event.target.value } : item)} /></label>
            <label>结果 URL 路径<input value={draft.resultPath} onChange={(event) => setDraft((item) => item ? { ...item, resultPath: event.target.value } : item)} /></label>
          </div>
          <div className="two-column">
            <label>状态字段<input value={draft.statusField} onChange={(event) => setDraft((item) => item ? { ...item, statusField: event.target.value } : item)} /></label>
            <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((item) => item ? { ...item, apiKeyEnv: event.target.value } : item)} /></label>
          </div>
          <label>API Key（可选）<input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft((item) => item ? { ...item, apiKey: event.target.value || null } : item)} /></label>
          <label>完成状态<input value={draft.completedStatuses.join(", ")} onChange={(event) => setStatusList("completedStatuses", event.target.value)} /></label>
          <label>失败状态<input value={draft.failedStatuses.join(", ")} onChange={(event) => setStatusList("failedStatuses", event.target.value)} /></label>
          <div className="two-column">
            <label>请求超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((item) => item ? { ...item, timeoutSeconds: Number(event.target.value) } : item)} /></label>
            <label>最大轮询秒数<input min={1} type="number" value={draft.maxPollSeconds} onChange={(event) => setDraft((item) => item ? { ...item, maxPollSeconds: Number(event.target.value) } : item)} /></label>
          </div>
          <label>轮询间隔秒数<input min={1} type="number" value={draft.pollIntervalSeconds} onChange={(event) => setDraft((item) => item ? { ...item, pollIntervalSeconds: Number(event.target.value) } : item)} /></label>
          <label className="checkbox-row"><input checked={draft.downloadResult} onChange={(event) => setDraft((item) => item ? { ...item, downloadResult: event.target.checked } : item)} type="checkbox" />生成后下载视频到本地 artifact</label>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除服务商</button>
        </div>
      ) : null}
    </div>
  );
}

function SearchProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: SearchProvider[];
  saveProviders: (providers: SearchProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<SearchProvider | null>(null);
  const selected = providers.find((provider) => provider.id === selectedId);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((item) => item.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((p) => p.id === id ? { ...p, enabled: !p.enabled } : p));
  };
  const defaultSearchApiKeyEnv = (providerType: string) => {
    if (providerType === "firecrawl") return "FIRECRAWL_API_KEY";
    if (providerType === "tavily") return "TAVILY_API_KEY";
    if (providerType === "exa") return "EXA_API_KEY";
    if (providerType === "brave-free") return "BRAVE_SEARCH_API_KEY";
    if (providerType === "parallel") return "PARALLEL_API_KEY";
    return "";
  };
  const defaultSearchBaseUrl = (providerType: string) => {
    if (providerType === "searxng") return "http://127.0.0.1:8080";
    if (providerType === "firecrawl") return "https://api.firecrawl.dev";
    if (providerType === "tavily") return "https://api.tavily.com";
    if (providerType === "exa") return "https://api.exa.ai";
    if (providerType === "brave-free") return "https://api.search.brave.com/res/v1/web/search";
    return "";
  };
  const add = async () => {
    const provider: SearchProvider = {
      id: `search-provider-${crypto.randomUUID()}`,
      name: "SearXNG 搜索",
      providerType: "searxng",
      baseUrl: "http://127.0.0.1:8080",
      apiKeyEnv: "",
      apiKey: null,
      enabled: false,
      timeoutSeconds: 10
    };
    const nextProviders = [...providers, provider];
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders(nextProviders);
  };
  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((provider) => provider.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };
  const searchTypeBadgeClass = (t: string) => {
    if (t === "searxng") return "searxng";
    if (t === "ddgs" || t === "duckduckgo_html") return "duckduckgo";
    if (t === "brave-free") return "brave";
    return "searxng";
  };
  const searchTypeLabel = (t: string) => {
    if (t === "searxng") return "SearXNG";
    if (t === "firecrawl") return "Firecrawl";
    if (t === "tavily") return "Tavily";
    if (t === "exa") return "Exa";
    if (t === "brave-free") return "Brave";
    if (t === "parallel") return "Parallel";
    if (t === "ddgs" || t === "duckduckgo_html") return "DDGS";
    return t;
  };
  const updateProviderType = (providerType: string) => {
    setDraft((item) => item ? {
      ...item,
      providerType,
      baseUrl: item.baseUrl || defaultSearchBaseUrl(providerType),
      apiKeyEnv: item.apiKeyEnv || defaultSearchApiKeyEnv(providerType)
    } : item);
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Search</span><strong>搜索服务</strong></div>
        <button className="icon-only-btn" onClick={() => void add()} title="添加搜索服务" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Globe size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的搜索服务</p>
            <button className="btn-primary" onClick={() => void add()} type="button">添加搜索服务</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className={`provider-type-badge search ${searchTypeBadgeClass(provider.providerType)}`}>
                        {searchTypeLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.baseUrl || "未配置地址"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input
                          type="checkbox"
                          checked={provider.enabled}
                          onChange={() => toggleEnabled(provider.id)}
                        />
                        <span className="switch-track" />
                      </label>
                      <ChevronRight size={16} className="provider-card-arrow" />
                    </div>
                  </div>
                </button>
              </div>
            ))}
          </div>
        )
      ) : draft ? (
        <div className="settings-form provider-card">
          <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
          <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((item) => item ? { ...item, enabled: event.target.checked } : item)} type="checkbox" />启用当前搜索服务</label>
          <label>名称<input value={draft.name} onChange={(event) => setDraft((item) => item ? { ...item, name: event.target.value } : item)} /></label>
          <label>类型<select value={draft.providerType} onChange={(event) => updateProviderType(event.target.value)}>
            <option value="searxng">SearXNG</option>
            <option value="firecrawl">Firecrawl</option>
            <option value="tavily">Tavily</option>
            <option value="exa">Exa</option>
            <option value="brave-free">Brave Search</option>
            <option value="parallel">Parallel</option>
            <option value="ddgs">DDGS</option>
          </select></label>
          <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((item) => item ? { ...item, baseUrl: event.target.value } : item)} placeholder="http://127.0.0.1:8080" /></label>
          <label>API Key 环境变量<input value={draft.apiKeyEnv || ""} onChange={(event) => setDraft((item) => item ? { ...item, apiKeyEnv: event.target.value } : item)} placeholder={defaultSearchApiKeyEnv(draft.providerType)} /></label>
          <label>API Key（可选）<input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft((item) => item ? { ...item, apiKey: event.target.value || null } : item)} /></label>
          <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((item) => item ? { ...item, timeoutSeconds: Number(event.target.value) } : item)} /></label>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除搜索服务</button>
        </div>
      ) : null}
    </div>
  );
}

function VisionProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: VisionProvider[];
  saveProviders: (providers: VisionProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<VisionProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const selected = providers.find((provider) => provider.id === selectedId);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((item) => item.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((p) => p.id === id ? { ...p, enabled: !p.enabled } : p));
  };
  const add = async (providerType = "ollama") => {
    const provider: VisionProvider = {
      id: `vision-provider-${crypto.randomUUID()}`,
      name: providerType === "ollama" ? "Ollama Qwen2.5-VL 本地识图" : "OpenAI Compatible 识图",
      providerType,
      baseUrl: providerType === "ollama" ? "http://127.0.0.1:11434" : "",
      apiKeyEnv: "SYNTHCHAT_VISION_API_KEY",
      apiKey: null,
      model: providerType === "ollama" ? "qwen2.5vl:7b" : "",
      enabled: false,
      timeoutSeconds: 60
    };
    const nextProviders = [...providers, provider];
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders(nextProviders);
    setShowTypeSheet(false);
  };
  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((provider) => provider.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };
  const visionTypeBadgeClass = (t: string) => {
    if (t === "ollama") return "ollama";
    if (t === "openai_compatible") return "openai_vision";
    return "ollama";
  };
  const visionTypeLabel = (t: string) => {
    if (t === "ollama") return "Ollama";
    if (t === "openai_compatible") return "OpenAI";
    return t;
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Vision</span><strong>识图服务</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加识图服务" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Bot size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的识图服务</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加识图服务</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className={`provider-type-badge vision ${visionTypeBadgeClass(provider.providerType)}`}>
                        {visionTypeLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.model || "未配置模型"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(e) => e.stopPropagation()}>
                        <input
                          type="checkbox"
                          checked={provider.enabled}
                          onChange={() => toggleEnabled(provider.id)}
                        />
                        <span className="switch-track" />
                      </label>
                      <ChevronRight size={16} className="provider-card-arrow" />
                    </div>
                  </div>
                </button>
              </div>
            ))}
          </div>
        )
      ) : draft ? (
        <div className="settings-form provider-card">
          <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
          <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((item) => item ? { ...item, enabled: event.target.checked } : item)} type="checkbox" />启用当前识图服务</label>
          <label>名称<input value={draft.name} onChange={(event) => setDraft((item) => item ? { ...item, name: event.target.value } : item)} /></label>
          <label>类型<select value={draft.providerType} onChange={(event) => setDraft((item) => item ? { ...item, providerType: event.target.value } : item)}>
            <option value="ollama">Ollama</option>
            <option value="openai_compatible">OpenAI Compatible</option>
          </select></label>
          <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((item) => item ? { ...item, baseUrl: event.target.value } : item)} placeholder={draft.providerType === "ollama" ? "http://127.0.0.1:11434" : "https://api.example.com/v1"} /></label>
          <div className="two-column">
            <label>模型<input value={draft.model} onChange={(event) => setDraft((item) => item ? { ...item, model: event.target.value } : item)} placeholder={draft.providerType === "ollama" ? "qwen2.5vl:7b" : "gpt-4o-mini"} /></label>
            <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((item) => item ? { ...item, timeoutSeconds: Number(event.target.value) } : item)} /></label>
          </div>
          <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((item) => item ? { ...item, apiKeyEnv: event.target.value } : item)} /></label>
          <label>API Key（可选）<input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft((item) => item ? { ...item, apiKey: event.target.value || null } : item)} /></label>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除识图服务</button>
        </div>
      ) : null}
      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">选择识图服务类型</div>
            <button className="sheet-item" onClick={() => void add("ollama")} type="button">Ollama（本地模型）</button>
            <button className="sheet-item" onClick={() => void add("openai_compatible")} type="button">OpenAI Compatible（云端API）</button>
            <button className="sheet-cancel" onClick={() => setShowTypeSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function browserProviderLabel(type: string) {
  if (type === "browser-use") return "Browser Use";
  if (type === "browserbase") return "Browserbase";
  return type;
}

function browserProviderDefaults(providerType: string): Omit<BrowserProvider, "id"> {
  if (providerType === "browserbase") {
    return {
      name: "Browserbase",
      providerType,
      baseUrl: "https://api.browserbase.com",
      apiKeyEnv: "BROWSERBASE_API_KEY",
      apiKey: null,
      projectId: "",
      recordSessions: false,
      enabled: false,
      timeoutSeconds: 30
    };
  }
  return {
    name: "Browser Use",
    providerType: "browser-use",
    baseUrl: "https://api.browser-use.com/api/v3",
    apiKeyEnv: "BROWSER_USE_API_KEY",
    apiKey: null,
    projectId: "",
    recordSessions: false,
    enabled: false,
    timeoutSeconds: 30
  };
}

function BrowserProviderSettings({
  onBack,
  providers,
  saveProviders
}: {
  onBack?: () => void;
  providers: BrowserProvider[];
  saveProviders: (providers: BrowserProvider[]) => Promise<void>;
}) {
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<BrowserProvider | null>(null);
  const [showTypeSheet, setShowTypeSheet] = useState(false);
  const selected = providers.find((provider) => provider.id === selectedId);
  useEffect(() => {
    if (selectedId && !selected) {
      setSelectedId("");
      setDraft(null);
    }
  }, [selected, selectedId]);
  const selectProvider = (id: string) => {
    const provider = providers.find((item) => item.id === id);
    setSelectedId(id);
    setDraft(provider ? { ...provider } : null);
  };
  const saveDraft = async () => {
    if (!draft) return;
    await saveProviders(providers.map((item) => (item.id === draft.id ? draft : item)));
    setSelectedId("");
    setDraft(null);
  };
  const toggleEnabled = (id: string) => {
    void saveProviders(providers.map((provider) => provider.id === id ? { ...provider, enabled: !provider.enabled } : provider));
  };
  const add = async (providerType = "browser-use") => {
    const provider: BrowserProvider = {
      id: `browser-provider-${crypto.randomUUID()}`,
      ...browserProviderDefaults(providerType)
    };
    const nextProviders = [...providers, provider];
    setDraft({ ...provider });
    setSelectedId(provider.id);
    await saveProviders(nextProviders);
    setShowTypeSheet(false);
  };
  const remove = async () => {
    if (!draft) return;
    await saveProviders(providers.filter((provider) => provider.id !== draft.id));
    setSelectedId("");
    setDraft(null);
  };
  const updateProviderType = (providerType: string) => {
    const defaults = browserProviderDefaults(providerType);
    setDraft((item) => item ? {
      ...item,
      providerType: defaults.providerType,
      name: item.name || defaults.name,
      baseUrl: item.baseUrl || defaults.baseUrl,
      apiKeyEnv: item.apiKeyEnv || defaults.apiKeyEnv,
      projectId: item.projectId ?? defaults.projectId
    } : item);
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Browser</span><strong>浏览器服务</strong></div>
        <button className="icon-only-btn" onClick={() => setShowTypeSheet(true)} title="添加浏览器服务" type="button"><Plus size={19} /></button>
      </div>
      {!selectedId ? (
        providers.length === 0 ? (
          <div className="empty-state compact">
            <div className="empty-icon-wrap"><Globe size={48} strokeWidth={1.5} /></div>
            <p>没有已配置的浏览器服务</p>
            <button className="btn-primary" onClick={() => setShowTypeSheet(true)} type="button">添加浏览器服务</button>
          </div>
        ) : (
          <div className="provider-list">
            {providers.map((provider) => (
              <div className="card provider-item-card" key={provider.id}>
                <button className="provider-card-btn" onClick={() => selectProvider(provider.id)} type="button">
                  <div className="provider-card-header">
                    <div className="provider-card-left">
                      <span className="provider-type-badge">
                        {browserProviderLabel(provider.providerType)}
                      </span>
                      <div className="provider-card-info">
                        <strong className="provider-card-name">{provider.name}</strong>
                        <span className="provider-card-model">{provider.baseUrl || "未配置地址"}</span>
                      </div>
                    </div>
                    <div className="provider-card-right">
                      <label className="switch-wrap" onClick={(event) => event.stopPropagation()}>
                        <input
                          type="checkbox"
                          checked={provider.enabled}
                          onChange={() => toggleEnabled(provider.id)}
                        />
                        <span className="switch-track" />
                      </label>
                      <ChevronRight size={16} className="provider-card-arrow" />
                    </div>
                  </div>
                </button>
              </div>
            ))}
          </div>
        )
      ) : draft ? (
        <div className="settings-form provider-card">
          <div className="panel-title action-title"><button className="icon-only-btn" onClick={() => { setSelectedId(""); setDraft(null); }} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button><div className="panel-title-text"><span>Edit</span><strong>{draft.name}</strong></div><button onClick={() => void saveDraft()} type="button">完成</button></div>
          <label className="checkbox-row"><input checked={draft.enabled} onChange={(event) => setDraft((item) => item ? { ...item, enabled: event.target.checked } : item)} type="checkbox" />启用当前浏览器服务</label>
          <label>名称<input value={draft.name} onChange={(event) => setDraft((item) => item ? { ...item, name: event.target.value } : item)} /></label>
          <label>类型<select value={draft.providerType} onChange={(event) => updateProviderType(event.target.value)}>
            <option value="browser-use">Browser Use</option>
            <option value="browserbase">Browserbase</option>
          </select></label>
          <label>Base URL<input value={draft.baseUrl} onChange={(event) => setDraft((item) => item ? { ...item, baseUrl: event.target.value } : item)} placeholder={draft.providerType === "browserbase" ? "https://api.browserbase.com" : "https://api.browser-use.com/api/v3"} /></label>
          <div className="two-column">
            <label>API Key 环境变量<input value={draft.apiKeyEnv} onChange={(event) => setDraft((item) => item ? { ...item, apiKeyEnv: event.target.value } : item)} placeholder={draft.providerType === "browserbase" ? "BROWSERBASE_API_KEY" : "BROWSER_USE_API_KEY"} /></label>
            <label>超时秒数<input min={1} type="number" value={draft.timeoutSeconds} onChange={(event) => setDraft((item) => item ? { ...item, timeoutSeconds: Number(event.target.value) } : item)} /></label>
          </div>
          <label>API Key（可选）<input type="password" value={draft.apiKey ?? ""} onChange={(event) => setDraft((item) => item ? { ...item, apiKey: event.target.value || null } : item)} /></label>
          <label>Project ID<input value={draft.projectId ?? ""} onChange={(event) => setDraft((item) => item ? { ...item, projectId: event.target.value } : item)} placeholder={draft.providerType === "browserbase" ? "Browserbase project id" : "通常无需填写"} /></label>
          <label className="checkbox-row"><input checked={Boolean(draft.recordSessions)} onChange={(event) => setDraft((item) => item ? { ...item, recordSessions: event.target.checked } : item)} type="checkbox" />自动录制浏览器会话</label>
          <p className="form-hint">Agent 会优先使用静态页面快照、表单结构和请求线索；只有这些信息不足时才创建真实浏览器会话。</p>
          <button className="btn-danger-outline" onClick={() => void remove()} type="button">删除浏览器服务</button>
        </div>
      ) : null}
      {showTypeSheet ? (
        <div className="sheet-backdrop" onClick={() => setShowTypeSheet(false)}>
          <div className="action-sheet" onClick={(event) => event.stopPropagation()}>
            <div className="sheet-title">选择浏览器服务类型</div>
            <button className="sheet-item" onClick={() => void add("browser-use")} type="button">Browser Use</button>
            <button className="sheet-item" onClick={() => void add("browserbase")} type="button">Browserbase</button>
            <button className="sheet-cancel" onClick={() => setShowTypeSheet(false)} type="button">取消</button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function defaultVideoSummaryConfig(): VideoSummaryConfig {
  return {
    enabled: true,
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
  };
}

function VideoSummarySettings({
  onBack,
  config,
  onSave
}: {
  onBack?: () => void;
  config: VideoSummaryConfig;
  onSave: (patch: Partial<VideoSummaryConfig>) => Promise<void>;
}) {
  const [draft, setDraft] = useState<VideoSummaryConfig>(() => ({ ...defaultVideoSummaryConfig(), ...config }));
  useEffect(() => setDraft({ ...defaultVideoSummaryConfig(), ...config }), [config]);
  const update = <K extends keyof VideoSummaryConfig>(key: K, value: VideoSummaryConfig[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };
  const save = () => void onSave(draft);

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Video</span><strong>视频总结</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="settings-form provider-card">
        <label className="checkbox-row">
          <input checked={draft.enabled} onChange={(event) => update("enabled", event.target.checked)} type="checkbox" />
          无字幕时启用本地音频转写
        </label>
        <div className="two-column">
          <label>转写引擎
            <select value={draft.transcriber} onChange={(event) => update("transcriber", event.target.value)}>
              <option value="auto">auto</option>
              <option value="faster_whisper">faster-whisper</option>
              <option value="sensevoice">SenseVoice</option>
              <option value="none">关闭</option>
            </select>
          </label>
          <label>模型根目录
            <input value={draft.modelsDir} onChange={(event) => update("modelsDir", event.target.value)} placeholder="留空自动发现 models 目录" />
          </label>
        </div>
        <div className="two-column">
          <label>yt-dlp 命令
            <input value={draft.ytDlpCommand} onChange={(event) => update("ytDlpCommand", event.target.value)} placeholder="yt-dlp" />
          </label>
          <label>ffmpeg 目录
            <input value={draft.ffmpegBinPath} onChange={(event) => update("ffmpegBinPath", event.target.value)} placeholder="留空使用 PATH" />
          </label>
        </div>
        <label>Bilibili / yt-dlp Cookie
          <input type="password" value={draft.cookie} onChange={(event) => update("cookie", event.target.value)} placeholder="SESSDATA=...; bili_jct=...; DedeUserID=..." />
        </label>
        <label>cookies.txt 文件路径
          <input value={draft.cookieFile} onChange={(event) => update("cookieFile", event.target.value)} placeholder="Netscape cookies.txt，可由浏览器扩展导出" />
        </label>
        <div className="two-column">
          <label>请求超时（秒）
            <input min={3} type="number" value={draft.timeoutSeconds} onChange={(event) => update("timeoutSeconds", Number(event.target.value))} />
          </label>
          <label>元数据超时（秒）
            <input min={10} type="number" value={draft.ytdlpInfoTimeoutSeconds} onChange={(event) => update("ytdlpInfoTimeoutSeconds", Number(event.target.value))} />
          </label>
        </div>
        <div className="two-column">
          <label>音频下载超时（秒）
            <input min={30} type="number" value={draft.downloadTimeoutSeconds} onChange={(event) => update("downloadTimeoutSeconds", Number(event.target.value))} />
          </label>
          <label>输出目录
            <input value={draft.outputDir} onChange={(event) => update("outputDir", event.target.value)} placeholder="留空使用应用数据目录" />
          </label>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">faster-whisper</div>
        <div className="settings-form">
          <div className="two-column">
            <label>模型名
              <input value={draft.fasterWhisperModel} onChange={(event) => update("fasterWhisperModel", event.target.value)} placeholder="small" />
            </label>
            <label>模型目录
              <input value={draft.fasterWhisperModelDir} onChange={(event) => update("fasterWhisperModelDir", event.target.value)} placeholder="留空使用 models/faster-whisper/small" />
            </label>
          </div>
          <div className="two-column">
            <label>设备
              <select value={draft.fasterWhisperDevice} onChange={(event) => update("fasterWhisperDevice", event.target.value)}>
                <option value="cpu">cpu</option>
                <option value="cuda">cuda</option>
                <option value="auto">auto</option>
              </select>
            </label>
            <label>计算类型
              <select value={draft.fasterWhisperComputeType} onChange={(event) => update("fasterWhisperComputeType", event.target.value)}>
                <option value="int8">int8</option>
                <option value="float16">float16</option>
                <option value="float32">float32</option>
              </select>
            </label>
          </div>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">SenseVoice</div>
        <div className="settings-form">
          <div className="two-column">
            <label>模型目录
              <input value={draft.senseVoiceModelDir} onChange={(event) => update("senseVoiceModelDir", event.target.value)} placeholder="留空使用 models/sensevoice/SenseVoiceSmall" />
            </label>
            <label>设备
              <select value={draft.senseVoiceDevice} onChange={(event) => update("senseVoiceDevice", event.target.value)}>
                <option value="cpu">cpu</option>
                <option value="cuda">cuda</option>
              </select>
            </label>
          </div>
        </div>
        <div className="form-hint">SenseVoice 需要 Python 环境安装 funasr；未安装时 auto 会继续尝试 faster-whisper。</div>
      </div>
    </div>
  );
}

function ChatSettings({
  onBack,
  config,
  llmProviders,
  onSave
}: {
  onBack?: () => void;
  config: ChatConfig;
  llmProviders: LlmProvider[];
  onSave: (patch: Partial<ChatConfig>) => Promise<void>;
}) {
  const [wait, setWait] = useState(config.queueWaitSeconds);
  const [dedupEnabled, setDedupEnabled] = useState(config.messageDedupEnabled !== false);
  const [dedupWindow, setDedupWindow] = useState(config.messageDedupWindowSeconds ?? 30);
  const [runTimeout, setRunTimeout] = useState(config.agentRunTimeoutSeconds ?? 600);
  const [busyInputMode, setBusyInputMode] = useState(config.busyInputMode ?? "queue");
  const [shortContextAbortOnSummaryFailure, setShortContextAbortOnSummaryFailure] = useState(config.shortContextAbortOnSummaryFailure === true);
  const [shortContextSummaryProviderId, setShortContextSummaryProviderId] = useState(config.shortContextSummaryProviderId ?? "");
  const [shortContextSummaryModel, setShortContextSummaryModel] = useState(config.shortContextSummaryModel ?? "");
  const [autoTitleEnabled, setAutoTitleEnabled] = useState(config.autoTitleEnabled !== false);
  const [uiLimit, setUiLimit] = useState(config.uiMessageLimit ?? 180);
  const [artifactLimit, setArtifactLimit] = useState(config.artifactScanLimit ?? 80);
  const [previewChars, setPreviewChars] = useState(config.uiMessagePreviewChars ?? 12000);
  const [streamChars, setStreamChars] = useState(config.uiStreamCharsPerSecond ?? 36);
  const [thinkingMs, setThinkingMs] = useState(config.thinkingMinVisibleMs ?? 1800);
  const [petCloudDurationSeconds, setPetCloudDurationSeconds] = useState(config.petCloudDurationSeconds ?? 10);
  const [bottomThreshold, setBottomThreshold] = useState(config.bottomFollowThresholdPx ?? 180);
  const [activePollMs, setActivePollMs] = useState(config.activePollIntervalMs ?? 1500);
  const [idlePollMs, setIdlePollMs] = useState(config.idlePollIntervalMs ?? 3000);
  const [intentMode, setIntentMode] = useState(config.intentAnalyzerMode ?? "llm");
  const [intentProviderId, setIntentProviderId] = useState(config.intentAnalyzerProviderId ?? "");
  const [intentModel, setIntentModel] = useState(config.intentAnalyzerModel ?? "");
  const [intentEmbeddingConfidence, setIntentEmbeddingConfidence] = useState(config.intentEmbeddingMinConfidence ?? 0.62);
  const [intentLlmConfidence, setIntentLlmConfidence] = useState(config.intentLlmMinConfidence ?? 0.65);
  const [intentLlmTimeout, setIntentLlmTimeout] = useState(config.intentLlmTimeoutSeconds ?? 10);
  const [intentLlmMaxTokens, setIntentLlmMaxTokens] = useState(config.intentLlmMaxTokens ?? 384);
  const [intentLlmPrompt, setIntentLlmPrompt] = useState(config.intentLlmPrompt ?? "");
  const [routerEnabled, setRouterEnabled] = useState(config.toolRouterLlmEnabled !== false);
  const [routerConfidence, setRouterConfidence] = useState(config.toolRouterLlmMinConfidence ?? 0.72);
  const [routerTimeout, setRouterTimeout] = useState(config.toolRouterLlmTimeoutSeconds ?? 15);
  const [routerMaxTokens, setRouterMaxTokens] = useState(config.toolRouterLlmMaxTokens ?? 2048);
  const [routerPrompt, setRouterPrompt] = useState(config.toolRouterLlmPrompt ?? "");
  const [toolUseEnforcement, setToolUseEnforcement] = useState(config.toolUseEnforcement ?? "auto");
  const [toolParallelEnabled, setToolParallelEnabled] = useState(config.toolParallelEnabled !== false);
  const [toolParallelLimit, setToolParallelLimit] = useState(config.toolParallelLimit ?? 4);
  const [sendMessageToolEnabled, setSendMessageToolEnabled] = useState(config.sendMessageToolEnabled === true);
  const [toolApprovalMode, setToolApprovalMode] = useState(config.toolApprovalMode ?? "risky");
  const [trustedToolPatterns, setTrustedToolPatterns] = useState(config.trustedToolPatterns ?? []);
  const [trustedToolPatternDraft, setTrustedToolPatternDraft] = useState("");
  const [trustedCommandPatterns, setTrustedCommandPatterns] = useState(config.trustedCommandPatterns ?? []);
  const [trustedCommandPatternDraft, setTrustedCommandPatternDraft] = useState("");
  const [llmCredentialPoolStrategy, setLlmCredentialPoolStrategy] = useState(config.llmCredentialPoolStrategy ?? "fill_first");
  const [toolEnvPassthroughDraft, setToolEnvPassthroughDraft] = useState((config.toolEnvPassthrough ?? []).join("\n"));
  const [llmRetryCount, setLlmRetryCount] = useState(config.llmRetryCount ?? 2);
  const [llmRetryBackoff, setLlmRetryBackoff] = useState(config.llmRetryBackoffMs ?? 800);
  const [responsesReasoningReplayEnabled, setResponsesReasoningReplayEnabled] = useState(config.responsesReasoningReplayEnabled !== false);
  const [toolRetryCount, setToolRetryCount] = useState(config.toolCallRetryCount ?? 1);
  const [toolRetryBackoff, setToolRetryBackoff] = useState(config.toolCallRetryBackoffMs ?? 300);
  const [guardWarningsEnabled, setGuardWarningsEnabled] = useState(config.toolGuardrailWarningsEnabled !== false);
  const [guardHardStopEnabled, setGuardHardStopEnabled] = useState(config.toolGuardrailHardStopEnabled === true);
  const [guardExactWarnAfter, setGuardExactWarnAfter] = useState(config.toolGuardrailExactFailureWarnAfter ?? 2);
  const [guardSameToolWarnAfter, setGuardSameToolWarnAfter] = useState(config.toolGuardrailSameToolFailureWarnAfter ?? 3);
  const [guardNoProgressWarnAfter, setGuardNoProgressWarnAfter] = useState(config.toolGuardrailNoProgressWarnAfter ?? 2);
  const [guardExactLimit, setGuardExactLimit] = useState(config.toolGuardrailExactFailureLimit ?? 5);
  const [guardSameToolLimit, setGuardSameToolLimit] = useState(config.toolGuardrailSameToolFailureLimit ?? 8);
  const [guardNoProgressLimit, setGuardNoProgressLimit] = useState(config.toolGuardrailNoProgressLimit ?? 5);
  const [backgroundSkillReviewEnabled, setBackgroundSkillReviewEnabled] = useState(config.backgroundSkillReviewEnabled !== false);
  const [backgroundSkillReviewAutoCreateEnabled, setBackgroundSkillReviewAutoCreateEnabled] = useState(config.backgroundSkillReviewAutoCreateEnabled === true);
  const [backgroundSkillCuratorEnabled, setBackgroundSkillCuratorEnabled] = useState(config.backgroundSkillCuratorEnabled !== false);
  const [backgroundSkillCuratorIntervalHours, setBackgroundSkillCuratorIntervalHours] = useState(config.backgroundSkillCuratorIntervalHours ?? 168);
  const [skillHotReloadEnabled, setSkillHotReloadEnabled] = useState(config.skillHotReloadEnabled !== false);
  const [skillHotReloadInterval, setSkillHotReloadInterval] = useState(config.skillHotReloadIntervalSeconds ?? 3);
  const [cleanupEnabled, setCleanupEnabled] = useState(config.historyCleanupEnabled !== false);
  const [retentionDays, setRetentionDays] = useState(config.historyRetentionDays ?? 14);
  const [storedMessages, setStoredMessages] = useState(config.maxStoredMessagesPerConversation ?? 300);
  const [storedRuns, setStoredRuns] = useState(config.maxStoredAgentRuns ?? 50);
  const [storedTraces, setStoredTraces] = useState(config.maxStoredToolTraces ?? 100);

  useEffect(() => {
    setWait(config.queueWaitSeconds);
    setDedupEnabled(config.messageDedupEnabled !== false);
    setDedupWindow(config.messageDedupWindowSeconds ?? 30);
    setRunTimeout(config.agentRunTimeoutSeconds ?? 600);
    setBusyInputMode(config.busyInputMode ?? "queue");
    setShortContextAbortOnSummaryFailure(config.shortContextAbortOnSummaryFailure === true);
    setShortContextSummaryProviderId(config.shortContextSummaryProviderId ?? "");
    setShortContextSummaryModel(config.shortContextSummaryModel ?? "");
    setAutoTitleEnabled(config.autoTitleEnabled !== false);
    setUiLimit(config.uiMessageLimit ?? 180);
    setArtifactLimit(config.artifactScanLimit ?? 80);
    setPreviewChars(config.uiMessagePreviewChars ?? 12000);
    setStreamChars(config.uiStreamCharsPerSecond ?? 36);
    setThinkingMs(config.thinkingMinVisibleMs ?? 1800);
    setPetCloudDurationSeconds(config.petCloudDurationSeconds ?? 10);
    setBottomThreshold(config.bottomFollowThresholdPx ?? 180);
    setActivePollMs(config.activePollIntervalMs ?? 1500);
    setIdlePollMs(config.idlePollIntervalMs ?? 3000);
    setIntentMode(config.intentAnalyzerMode ?? "llm");
    setIntentProviderId(config.intentAnalyzerProviderId ?? "");
    setIntentModel(config.intentAnalyzerModel ?? "");
    setIntentEmbeddingConfidence(config.intentEmbeddingMinConfidence ?? 0.62);
    setIntentLlmConfidence(config.intentLlmMinConfidence ?? 0.65);
    setIntentLlmTimeout(config.intentLlmTimeoutSeconds ?? 10);
    setIntentLlmMaxTokens(config.intentLlmMaxTokens ?? 384);
    setIntentLlmPrompt(config.intentLlmPrompt ?? "");
    setRouterEnabled(config.toolRouterLlmEnabled !== false);
    setRouterConfidence(config.toolRouterLlmMinConfidence ?? 0.72);
    setRouterTimeout(config.toolRouterLlmTimeoutSeconds ?? 15);
    setRouterMaxTokens(config.toolRouterLlmMaxTokens ?? 2048);
    setRouterPrompt(config.toolRouterLlmPrompt ?? "");
    setToolUseEnforcement(config.toolUseEnforcement ?? "auto");
    setToolParallelEnabled(config.toolParallelEnabled !== false);
    setToolParallelLimit(config.toolParallelLimit ?? 4);
    setSendMessageToolEnabled(config.sendMessageToolEnabled === true);
    setToolApprovalMode(config.toolApprovalMode ?? "risky");
    setTrustedToolPatterns(config.trustedToolPatterns ?? []);
    setTrustedToolPatternDraft("");
    setTrustedCommandPatterns(config.trustedCommandPatterns ?? []);
    setTrustedCommandPatternDraft("");
    setLlmCredentialPoolStrategy(config.llmCredentialPoolStrategy ?? "fill_first");
    setToolEnvPassthroughDraft((config.toolEnvPassthrough ?? []).join("\n"));
    setLlmRetryCount(config.llmRetryCount ?? 2);
    setLlmRetryBackoff(config.llmRetryBackoffMs ?? 800);
    setResponsesReasoningReplayEnabled(config.responsesReasoningReplayEnabled !== false);
    setToolRetryCount(config.toolCallRetryCount ?? 1);
    setToolRetryBackoff(config.toolCallRetryBackoffMs ?? 300);
    setGuardWarningsEnabled(config.toolGuardrailWarningsEnabled !== false);
    setGuardHardStopEnabled(config.toolGuardrailHardStopEnabled === true);
    setGuardExactWarnAfter(config.toolGuardrailExactFailureWarnAfter ?? 2);
    setGuardSameToolWarnAfter(config.toolGuardrailSameToolFailureWarnAfter ?? 3);
    setGuardNoProgressWarnAfter(config.toolGuardrailNoProgressWarnAfter ?? 2);
    setGuardExactLimit(config.toolGuardrailExactFailureLimit ?? 5);
    setGuardSameToolLimit(config.toolGuardrailSameToolFailureLimit ?? 8);
    setGuardNoProgressLimit(config.toolGuardrailNoProgressLimit ?? 5);
    setBackgroundSkillReviewEnabled(config.backgroundSkillReviewEnabled !== false);
    setBackgroundSkillReviewAutoCreateEnabled(config.backgroundSkillReviewAutoCreateEnabled === true);
    setBackgroundSkillCuratorEnabled(config.backgroundSkillCuratorEnabled !== false);
    setBackgroundSkillCuratorIntervalHours(config.backgroundSkillCuratorIntervalHours ?? 168);
    setSkillHotReloadEnabled(config.skillHotReloadEnabled !== false);
    setSkillHotReloadInterval(config.skillHotReloadIntervalSeconds ?? 3);
    setCleanupEnabled(config.historyCleanupEnabled !== false);
    setRetentionDays(config.historyRetentionDays ?? 14);
    setStoredMessages(config.maxStoredMessagesPerConversation ?? 300);
    setStoredRuns(config.maxStoredAgentRuns ?? 50);
    setStoredTraces(config.maxStoredToolTraces ?? 100);
  }, [config.activePollIntervalMs, config.agentRunTimeoutSeconds, config.artifactScanLimit, config.autoTitleEnabled, config.backgroundSkillCuratorEnabled, config.backgroundSkillCuratorIntervalHours, config.backgroundSkillReviewAutoCreateEnabled, config.backgroundSkillReviewEnabled, config.bottomFollowThresholdPx, config.busyInputMode, config.delegationInheritMcpToolsets, config.delegationMaxConcurrentChildren, config.delegationOrchestratorEnabled, config.delegationStrategy, config.delegationSubagentAutoApprove, config.delegationSubagentModel, config.delegationSubagentProviderId, config.historyCleanupEnabled, config.historyRetentionDays, config.idlePollIntervalMs, config.intentAnalyzerMode, config.intentAnalyzerModel, config.intentAnalyzerProviderId, config.intentEmbeddingMinConfidence, config.intentLlmMaxTokens, config.intentLlmMinConfidence, config.intentLlmPrompt, config.intentLlmTimeoutSeconds, config.llmCredentialPoolStrategy, config.llmRetryBackoffMs, config.llmRetryCount, config.maxStoredAgentRuns, config.maxStoredMessagesPerConversation, config.maxStoredToolTraces, config.messageDedupEnabled, config.messageDedupWindowSeconds, config.petCloudDurationSeconds, config.queueWaitSeconds, config.responsesReasoningReplayEnabled, config.sendMessageToolEnabled, config.shortContextAbortOnSummaryFailure, config.shortContextSummaryModel, config.shortContextSummaryProviderId, config.skillHotReloadEnabled, config.skillHotReloadIntervalSeconds, config.thinkingMinVisibleMs, config.toolApprovalMode, config.toolCallRetryBackoffMs, config.toolCallRetryCount, config.toolEnvPassthrough, config.toolGuardrailExactFailureLimit, config.toolGuardrailExactFailureWarnAfter, config.toolGuardrailHardStopEnabled, config.toolGuardrailNoProgressLimit, config.toolGuardrailNoProgressWarnAfter, config.toolGuardrailSameToolFailureLimit, config.toolGuardrailSameToolFailureWarnAfter, config.toolGuardrailWarningsEnabled, config.toolParallelEnabled, config.toolParallelLimit, config.toolRouterLlmEnabled, config.toolRouterLlmMaxTokens, config.toolRouterLlmMinConfidence, config.toolRouterLlmPrompt, config.toolRouterLlmTimeoutSeconds, config.toolUseEnforcement, config.trustedCommandPatterns, config.trustedToolPatterns, config.uiMessageLimit, config.uiMessagePreviewChars, config.uiStreamCharsPerSecond]);

  const save = () => void onSave({
    busyInputMode: busyInputMode,
    messageDedupEnabled: dedupEnabled,
    messageDedupWindowSeconds: dedupWindow,
    shortContextAbortOnSummaryFailure: shortContextAbortOnSummaryFailure,
    shortContextSummaryProviderId: shortContextSummaryProviderId,
    shortContextSummaryModel: shortContextSummaryModel,
    autoTitleEnabled: autoTitleEnabled,
    queueWaitSeconds: wait,
    agentRunTimeoutSeconds: runTimeout,
    uiMessageLimit: uiLimit,
    artifactScanLimit: artifactLimit,
    uiMessagePreviewChars: previewChars,
    uiStreamCharsPerSecond: streamChars,
    thinkingMinVisibleMs: thinkingMs,
    petCloudDurationSeconds: petCloudDurationSeconds,
    bottomFollowThresholdPx: bottomThreshold,
    activePollIntervalMs: activePollMs,
    idlePollIntervalMs: idlePollMs,
    intentAnalyzerMode: intentMode === "llm" ? "llm" : "embedding",
    intentAnalyzerProviderId: intentProviderId,
    intentAnalyzerModel: intentModel,
    intentEmbeddingMinConfidence: intentEmbeddingConfidence,
    intentLlmMinConfidence: intentLlmConfidence,
    intentLlmTimeoutSeconds: intentLlmTimeout,
    intentLlmMaxTokens: intentLlmMaxTokens,
    intentLlmPrompt: intentLlmPrompt,
    toolRouterMode: "llm_unified",
    toolRouterLlmEnabled: routerEnabled,
    toolRouterLlmMinConfidence: routerConfidence,
    toolRouterLlmTimeoutSeconds: routerTimeout,
    toolRouterLlmMaxTokens: routerMaxTokens,
    toolRouterLlmPrompt: routerPrompt,
    toolUseEnforcement: toolUseEnforcement,
    toolParallelEnabled: toolParallelEnabled,
    toolParallelLimit: toolParallelLimit,
    sendMessageToolEnabled: sendMessageToolEnabled,
    toolApprovalMode: toolApprovalMode,
    trustedToolPatterns: trustedToolPatterns,
    trustedCommandPatterns: trustedCommandPatterns,
    llmCredentialPoolStrategy: llmCredentialPoolStrategy,
    toolEnvPassthrough: toolEnvPassthroughDraft
      .split(/[\n,;]/)
      .map((item) => item.trim())
      .filter(Boolean),
    llmRetryCount: llmRetryCount,
    llmRetryBackoffMs: llmRetryBackoff,
    responsesReasoningReplayEnabled: responsesReasoningReplayEnabled,
    toolCallRetryCount: toolRetryCount,
    toolCallRetryBackoffMs: toolRetryBackoff,
    toolGuardrailWarningsEnabled: guardWarningsEnabled,
    toolGuardrailHardStopEnabled: guardHardStopEnabled,
    toolGuardrailExactFailureWarnAfter: guardExactWarnAfter,
    toolGuardrailSameToolFailureWarnAfter: guardSameToolWarnAfter,
    toolGuardrailNoProgressWarnAfter: guardNoProgressWarnAfter,
    toolGuardrailExactFailureLimit: guardExactLimit,
    toolGuardrailSameToolFailureLimit: guardSameToolLimit,
    toolGuardrailNoProgressLimit: guardNoProgressLimit,
    backgroundSkillReviewEnabled: backgroundSkillReviewEnabled,
    backgroundSkillReviewAutoCreateEnabled: backgroundSkillReviewAutoCreateEnabled,
    backgroundSkillCuratorEnabled: backgroundSkillCuratorEnabled,
    backgroundSkillCuratorIntervalHours: backgroundSkillCuratorIntervalHours,
    skillHotReloadEnabled: skillHotReloadEnabled,
    skillHotReloadIntervalSeconds: skillHotReloadInterval,
    historyCleanupEnabled: cleanupEnabled,
    historyRetentionDays: retentionDays,
    maxStoredMessagesPerConversation: storedMessages,
    maxStoredAgentRuns: storedRuns,
    maxStoredToolTraces: storedTraces
  });

  const addTrustedToolPattern = async () => {
    const pattern = trustedToolPatternDraft.trim();
    if (!pattern) return;
    const next = await api.addTrustedToolPattern(pattern);
    setTrustedToolPatterns(next.chat.trustedToolPatterns ?? []);
    setTrustedToolPatternDraft("");
  };

  const removeTrustedToolPattern = async (pattern: string) => {
    const next = await api.removeTrustedToolPattern(pattern);
    setTrustedToolPatterns(next.chat.trustedToolPatterns ?? []);
  };

  const addTrustedCommandPattern = () => {
    const pattern = trustedCommandPatternDraft.trim();
    if (!pattern) return;
    setTrustedCommandPatterns((items) => Array.from(new Set([...items, pattern])));
    setTrustedCommandPatternDraft("");
  };

  const removeTrustedCommandPattern = (pattern: string) => {
    setTrustedCommandPatterns((items) => items.filter((item) => item !== pattern));
  };

  const approvalModeHint = {
    risky: "只拦截工具声明需要审批、非只读 HTTP、命令、写入、删除、提交、发送等高风险调用；可信规则会跳过审批。",
    smart: "高风险调用先由辅助 LLM 评估；明显安全时自动放行，明显危险时阻断，不确定时等待人工审批。",
    always: "所有外部工具调用都先暂停等待确认；可信规则仍可直接放行。",
    never: "默认直接执行外部工具调用；仍保留运行记录和工具轨迹，适合本机可信环境。"
  }[toolApprovalMode] ?? "未知审批模式。";

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Chat</span><strong>对话设置</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">消息队列</div>
        <div className="form-group">
          <div className="form-row">
            <label>忙碌时输入</label>
            <select value={busyInputMode} onChange={(event) => setBusyInputMode(event.target.value)}>
              <option value="queue">加入队列</option>
              <option value="steer">注入规划</option>
              <option value="interrupt">中止并重开</option>
            </select>
          </div>
          <div className="form-row">
            <label>自动标题</label>
            <input checked={autoTitleEnabled} onChange={(event) => setAutoTitleEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>队列等待时间（秒）</label>
            <div className="stepper">
              <button onClick={() => setWait(Math.max(0, wait - 1))} type="button">−</button>
              <span className="stepper-val">{wait}</span>
              <button onClick={() => setWait(wait + 1)} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>单轮最长运行（秒）</label>
            <div className="stepper">
              <button onClick={() => setRunTimeout(Math.max(0, runTimeout - 30))} type="button">−</button>
              <span className="stepper-val">{runTimeout}</span>
              <button onClick={() => setRunTimeout(runTimeout + 30)} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>重复请求拦截</label>
            <input checked={dedupEnabled} onChange={(event) => setDedupEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row" style={{ opacity: dedupEnabled ? 1 : 0.5 }}>
            <label>重复窗口（秒）</label>
            <div className="stepper">
              <button onClick={() => setDedupWindow(Math.max(5, dedupWindow - 5))} type="button">−</button>
              <span className="stepper-val">{dedupWindow}</span>
              <button onClick={() => setDedupWindow(dedupWindow + 5)} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>压缩失败冻结</label>
            <input checked={shortContextAbortOnSummaryFailure} onChange={(event) => setShortContextAbortOnSummaryFailure(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>摘要服务商</label>
            <select value={shortContextSummaryProviderId} onChange={(event) => setShortContextSummaryProviderId(event.target.value)}>
              <option value="">跟随当前模型</option>
              {llmProviders.map((provider) => (
                <option key={provider.id} value={provider.id}>{provider.name || provider.id}</option>
              ))}
            </select>
          </div>
          <div className="form-row">
            <label>摘要模型</label>
            <input
              value={shortContextSummaryModel}
              onChange={(event) => setShortContextSummaryModel(event.target.value)}
              placeholder="留空使用服务商默认模型"
            />
          </div>
        </div>
        <div className="form-hint">控制 agent 正在运行时新消息的默认处理方式；单轮最长运行设为 0 时不自动超时。开启压缩失败冻结后，摘要模型失败不会丢弃旧历史，会暂停 agent 直到 /compact 成功。摘要服务商/模型为空时跟随当前主模型，辅助摘要失败会自动回退主模型一次。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">意图分析</div>
        <div className="form-group">
          <div className="form-row">
            <label>分析方法</label>
            <select value={intentMode} onChange={(event) => setIntentMode(event.target.value)}>
              <option value="embedding">Embedding 分类器</option>
              <option value="llm">LLM 原生理解</option>
            </select>
          </div>
          <div className="form-row" style={{ opacity: intentMode === "embedding" ? 1 : 0.45, pointerEvents: intentMode === "embedding" ? "auto" : "none" }}>
            <label>Embedding 置信度</label>
            <div className="stepper">
              <button onClick={() => setIntentEmbeddingConfidence(Math.max(0, Number((intentEmbeddingConfidence - 0.05).toFixed(2))))} type="button">−</button>
              <span className="stepper-val">{intentEmbeddingConfidence.toFixed(2)}</span>
              <button onClick={() => setIntentEmbeddingConfidence(Math.min(1, Number((intentEmbeddingConfidence + 0.05).toFixed(2))))} type="button">+</button>
            </div>
          </div>
          <div style={{ opacity: intentMode === "llm" ? 1 : 0.45, pointerEvents: intentMode === "llm" ? "auto" : "none" }}>
            <div className="form-row">
              <label>绑定服务商</label>
              <select value={intentProviderId} onChange={(event) => setIntentProviderId(event.target.value)}>
                <option value="">跟随当前角色</option>
                {llmProviders.map((provider) => (
                  <option key={provider.id} value={provider.id}>{provider.name || provider.id}</option>
                ))}
              </select>
            </div>
            <div className="form-row">
              <label>绑定模型</label>
              <input value={intentModel} onChange={(event) => setIntentModel(event.target.value)} placeholder="留空使用服务商默认模型" />
            </div>
            <div className="form-row">
              <label>LLM 置信度</label>
              <div className="stepper">
                <button onClick={() => setIntentLlmConfidence(Math.max(0, Number((intentLlmConfidence - 0.05).toFixed(2))))} type="button">−</button>
                <span className="stepper-val">{intentLlmConfidence.toFixed(2)}</span>
                <button onClick={() => setIntentLlmConfidence(Math.min(1, Number((intentLlmConfidence + 0.05).toFixed(2))))} type="button">+</button>
              </div>
            </div>
            <div className="form-row">
              <label>LLM 超时（秒）</label>
              <div className="stepper">
                <button onClick={() => setIntentLlmTimeout(Math.max(3, intentLlmTimeout - 1))} type="button">−</button>
                <span className="stepper-val">{intentLlmTimeout}</span>
                <button onClick={() => setIntentLlmTimeout(Math.min(120, intentLlmTimeout + 1))} type="button">+</button>
              </div>
            </div>
            <div className="form-row">
              <label>LLM 最大 token</label>
              <div className="stepper">
                <button onClick={() => setIntentLlmMaxTokens(Math.max(64, intentLlmMaxTokens - 64))} type="button">−</button>
                <span className="stepper-val">{intentLlmMaxTokens}</span>
                <button onClick={() => setIntentLlmMaxTokens(Math.min(4096, intentLlmMaxTokens + 64))} type="button">+</button>
              </div>
            </div>
            <div className="form-row vertical">
              <label>LLM 意图提示词</label>
              <textarea value={intentLlmPrompt} onChange={(event) => setIntentLlmPrompt(event.target.value)} />
            </div>
          </div>
        </div>
        <div className="form-hint">生产路径只使用当前选择的方法；LLM 模式不可用时返回 unknown，不切回旧规则。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">工具路由</div>
        <div className="form-group">
          <div className="form-row">
            <label>统一 LLM 路由</label>
            <input checked={routerEnabled} onChange={(event) => setRouterEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>最低置信度</label>
            <div className="stepper">
              <button onClick={() => setRouterConfidence(Math.max(0, Number((routerConfidence - 0.05).toFixed(2))))} type="button">−</button>
              <span className="stepper-val">{routerConfidence.toFixed(2)}</span>
              <button onClick={() => setRouterConfidence(Math.min(1, Number((routerConfidence + 0.05).toFixed(2))))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>分类超时（秒）</label>
            <div className="stepper">
              <button onClick={() => setRouterTimeout(Math.max(3, routerTimeout - 1))} type="button">−</button>
              <span className="stepper-val">{routerTimeout}</span>
              <button onClick={() => setRouterTimeout(Math.min(120, routerTimeout + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>分类最大 token</label>
            <div className="stepper">
              <button onClick={() => setRouterMaxTokens(Math.max(64, routerMaxTokens - 64))} type="button">−</button>
              <span className="stepper-val">{routerMaxTokens}</span>
              <button onClick={() => setRouterMaxTokens(Math.min(4096, routerMaxTokens + 64))} type="button">+</button>
            </div>
          </div>
          <div className="form-row vertical">
            <label>分类提示词</label>
            <textarea value={routerPrompt} onChange={(event) => setRouterPrompt(event.target.value)} />
          </div>
          <div className="form-row">
            <label>工具协议修复</label>
            <select value={toolUseEnforcement} onChange={(event) => setToolUseEnforcement(event.target.value)}>
              <option value="auto">自动修复</option>
              <option value="off">关闭</option>
            </select>
          </div>
          <div className="form-row">
            <label>并行工具</label>
            <input checked={toolParallelEnabled} onChange={(event) => setToolParallelEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>send_message 工具</label>
            <input checked={sendMessageToolEnabled} onChange={(event) => setSendMessageToolEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-hint">关闭时 Agent 不会看到或调用 Hermes send_message；扫码微信通道不依赖它。</div>
          <div className="form-row">
            <label>并行上限</label>
            <div className="stepper">
              <button onClick={() => setToolParallelLimit(Math.max(1, toolParallelLimit - 1))} type="button">−</button>
              <span className="stepper-val">{toolParallelLimit}</span>
              <button onClick={() => setToolParallelLimit(Math.min(64, toolParallelLimit + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>工具审批模式</label>
            <select value={toolApprovalMode} onChange={(event) => setToolApprovalMode(event.target.value)}>
              <option value="risky">仅高风险</option>
              <option value="smart">智能审批</option>
              <option value="always">全部审批</option>
              <option value="never">默认允许（never）</option>
            </select>
          </div>
          <div className="form-hint">{approvalModeHint}</div>
          <div className="form-row">
            <label>凭据池策略</label>
            <select value={llmCredentialPoolStrategy} onChange={(event) => setLlmCredentialPoolStrategy(event.target.value)}>
              <option value="fill_first">优先第一个</option>
              <option value="round_robin">轮询</option>
              <option value="least_used">最少使用</option>
              <option value="random">随机</option>
            </select>
          </div>
          <div className="form-row vertical">
            <label>命令环境放行</label>
            <textarea
              rows={3}
              value={toolEnvPassthroughDraft}
              onChange={(event) => setToolEnvPassthroughDraft(event.target.value)}
              placeholder="每行一个变量名，例如 NOTION_TOKEN"
            />
            <div className="form-hint">terminal、process、execute_code 默认移除 API_KEY/TOKEN/SECRET 等敏感环境变量；这里列出的变量会保留。</div>
          </div>
          <div className="form-row vertical">
            <label>可信工具规则</label>
            <div className="inline-actions">
              <input
                value={trustedToolPatternDraft}
                onChange={(event) => setTrustedToolPatternDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void addTrustedToolPattern();
                  }
                }}
                placeholder="server.tool、server.* 或 *"
              />
              <button className="btn-primary" onClick={() => void addTrustedToolPattern()} type="button">
                添加
              </button>
            </div>
            <div className="form-hint">命中规则的工具调用会跳过审批；`*` 表示信任全部外部工具。</div>
            {trustedToolPatterns.length > 0 ? (
              <div className="adapter-list" style={{ marginTop: 8 }}>
                {trustedToolPatterns.map((pattern) => (
                  <div className="adapter-row trace-row" key={pattern}>
                    <span className="status-badge enabled">trusted</span>
                    <div className="adapter-info">
                      <strong>{pattern}</strong>
                      <small>匹配后续工具调用时直接执行</small>
                    </div>
                    <button className="btn-secondary" onClick={() => void removeTrustedToolPattern(pattern)} type="button">
                      移除
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
          <div className="form-row vertical">
            <label>可信命令规则</label>
            <div className="inline-actions">
              <input
                value={trustedCommandPatternDraft}
                onChange={(event) => setTrustedCommandPatternDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    addTrustedCommandPattern();
                  }
                }}
                placeholder='例如 npm run build 或 git status*'
              />
              <button className="btn-primary" onClick={addTrustedCommandPattern} type="button">
                添加
              </button>
            </div>
            <div className="form-hint">匹配 terminal、process start/run、execute_code 的命令文本时跳过审批；支持 `*` 通配。hardline 风险不会被绕过。</div>
            {trustedCommandPatterns.length > 0 ? (
              <div className="adapter-list" style={{ marginTop: 8 }}>
                {trustedCommandPatterns.map((pattern) => (
                  <div className="adapter-row trace-row" key={pattern}>
                    <span className="status-badge enabled">trusted</span>
                    <div className="adapter-info">
                      <strong>{pattern}</strong>
                      <small>匹配后续命令调用时直接执行</small>
                    </div>
                    <button className="btn-secondary" onClick={() => removeTrustedCommandPattern(pattern)} type="button">
                      移除
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
          <div className="form-row">
            <label>模型重试次数</label>
            <div className="stepper">
              <button onClick={() => setLlmRetryCount(Math.max(0, llmRetryCount - 1))} type="button">−</button>
              <span className="stepper-val">{llmRetryCount}</span>
              <button onClick={() => setLlmRetryCount(Math.min(5, llmRetryCount + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>模型退避（ms）</label>
            <div className="stepper">
              <button onClick={() => setLlmRetryBackoff(Math.max(100, llmRetryBackoff - 100))} type="button">−</button>
              <span className="stepper-val">{llmRetryBackoff}</span>
              <button onClick={() => setLlmRetryBackoff(Math.min(60000, llmRetryBackoff + 100))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>Responses reasoning 回放</label>
            <input checked={responsesReasoningReplayEnabled} onChange={(event) => setResponsesReasoningReplayEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-hint">关闭后不会向 Responses 请求附带历史 encrypted reasoning item，也不会请求新的 encrypted reasoning 内容。</div>
          <div className="form-row">
            <label>工具重试次数</label>
            <div className="stepper">
              <button onClick={() => setToolRetryCount(Math.max(0, toolRetryCount - 1))} type="button">−</button>
              <span className="stepper-val">{toolRetryCount}</span>
              <button onClick={() => setToolRetryCount(Math.min(5, toolRetryCount + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>重试退避（ms）</label>
            <div className="stepper">
              <button onClick={() => setToolRetryBackoff(Math.max(0, toolRetryBackoff - 100))} type="button">−</button>
              <span className="stepper-val">{toolRetryBackoff}</span>
              <button onClick={() => setToolRetryBackoff(Math.min(10000, toolRetryBackoff + 100))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>循环提醒</label>
            <input checked={guardWarningsEnabled} onChange={(event) => setGuardWarningsEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>循环硬停止</label>
            <input checked={guardHardStopEnabled} onChange={(event) => setGuardHardStopEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>同参失败提醒</label>
            <div className="stepper">
              <button onClick={() => setGuardExactWarnAfter(Math.max(1, guardExactWarnAfter - 1))} type="button">−</button>
              <span className="stepper-val">{guardExactWarnAfter}</span>
              <button onClick={() => setGuardExactWarnAfter(Math.min(12, guardExactWarnAfter + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>同工具失败提醒</label>
            <div className="stepper">
              <button onClick={() => setGuardSameToolWarnAfter(Math.max(1, guardSameToolWarnAfter - 1))} type="button">−</button>
              <span className="stepper-val">{guardSameToolWarnAfter}</span>
              <button onClick={() => setGuardSameToolWarnAfter(Math.min(16, guardSameToolWarnAfter + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>无进展提醒</label>
            <div className="stepper">
              <button onClick={() => setGuardNoProgressWarnAfter(Math.max(1, guardNoProgressWarnAfter - 1))} type="button">−</button>
              <span className="stepper-val">{guardNoProgressWarnAfter}</span>
              <button onClick={() => setGuardNoProgressWarnAfter(Math.min(12, guardNoProgressWarnAfter + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row" style={{ opacity: guardHardStopEnabled ? 1 : 0.5 }}>
            <label>同参失败停止</label>
            <div className="stepper">
              <button onClick={() => setGuardExactLimit(Math.max(1, guardExactLimit - 1))} type="button">−</button>
              <span className="stepper-val">{guardExactLimit}</span>
              <button onClick={() => setGuardExactLimit(Math.min(12, guardExactLimit + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row" style={{ opacity: guardHardStopEnabled ? 1 : 0.5 }}>
            <label>同工具失败停止</label>
            <div className="stepper">
              <button onClick={() => setGuardSameToolLimit(Math.max(1, guardSameToolLimit - 1))} type="button">−</button>
              <span className="stepper-val">{guardSameToolLimit}</span>
              <button onClick={() => setGuardSameToolLimit(Math.min(16, guardSameToolLimit + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row" style={{ opacity: guardHardStopEnabled ? 1 : 0.5 }}>
            <label>无进展停止</label>
            <div className="stepper">
              <button onClick={() => setGuardNoProgressLimit(Math.max(1, guardNoProgressLimit - 1))} type="button">−</button>
              <span className="stepper-val">{guardNoProgressLimit}</span>
              <button onClick={() => setGuardNoProgressLimit(Math.min(12, guardNoProgressLimit + 1))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">统一 LLM 路由已完全替换三级漏斗；模型与工具重试只处理超时、连接中断、429/5xx 等瞬时失败，参数或路径错误会交给失败重规划。循环保护默认只向 planner 注入提醒；开启硬停止后才会自动终止重复工具路径。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">后台复盘</div>
        <div className="form-group">
          <div className="form-row">
            <label>技能建议复盘</label>
            <input checked={backgroundSkillReviewEnabled} onChange={(event) => setBackgroundSkillReviewEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>自动创建技能</label>
            <input checked={backgroundSkillReviewAutoCreateEnabled} onChange={(event) => setBackgroundSkillReviewAutoCreateEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>自动整理报告</label>
            <input checked={backgroundSkillCuratorEnabled} onChange={(event) => setBackgroundSkillCuratorEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>整理间隔（小时）</label>
            <div className="stepper">
              <button onClick={() => setBackgroundSkillCuratorIntervalHours(Math.max(1, backgroundSkillCuratorIntervalHours - 24))} type="button">−</button>
              <span className="stepper-val">{backgroundSkillCuratorIntervalHours}</span>
              <button onClick={() => setBackgroundSkillCuratorIntervalHours(Math.min(2160, backgroundSkillCuratorIntervalHours + 24))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">完成一次 agent run 后异步检查技能库改进建议；自动整理报告按间隔生成 curator dry-run 报告，不会自动归档。记忆复盘请在通讯录的记忆管理中配置。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">界面性能</div>
        <div className="form-group">
          <div className="form-row">
            <label>页面加载消息数</label>
            <div className="stepper">
              <button onClick={() => setUiLimit(Math.max(40, uiLimit - 20))} type="button">−</button>
              <span className="stepper-val">{uiLimit}</span>
              <button onClick={() => setUiLimit(Math.min(1000, uiLimit + 20))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>附件扫描消息数</label>
            <div className="stepper">
              <button onClick={() => setArtifactLimit(Math.max(20, artifactLimit - 10))} type="button">−</button>
              <span className="stepper-val">{artifactLimit}</span>
              <button onClick={() => setArtifactLimit(Math.min(uiLimit, artifactLimit + 10))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>单条消息预览字符</label>
            <div className="stepper">
              <button onClick={() => setPreviewChars(Math.max(2000, previewChars - 1000))} type="button">−</button>
              <span className="stepper-val">{previewChars}</span>
              <button onClick={() => setPreviewChars(Math.min(100000, previewChars + 1000))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>回复显现速度（字/秒）</label>
            <div className="stepper">
              <button onClick={() => setStreamChars(Math.max(8, streamChars - 4))} type="button">−</button>
              <span className="stepper-val">{streamChars}</span>
              <button onClick={() => setStreamChars(Math.min(160, streamChars + 4))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>Thinking 最短展示（ms）</label>
            <div className="stepper">
              <button onClick={() => setThinkingMs(Math.max(0, thinkingMs - 200))} type="button">−</button>
              <span className="stepper-val">{thinkingMs}</span>
              <button onClick={() => setThinkingMs(Math.min(8000, thinkingMs + 200))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>底部跟随容忍距离</label>
            <div className="stepper">
              <button onClick={() => setBottomThreshold(Math.max(24, bottomThreshold - 24))} type="button">−</button>
              <span className="stepper-val">{bottomThreshold}px</span>
              <button onClick={() => setBottomThreshold(Math.min(600, bottomThreshold + 24))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>桌宠气泡时长（秒）</label>
            <div className="stepper">
              <button onClick={() => setPetCloudDurationSeconds(Math.max(1, petCloudDurationSeconds - 1))} type="button">−</button>
              <span className="stepper-val">{petCloudDurationSeconds}</span>
              <button onClick={() => setPetCloudDurationSeconds(Math.min(120, petCloudDurationSeconds + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>活跃刷新间隔（ms）</label>
            <div className="stepper">
              <button onClick={() => setActivePollMs(Math.max(300, activePollMs - 100))} type="button">−</button>
              <span className="stepper-val">{activePollMs}</span>
              <button onClick={() => setActivePollMs(Math.min(30000, activePollMs + 100))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>空闲刷新间隔（ms）</label>
            <div className="stepper">
              <button onClick={() => setIdlePollMs(Math.max(1000, idlePollMs - 500))} type="button">−</button>
              <span className="stepper-val">{idlePollMs}</span>
              <button onClick={() => setIdlePollMs(Math.min(120000, idlePollMs + 500))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>Skill 热更新</label>
            <input checked={skillHotReloadEnabled} onChange={(event) => setSkillHotReloadEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>Skill 扫描间隔（秒）</label>
            <div className="stepper">
              <button onClick={() => setSkillHotReloadInterval(Math.max(1, skillHotReloadInterval - 1))} type="button">−</button>
              <span className="stepper-val">{skillHotReloadInterval}</span>
              <button onClick={() => setSkillHotReloadInterval(Math.min(3600, skillHotReloadInterval + 1))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">限制一次加载数量，并控制对话页的显现节奏、Thinking 停留、底部跟随、事件刷新和 Skill 热更新。</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">历史资源清理</div>
        <div className="form-group">
          <div className="form-row">
            <label>自动清理</label>
            <input checked={cleanupEnabled} onChange={(event) => setCleanupEnabled(event.target.checked)} type="checkbox" />
          </div>
          <div className="form-row">
            <label>保留天数</label>
            <div className="stepper">
              <button onClick={() => setRetentionDays(Math.max(1, retentionDays - 1))} type="button">−</button>
              <span className="stepper-val">{retentionDays}</span>
              <button onClick={() => setRetentionDays(Math.min(3650, retentionDays + 1))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>每会话最多消息</label>
            <div className="stepper">
              <button onClick={() => setStoredMessages(Math.max(50, storedMessages - 50))} type="button">−</button>
              <span className="stepper-val">{storedMessages}</span>
              <button onClick={() => setStoredMessages(Math.min(20000, storedMessages + 50))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>最多运行记录</label>
            <div className="stepper">
              <button onClick={() => setStoredRuns(Math.max(5, storedRuns - 5))} type="button">−</button>
              <span className="stepper-val">{storedRuns}</span>
              <button onClick={() => setStoredRuns(Math.min(5000, storedRuns + 5))} type="button">+</button>
            </div>
          </div>
          <div className="form-row">
            <label>最多工具 traces</label>
            <div className="stepper">
              <button onClick={() => setStoredTraces(Math.max(5, storedTraces - 10))} type="button">−</button>
              <span className="stepper-val">{storedTraces}</span>
              <button onClick={() => setStoredTraces(Math.min(10000, storedTraces + 10))} type="button">+</button>
            </div>
          </div>
        </div>
        <div className="form-hint">启动后自动清理过期会话、运行记录、工具 traces、state/workspace 快照，并裁剪超出上限的历史消息</div>
      </div>
    </div>
  );
}

function ReplySettings({
  onBack,
  config,
  onSave
}: {
  onBack?: () => void;
  config: {
    typingDelayEnabled?: boolean;
    typingSpeed: number;
    typingSpeedRandomMin: number;
    typingSpeedRandomMax: number;
    splitByNewline: boolean;
    showTypingIndicator: boolean;
    typingIndicatorRefreshSeconds?: number;
  };
  onSave: (patch: Partial<typeof config>) => Promise<void>;
}) {
  const [splitByNewline, setSplitByNewline] = useState(config.splitByNewline);
  const [delayEnabled, setDelayEnabled] = useState(config.typingDelayEnabled !== false);
  const [typingSpeed, setTypingSpeed] = useState(config.typingSpeed);
  const [randomMin, setRandomMin] = useState(config.typingSpeedRandomMin);
  const [randomMax, setRandomMax] = useState(config.typingSpeedRandomMax);
  const [showTyping, setShowTyping] = useState(config.showTypingIndicator);
  const [typingRefreshSeconds, setTypingRefreshSeconds] = useState(config.typingIndicatorRefreshSeconds ?? 2);

  useEffect(() => {
    setSplitByNewline(config.splitByNewline);
    setDelayEnabled(config.typingDelayEnabled !== false);
    setTypingSpeed(config.typingSpeed);
    setRandomMin(config.typingSpeedRandomMin);
    setRandomMax(config.typingSpeedRandomMax);
    setShowTyping(config.showTypingIndicator);
    setTypingRefreshSeconds(config.typingIndicatorRefreshSeconds ?? 2);
  }, [config]);

  const save = () => void onSave({
    splitByNewline,
    typingDelayEnabled: delayEnabled,
    typingSpeed,
    typingSpeedRandomMin: randomMin,
    typingSpeedRandomMax: randomMax,
    showTypingIndicator: showTyping,
    typingIndicatorRefreshSeconds: typingRefreshSeconds
  });

  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Reply</span><strong>回复设置</strong></div>
        <button className="btn-primary" onClick={save} type="button">保存</button>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">回复拆分</div>
        <div className="form-group">
          <div className="form-row">
            <label>按换行拆分</label>
            <label className="switch-wrap">
              <input
                type="checkbox"
                checked={splitByNewline}
                onChange={(event) => setSplitByNewline(event.target.checked)}
              />
              <span className="switch-track" />
            </label>
          </div>
        </div>
        <div className="form-hint">将 AI 回复按换行符拆分为多条消息存储和发送</div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">打字模拟</div>
        <div className="form-group">
          <div className="form-row">
            <label>消息间延迟</label>
            <label className="switch-wrap">
              <input
                type="checkbox"
                checked={delayEnabled}
                onChange={(event) => setDelayEnabled(event.target.checked)}
              />
              <span className="switch-track" />
            </label>
          </div>
        </div>
        <div className="form-hint">开启后，相邻消息之间按打字速度模拟延迟</div>
        <div style={{ opacity: delayEnabled ? 1 : 0.45, pointerEvents: delayEnabled ? "auto" : "none" }}>
          <div className="form-group">
            <div className="form-row">
              <label>打字速度</label>
              <div className="slider-wrap">
                <input
                  type="range"
                  min={0.05}
                  max={1}
                  step={0.05}
                  value={typingSpeed}
                  onChange={(event) => setTypingSpeed(Number(event.target.value))}
                />
                <span className="slider-val">{typingSpeed.toFixed(2)}</span>
              </div>
            </div>
          </div>
          <div className="form-group">
            <div className="form-row">
              <label>随机下限</label>
              <div className="slider-wrap">
                <input
                  type="range"
                  min={0.01}
                  max={0.5}
                  step={0.01}
                  value={randomMin}
                  onChange={(event) => setRandomMin(Number(event.target.value))}
                />
                <span className="slider-val">{randomMin.toFixed(2)}</span>
              </div>
            </div>
          </div>
          <div className="form-group">
            <div className="form-row">
              <label>随机上限</label>
              <div className="slider-wrap">
                <input
                  type="range"
                  min={0.01}
                  max={0.5}
                  step={0.01}
                  value={randomMax}
                  onChange={(event) => setRandomMax(Number(event.target.value))}
                />
                <span className="slider-val">{randomMax.toFixed(2)}</span>
              </div>
            </div>
          </div>
          <div className="form-hint">相邻消息之间延迟时间 = 字数 × (打字速度 + 随机值)，结果限制在 0.5~8 秒</div>
        </div>
        <div className="form-group">
          <div className="form-row">
            <label>输入指示器</label>
            <label className="switch-wrap">
              <input
                type="checkbox"
                checked={showTyping}
                onChange={(event) => setShowTyping(event.target.checked)}
              />
              <span className="switch-track" />
            </label>
          </div>
        </div>
        <div style={{ opacity: showTyping ? 1 : 0.45, pointerEvents: showTyping ? "auto" : "none" }}>
          <div className="form-group">
            <div className="form-row">
              <label>续期间隔</label>
              <div className="slider-wrap">
                <input
                  type="range"
                  min={1}
                  max={10}
                  step={1}
                  value={typingRefreshSeconds}
                  onChange={(event) => setTypingRefreshSeconds(Number(event.target.value))}
                />
                <span className="slider-val">{typingRefreshSeconds}s</span>
              </div>
            </div>
          </div>
        </div>
        <div className="form-hint">模型思考和回复时显示"对方正在输入"，并按续期间隔刷新，直到桌面端结束“正在思考”。</div>
      </div>
    </div>
  );
}

function ThemeSettings({
  onBack,
  themes,
  importThemeCss,
  saveThemes
}: {
  onBack?: () => void;
  themes: ThemeConfig[];
  importThemeCss: (file: File) => Promise<void>;
  saveThemes: (themes: ThemeConfig[]) => Promise<void>;
}) {
  const [mode, setMode] = useState<"light" | "dark" | "auto">(themes[0]?.mode ?? "light");
  const [exportPath, setExportPath] = useState("");
  const themeInput = useRef<HTMLInputElement | null>(null);
  const add = () => {
    const now = new Date().toISOString();
    void saveThemes([...themes, { id: crypto.randomUUID(), name: "新主题", mode, active: false, css: "", createdAt: now, updatedAt: now }]);
  };
  const onThemeFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (file) await importThemeCss(file);
  };
  const exportCss = async () => {
    setExportPath(await api.exportThemesCss(themes.filter((theme) => theme.active).map((theme) => theme.id)));
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title"><BackBtn onBack={onBack} /><div className="panel-title-text"><span>Theme</span><strong>主题</strong></div><button onClick={add} type="button"><Plus size={15} />新建</button></div>
      <input accept=".css,text/css" className="hidden-input" onChange={onThemeFile} ref={themeInput} type="file" />
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">外观模式</div>
        {(["light", "dark", "auto"] as const).map((item) => (
          <button className="card-row clickable-row theme-mode-row theme-mode-active" key={item} onClick={() => {
            setMode(item);
            void saveThemes(themes.map((t) => ({ ...t, mode: item })));
          }} type="button">
            <span className={`row-icon ${mode === item ? "primary" : "cyan"}`}><Palette size={18} /></span>
            <span className="row-label">{item === "light" ? "浅色" : item === "dark" ? "深色" : "跟随系统"}</span>
            {mode === item ? <span className="check-mark">✓</span> : null}
          </button>
        ))}
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">主题操作</div>
        <div className="form-actions-horizontal">
          <button className="btn-secondary-outline" onClick={() => themeInput.current?.click()} type="button"><Upload size={15} />导入 CSS</button>
          <button className="btn-secondary-outline" onClick={() => void exportCss()} type="button">导出当前主题</button>
        </div>
      </div>
      {exportPath ? <p className="form-hint panel-hint">已导出：{exportPath}</p> : null}
      <div className="theme-list">
        {themes.map((theme, index) => (
          <div className="card theme-card" key={`${theme.name}-${index}`}>
            <div className="theme-header">
              <div className="theme-info">
                <strong>{theme.name}</strong>
                <span className="theme-meta">{theme.active ? "正在应用" : "可用主题"} · {theme.css ? "自定义 CSS" : "默认样式"}</span>
              </div>
              <div className="theme-actions">
                <button className={theme.active ? "btn-secondary-outline" : "btn-primary-outline"} onClick={() => void saveThemes(themes.map((item, i) => (i === index ? { ...item, active: !item.active, mode } : item)))} type="button">{theme.active ? "移出" : "应用"}</button>
                <button className="btn-danger-outline-sm" onClick={() => void saveThemes(themes.filter((_, i) => i !== index))} type="button">删除</button>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

function EmojiSettings({
  onBack,
  groups,
  saveGroups,
  uploadImage
}: {
  onBack?: () => void;
  groups: EmojiGroup[];
  saveGroups: (groups: EmojiGroup[]) => Promise<void>;
  uploadImage: (groupId: string, emotion: string, file: File) => Promise<void>;
}) {
  const fileInput = useRef<HTMLInputElement | null>(null);
  const [uploadGroupId, setUploadGroupId] = useState("");
  const [uploadEmotion, setUploadEmotion] = useState("");
  const addGroup = async () => {
    const name = window.prompt("分组名称", "新分组")?.trim();
    if (!name) return;
    const next = await api.createEmojiGroup(name);
    await saveGroups(next);
  };
  const addEmotion = async (groupId: string) => {
    const emotion = window.prompt("情绪分类名称", "happy")?.trim();
    if (!emotion) return;
    const next = await api.createEmojiEmotion(groupId, emotion);
    await saveGroups(next);
  };
  const renameGroup = async (group: EmojiGroup) => {
    const newName = window.prompt("新的分组名称", group.name)?.trim();
    if (!newName || newName === group.id) return;
    const next = await api.renameEmojiGroup(group.id, newName);
    await saveGroups(next);
  };
  const renameEmotion = async (groupId: string, emotion: string) => {
    const newName = window.prompt("新的情绪分类名称", emotion)?.trim();
    if (!newName || newName === emotion) return;
    const next = await api.renameEmojiEmotion(groupId, emotion, newName);
    await saveGroups(next);
  };
  const deleteGroup = async (groupId: string) => {
    if (!window.confirm("删除该表情包分组？")) return;
    const next = await api.deleteEmojiGroup(groupId);
    await saveGroups(next);
  };
  const deleteEmotion = async (groupId: string, emotion: string) => {
    if (!window.confirm("删除该情绪分类及其中图片？")) return;
    const next = await api.deleteEmojiEmotion(groupId, emotion);
    await saveGroups(next);
  };
  const deleteImage = async (groupId: string, emotion: string, path: string) => {
    const fileName = path.split(/[\\/]/).pop() || "";
    if (!fileName || !window.confirm(`删除图片 ${fileName}？`)) return;
    const next = await api.deleteEmojiImage(groupId, emotion, fileName);
    await saveGroups(next);
  };
  const renameImage = async (groupId: string, emotion: string, path: string) => {
    const fileName = path.split(/[\\/]/).pop() || "";
    const newName = window.prompt("新的图片文件名", fileName)?.trim();
    if (!fileName || !newName || newName === fileName) return;
    const next = await api.renameEmojiImage(groupId, emotion, fileName, newName);
    await saveGroups(next);
  };
  const onFile = async (event: ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    if (uploadGroupId && uploadEmotion) {
      for (const file of files) {
        await uploadImage(uploadGroupId, uploadEmotion, file);
      }
    }
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title"><BackBtn onBack={onBack} /><div className="panel-title-text"><span>Emoji</span><strong>表情包管理</strong></div><button className="btn-primary" onClick={addGroup} type="button"><Plus size={15} />新建分组</button></div>
      <input accept="image/*" className="hidden-input" multiple onChange={onFile} ref={fileInput} type="file" />
      {groups.length === 0 ? (
        <div className="empty-state compact">
          <div className="empty-icon-wrap"><Smile size={48} strokeWidth={1.5} /></div>
          <p>没有表情包分组</p>
          <button className="btn-primary" onClick={addGroup} type="button">新建分组</button>
        </div>
      ) : (
        <div className="emoji-list">
          {groups.map((group) => (
            <div className="card emoji-card" key={group.id}>
              <div className="emoji-header">
                <div className="emoji-info">
                  <strong>{group.name}</strong>
                  <span className="emoji-meta">{group.emotions.length} 个情绪分类 · {group.images.length} 张图片</span>
                </div>
                <div className="emoji-actions">
                  <button className="btn-secondary-outline" onClick={() => void addEmotion(group.id)} type="button">新建情绪</button>
                  <button className="btn-secondary-outline" onClick={() => void renameGroup(group)} type="button">重命名</button>
                  <button className="btn-danger-outline-sm" onClick={() => void deleteGroup(group.id)} type="button">删除</button>
                </div>
              </div>
              <div className="emoji-emotion-list">
                {group.emotions.map((emotion) => {
                  const images = group.emotionImages?.[emotion] ?? [];
                  return (
                    <div className="emoji-emotion" key={emotion}>
                      <div className="emoji-emotion-head">
                        <strong>{emotion}</strong>
                        <span>{images.length} 张</span>
                        <button className="btn-secondary-outline-sm" onClick={() => { setUploadGroupId(group.id); setUploadEmotion(emotion); fileInput.current?.click(); }} type="button">上传</button>
                        <button className="btn-secondary-outline-sm" onClick={() => void renameEmotion(group.id, emotion)} type="button">重命名</button>
                        <button className="btn-danger-outline-sm" onClick={() => void deleteEmotion(group.id, emotion)} type="button">删除</button>
                      </div>
                      <div className="emoji-image-grid">
                        {images.map((path) => (
                          <div className="emoji-image-item" key={path}>
                            <img src={api.assetUrl(path)} alt={path.split(/[\\/]/).pop() || emotion} />
                            <div>
                              <button className="btn-secondary-outline-sm" onClick={() => void renameImage(group.id, emotion, path)} type="button">改名</button>
                              <button className="btn-danger-outline-sm" onClick={() => void deleteImage(group.id, emotion, path)} type="button">删除</button>
                            </div>
                          </div>
                        ))}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function NetworkSettings({
  onBack,
  config,
  weather,
  onSave,
  onSaveWeather
}: {
  onBack?: () => void;
  config: { port: number; password: string; publicEnabled: boolean; publicPort: number; publicSecret: string };
  weather: { qweatherApiKey: string; qweatherApiHost: string; defaultLocation: string; timeoutSeconds: number };
  onSave: (patch: Partial<typeof config>) => Promise<void>;
  onSaveWeather: (patch: Partial<typeof weather>) => Promise<void>;
}) {
  const [draft, setDraft] = useState(config);
  const [weatherDraft, setWeatherDraft] = useState(weather);
  useEffect(() => setDraft(config), [config]);
  useEffect(() => setWeatherDraft(weather), [weather]);
  const save = () => {
    void onSave(draft);
    void onSaveWeather(weatherDraft);
  };
  const regenerate = () => {
    const publicPort = 30000 + Math.floor(Math.random() * 30000);
    const publicSecret = crypto.randomUUID().replace(/-/g, "").slice(0, 16);
    setDraft((d) => ({ ...d, publicPort, publicSecret }));
    void onSave({ publicPort, publicSecret });
  };
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title"><BackBtn onBack={onBack} /><div className="panel-title-text"><span>Network</span><strong>网络设置</strong></div><button className="btn-primary" onClick={save} type="button">保存</button></div>
      <div className="settings-form">
        <label>本地端口<input min={1} type="number" value={draft.port} onChange={(event) => setDraft((d) => ({ ...d, port: Number(event.target.value) }))} /></label>
        <label>公网访问密码<input type="password" value={draft.password} onChange={(event) => setDraft((d) => ({ ...d, password: event.target.value }))} placeholder="8位以上，含大小写字母和数字" /></label>
        <label className="checkbox-row"><input checked={draft.publicEnabled} onChange={(event) => setDraft((d) => ({ ...d, publicEnabled: event.target.checked }))} type="checkbox" />对公网开放（实验性）</label>
        <div className="two-column">
          <label>公网端口<input min={1} type="number" value={draft.publicPort} onChange={(event) => setDraft((d) => ({ ...d, publicPort: Number(event.target.value) }))} /></label>
          <label>随机路径<input value={draft.publicSecret} onChange={(event) => setDraft((d) => ({ ...d, publicSecret: event.target.value }))} /></label>
        </div>
        <button onClick={regenerate} type="button"><RefreshCw size={15} />重新生成端口和路径</button>
        <p className="form-hint">原版提示公网访问存在风险；请只在充分理解网络暴露风险时开启。</p>
        <div className="settings-divider" />
        <div className="panel-title-text">
          <span>Weather</span>
          <strong>天气服务</strong>
        </div>
        <label>
          和风天气 API Key
          <input
            type="password"
            value={weatherDraft.qweatherApiKey}
            onChange={(event) => setWeatherDraft((d) => ({ ...d, qweatherApiKey: event.target.value }))}
            placeholder="QWeather API Key"
          />
        </label>
        <label>
          和风天气 API Host
          <input
            value={weatherDraft.qweatherApiHost}
            onChange={(event) => setWeatherDraft((d) => ({ ...d, qweatherApiHost: event.target.value }))}
            placeholder="https://devapi.qweather.com"
          />
        </label>
        <div className="two-column">
          <label>
            默认城市
            <input
              value={weatherDraft.defaultLocation}
              onChange={(event) => setWeatherDraft((d) => ({ ...d, defaultLocation: event.target.value }))}
              placeholder="上海"
            />
          </label>
          <label>
            超时秒数
            <input
              min={3}
              max={30}
              type="number"
              value={weatherDraft.timeoutSeconds}
              onChange={(event) => setWeatherDraft((d) => ({ ...d, timeoutSeconds: Number(event.target.value) }))}
            />
          </label>
        </div>
        <p className="form-hint">用于内置天气查询工具；未填写 Key 时会明确提示配置，不会伪造天气。</p>
      </div>
    </div>
  );
}

function AboutSettings({ onBack, setView }: { onBack?: () => void; setView: (view: SettingsView) => void }) {
  const [appVersion, setAppVersion] = useState("V1.1.0");
  const [buildInfo, setBuildInfo] = useState<AppBuildInfo | null>(null);
  const [manifestUrl, setManifestUrl] = useState(readUpdateManifestUrl);
  const [updateStatus, setUpdateStatus] = useState("未检查");
  const [updateDetail, setUpdateDetail] = useState("");
  const [checking, setChecking] = useState(false);
  const [installingUpdate, setInstallingUpdate] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<AppUpdateCheck | null>(null);
  const autoCheckedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    void api.getAppBuildInfo().then((info: AppBuildInfo) => {
      if (cancelled) return;
      setBuildInfo(info);
      setAppVersion(`V${info.version}`);
      if (!readUpdateManifestUrl() && info.updateManifestUrl) {
        setManifestUrl(info.updateManifestUrl);
      }
    }).catch(() => {
      void getVersion().then((version) => {
        if (!cancelled) setAppVersion(`V${version}`);
      }).catch(() => {
        if (!cancelled) setAppVersion("V1.1.0");
      });
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const checkUpdates = useCallback(async (urlOverride?: string) => {
    const url = (urlOverride ?? manifestUrl).trim();
    if (!url) {
      setUpdateStatus("未配置更新源");
      setUpdateDetail("请填写可访问的版本清单地址，或在构建时注入 SYNTHCHAT_UPDATE_MANIFEST_URL。");
      setAvailableUpdate(null);
      return;
    }
    setChecking(true);
    setUpdateStatus("正在检查更新...");
    setUpdateDetail("");
    try {
      const result = await api.checkAppUpdate(url) as AppUpdateCheck;
      const normalizedUrl = result.sourceUrl?.trim() || url;
      writeUpdateManifestUrl(normalizedUrl);
      if (normalizedUrl !== manifestUrl) setManifestUrl(normalizedUrl);
      if (result.updateAvailable) {
        setAvailableUpdate(result);
        setUpdateStatus(`发现新版本 ${result.latestVersion}`);
        const detail = result.notes?.trim()
          || (result.publishedAt ? `发布时间 ${formatTime(result.publishedAt)}` : "可点击下方按钮打开下载页。");
        setUpdateDetail(detail);
      } else {
        setAvailableUpdate(null);
        setUpdateStatus("已经是最新版本");
        const checked = result.checkedAt ? `，检查时间 ${formatTime(result.checkedAt)}` : "";
        setUpdateDetail(`当前 ${result.currentVersion}，远端 ${result.latestVersion}${checked}`);
      }
    } catch (error) {
      const message = String(error).replace(/^bad request:\s*/i, "");
      setAvailableUpdate(null);
      if (message.includes("not configured")) {
        setUpdateStatus("未配置更新源");
        setUpdateDetail("请填写可访问的版本清单地址，或在构建时注入 SYNTHCHAT_UPDATE_MANIFEST_URL。");
      } else {
        setUpdateStatus("检查失败");
        setUpdateDetail(message);
      }
    } finally {
      setChecking(false);
    }
  }, [manifestUrl]);

  useEffect(() => {
    if (autoCheckedRef.current) return;
    const url = (manifestUrl || buildInfo?.updateManifestUrl || "").trim();
    if (!url) return;
    autoCheckedRef.current = true;
    void checkUpdates(url);
  }, [buildInfo?.updateManifestUrl, checkUpdates, manifestUrl]);

  const saveManifestUrl = () => {
    writeUpdateManifestUrl(manifestUrl);
    setUpdateStatus("更新源已保存");
    setUpdateDetail("之后进入关于页会自动检查该地址。");
  };

  const openUpdateUrl = async () => {
    const target = availableUpdate?.downloadUrl || availableUpdate?.releaseUrl || manifestUrl.trim();
    if (!target) return;
    try {
      await api.openAppUpdateUrl(target);
    } catch {
      window.open(target, "_blank", "noopener,noreferrer");
    }
  };

  const installUpdateSilently = async () => {
    const target = availableUpdate?.downloadUrl;
    if (!isSilentInstallAssetUrl(target)) {
      setUpdateStatus("无法自动安装");
      setUpdateDetail("当前更新源没有可静默安装的 .exe、.msi 或 .msix 资产，请打开下载页手动安装。");
      return;
    }
    const confirmed = window.confirm("将下载新版本安装包，随后关闭 SynthChat 并静默安装。是否继续？");
    if (!confirmed) return;
    setInstallingUpdate(true);
    setUpdateStatus("正在下载更新安装包...");
    setUpdateDetail("下载完成后应用会自动关闭，并在后台执行安装器。");
    try {
      await api.installAppUpdate(target);
      setUpdateDetail("安装器已启动，SynthChat 即将关闭。");
    } catch (error) {
      setInstallingUpdate(false);
      setUpdateStatus("自动安装失败");
      setUpdateDetail(String(error).replace(/^bad request:\s*/i, ""));
    }
  };

  return (
    <div className="primary-panel embedded-panel about-panel">
      <div className="panel-title action-title" style={{ width: "100%", marginBottom: 4 }}>
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>About</span><strong>关于 SynthChat</strong></div>
      </div>
      <div className="about-hero">
        <div className="brand-mark about-logo"><Sparkles size={32} /></div>
        <h2>SynthChat</h2>
        <p className="about-version">{appVersion}</p>
        <p className="about-subtitle">智能 AI 聊天机器人</p>
      </div>

      <div className="about-section">
        <div className="about-section-title">
          <RefreshCw size={14} />
          <span>应用更新</span>
        </div>
        <div className="menu-card flat-card about-card">
          <div className="settings-form" style={{ padding: "12px 14px" }}>
            <label style={{ display: "grid", gap: 4 }}>
              <span style={{ fontSize: "0.75rem", color: "var(--text-3)", fontWeight: 500 }}>更新源地址</span>
              <input value={manifestUrl} onChange={(event) => setManifestUrl(event.target.value)} placeholder="GitHub Releases API 或 update.json 地址" style={{ fontSize: 13 }} />
            </label>
            <div className="form-actions" style={{ marginTop: 8 }}>
              <button className="btn-secondary" onClick={saveManifestUrl} type="button">保存更新源</button>
              <button className="btn-primary" onClick={() => void checkUpdates()} disabled={checking} type="button">
                {checking ? "检查中..." : "检查更新"}
              </button>
            </div>
            {(updateStatus && updateStatus !== "未检查") && (
              <div className={`about-update-status ${availableUpdate ? "has-update" : updateStatus === "检查失败" ? "has-error" : "is-latest"}`}>
                <span className="about-update-status-text">{updateStatus}</span>
                {updateDetail && <span className="about-update-detail">{updateDetail}</span>}
              </div>
            )}
            {availableUpdate ? (
              <div className="form-actions" style={{ marginTop: 8 }}>
                <button className="btn-primary" onClick={() => void openUpdateUrl()} type="button" style={{ width: "100%" }}>
                  下载新版本 {availableUpdate.latestVersion}
                </button>
                {isSilentInstallAssetUrl(availableUpdate.downloadUrl) ? (
                  <button className="btn-secondary" onClick={() => void installUpdateSilently()} disabled={installingUpdate} type="button" style={{ width: "100%" }}>
                    {installingUpdate ? "正在准备安装..." : "下载并静默安装"}
                  </button>
                ) : null}
              </div>
            ) : null}
          </div>
        </div>
      </div>

      <div className="about-section">
        <div className="about-section-title">
          <Info size={14} />
          <span>更多信息</span>
        </div>
        <div className="menu-card flat-card about-card">
          {buildInfo ? (
            <div className="form-hint" style={{ padding: "10px 14px" }}>
              构建目标 {buildInfo.target} · 应用 ID {buildInfo.identifier}
            </div>
          ) : null}
          <MenuRow icon={Info} label="隐私说明及设置" onClick={() => setView("privacy")} iconColor="neutral" />
          <MenuRow icon={Info} label="软件声明" onClick={() => setView("statement")} iconColor="neutral" />
        </div>
      </div>

      <p className="about-footer">Made with love</p>
    </div>
  );
}

function InfoDocument({ onBack, title, body }: { onBack?: () => void; title: string; body: string[] }) {
  return (
    <div className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <BackBtn onBack={onBack} />
        <div className="panel-title-text"><span>Info</span><strong>{title}</strong></div>
      </div>
      <div className="doc-body">
        {body.map((paragraph) => <p key={paragraph}>{paragraph}</p>)}
      </div>
    </div>
  );
}
