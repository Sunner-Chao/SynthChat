import { ChangeEvent, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
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
  RefreshCw,
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
import { listen } from "@tauri-apps/api/event";
import { Avatar, MenuRow } from "./components/common";
import { SettingsPanel } from "./panels/SettingsPanel";
import { AgentsPanel, MemoryPanel, SkillsPanel, WorldbooksPanel, PluginsPanel, McpPanel } from "./panels/ToolPanels";
import { MomentsPanel } from "./panels/MomentsPanel";
import { PersonaPanel } from "./panels/PersonaPanel";
import { ChatExperience } from "./panels/ChatExperience";
import { EnvironmentCheck } from "./panels/EnvironmentCheck";
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
  return !(message.role === "user" && message.source === "proactive-internal");
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
  const setConversationProcessing = useAppStore((state) => state.setConversationProcessing);
  const handleAgentRunEvent = useAppStore((state) => state.handleAgentRunEvent);
  const handleManagedProcessEvent = useAppStore((state) => state.handleManagedProcessEvent);
  const upsertIncomingMessage = useAppStore((state) => state.upsertIncomingMessage);
  const refreshPersonasFromBackend = useCallback(async () => {
    const personas = await api.listPersonas();
    useAppStore.setState({ personas });
  }, []);
  const processingConversationCount = useAppStore((state) => state.processingConversationIds.length);
  const themes = useAppStore((state) => state.themes);
  const lastCountedMessageRef = useRef<Map<string, string>>(new Map());

  useEffect(() => {
    void bootstrap();
  }, [bootstrap]);

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
      isLast?: boolean;
    }>("synthchat-chat-event", (event) => {
      const payload = event.payload;
      if (payload.type === "processing" && payload.conversationId) {
        setConversationProcessing(payload.conversationId, true);
        if (payload.source === "wechat") {
          void (async () => {
            await refreshChatData(payload.conversationId ?? null, payload.personaId ?? null);
            setConversationProcessing(payload.conversationId ?? "", true);
          })();
        }
      }
      if ((payload.type === "assistant_message" || payload.type === "conversation_updated") && payload.conversationId) {
        setConversationProcessing(payload.conversationId, false);
        // Increment unread count for non-active conversations (assistant reply only, deduplicated)
        if (payload.type === "assistant_message" && payload.conversationId) {
          const state = useAppStore.getState();
          if (payload.conversationId !== state.activeConversationId) {
            const conv = state.conversations.find((c) => c.id === payload.conversationId);
            const updatedAt = conv?.updatedAt ?? "";
            const prev = lastCountedMessageRef.current.get(payload.conversationId);
            if (prev !== updatedAt) {
              lastCountedMessageRef.current.set(payload.conversationId, updatedAt);
              useAppStore.setState((s) => ({
                conversationUnreadCounts: {
                  ...s.conversationUnreadCounts,
                  [payload.conversationId!]: (s.conversationUnreadCounts[payload.conversationId!] ?? 0) + 1
                }
              }));
            }
          }
        }
      }
      if ((payload.type === "assistant_stream" || payload.type === "new_message" || payload.type === "tool_message" || payload.type === "assistant_message") && payload.message && isVisibleChatEventMessage(payload.message)) {
        upsertIncomingMessage(payload.message);
      }
      if (payload.type === "assistant_stream") {
        return;
      }
      if (payload.type === "tool_message") {
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
    };
  }, [refreshChatData, setConversationProcessing, upsertIncomingMessage]);

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
      if (payload.conversationId) {
        const running = !["completed", "failed", "aborted"].includes(payload.state);
        setConversationProcessing(payload.conversationId, running);
      }
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
  }, [handleAgentRunEvent, refreshAgentQueue, refreshAgentRuns, refreshChatData, setConversationProcessing, upsertIncomingMessage]);

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
  if (section === "mcp") return <McpPanel />;
  if (section === "settings") return <SettingsPanel />;
  if (section === "memory") return <MemoryPanel />;
  if (section === "worldbooks") return <WorldbooksPanel />;
  if (section === "plugins") return <PluginsPanel />;
  if (section === "agents") return <AgentsPanel />;
  if (section === "skills") return <SkillsPanel />;
  return null;
}

function ContactsPanel() {
  const {
    personas,
    llmProviders,
    conversations,
    accounts,
    setSection,
    openPersonaConversation,
    linkWechatAccount,
    unlinkWechatAccount,
    refreshAccounts
  } = useAppStore();
  const [query, setQuery] = useState("");
  const [selectedPersonaId, setSelectedPersonaId] = useState(personas[0]?.id ?? "");
  const [showWechatSheet, setShowWechatSheet] = useState(false);
  const [pollStatus, setPollStatus] = useState("");
  const filtered = personas.filter((persona) =>
    `${persona.name} ${persona.id} ${persona.llmProvider} ${persona.llmModel}`.toLowerCase().includes(query.toLowerCase())
  );
  const selectedPersona = personas.find((p) => p.id === selectedPersonaId) ?? personas[0] ?? null;
  const linkedAccount = selectedPersona ? accounts.find((account) => account.linkedPersona === selectedPersona.id) : null;
  const syncLinkedWechat = async () => {
    if (!linkedAccount) return;
    setPollStatus("正在同步微信消息...");
    try {
      const result = await api.wechatPollOnce(linkedAccount.id);
      await refreshAccounts();
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
            const provider = persona.llmProvider ? llmProviders.find((p) => p.id === persona.llmProvider) : null;
            const modelInfo = persona.llmModel || provider?.model || "";
            const providerName = provider?.name || "";
            let infoText = "";
            if (providerName || modelInfo) {
              infoText = [providerName, modelInfo].filter(Boolean).join(" · ");
            } else if (llmProviders.length > 0) {
              infoText = "请选择服务商";
            } else {
              infoText = "未配置服务商";
            }
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
                  <small>{infoText}</small>
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
              <MenuRow icon={Brain} label="长期记忆" value="管理记忆" onClick={() => setSection("memory")} />
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
