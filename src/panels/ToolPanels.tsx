import { useEffect, useMemo, useRef, useState } from "react";
import { BookOpen, Bot, Brain, ChevronDown, ChevronRight, Clock, Download, Edit3, ExternalLink, Globe, Hash, Layers, PlugZap, Plus, Puzzle, RefreshCw, Search, Shield, Sparkles, Star, Terminal, Trash2, XCircle } from "lucide-react";

// Mock listen function for standalone frontend
function listen<T>(event: string, handler: (event: { payload: T }) => void): Promise<() => void> {
  console.log(`[Mock Event] Registered listener for: ${event}`);
  return Promise.resolve(() => {
    console.log(`[Mock Event] Unregistered listener for: ${event}`);
  });
}
import { api } from "../lib/api";
import { filterSkillsByQuery } from "../lib/skillSearch";
import { useAppStore } from "../lib/store";
import type { AgentControlCommand, AgentDefinition, AgentQueuedRequest, AgentRunRecord, AgentTodoItem, CapabilityAdapter, EnhancedSkillSummary, ManagedProcessSnapshot, MarketplaceSkill, MemoryStatus, ModelCatalogEntry, PlannerTraceRecord, PluginAuxiliaryTaskSummary, PluginSummary, ScheduledAgentJob, ScheduledJobOutputRecord, SkillAuditLogEntry, SkillBundle, SkillInstallRecord, SkillTap, SkillTapStatus, SkillUpdateCheck, StateSnapshotManifest, ToolApprovalRequest, ToolArtifactRecord, ToolDefinition, ToolRouterTraceRecord, ToolTraceEntry, WorkspaceSnapshotManifest, Worldbook } from "../lib/types";

type SkillUrlPreset = {
  id: string;
  label: string;
  url: string;
  category: string;
};

const DEFAULT_SKILL_URL_PRESETS: SkillUrlPreset[] = [
  {
    id: "hermes-agent",
    label: "hermes-agent",
    url: "https://raw.githubusercontent.com/NousResearch/hermes-agent/main/skills/autonomous-ai-agents/hermes-agent/SKILL.md",
    category: "autonomous-ai-agents"
  },
  {
    id: "writing-plans",
    label: "writing-plans",
    url: "https://raw.githubusercontent.com/NousResearch/hermes-agent/main/skills/software-development/writing-plans/SKILL.md",
    category: "software-development"
  },
  {
    id: "systematic-debugging",
    label: "systematic-debugging",
    url: "https://raw.githubusercontent.com/NousResearch/hermes-agent/main/skills/software-development/systematic-debugging/SKILL.md",
    category: "software-development"
  },
  {
    id: "github-code-review",
    label: "github-code-review",
    url: "https://raw.githubusercontent.com/NousResearch/hermes-agent/main/skills/github/github-code-review/SKILL.md",
    category: "github"
  }
] as const;

const SKILL_URL_PRESETS_STORAGE_KEY = "synthchat.skillUrlPresets.v1";
const DEFAULT_SKILL_URL_PRESET_ID_STORAGE_KEY = "synthchat.skillUrlPreset.default.v1";

function normalizeSkillUrlPreset(value: unknown, index: number): SkillUrlPreset | null {
  if (!value || typeof value !== "object") return null;
  const candidate = value as Partial<SkillUrlPreset>;
  const label = typeof candidate.label === "string" ? candidate.label.trim() : "";
  const url = typeof candidate.url === "string" ? candidate.url.trim() : "";
  if (!label || !url) return null;
  const category = typeof candidate.category === "string" ? candidate.category.trim() : "";
  const id = typeof candidate.id === "string" && candidate.id.trim() ? candidate.id.trim() : `preset-${index}-${label.toLowerCase().replace(/\s+/g, "-")}`;
  return { id, label, url, category };
}

function loadSkillUrlPresets() {
  try {
    const raw = window.localStorage.getItem(SKILL_URL_PRESETS_STORAGE_KEY);
    if (!raw) return DEFAULT_SKILL_URL_PRESETS.slice();
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return DEFAULT_SKILL_URL_PRESETS.slice();
    const presets = parsed
      .map((item, index) => normalizeSkillUrlPreset(item, index))
      .filter((item): item is SkillUrlPreset => item !== null);
    return presets.length > 0 ? presets : DEFAULT_SKILL_URL_PRESETS.slice();
  } catch {
    return DEFAULT_SKILL_URL_PRESETS.slice();
  }
}

function saveSkillUrlPresets(presets: SkillUrlPreset[]) {
  window.localStorage.setItem(SKILL_URL_PRESETS_STORAGE_KEY, JSON.stringify(presets));
}

function loadDefaultSkillUrlPresetId(presets: SkillUrlPreset[]) {
  const raw = window.localStorage.getItem(DEFAULT_SKILL_URL_PRESET_ID_STORAGE_KEY)?.trim() ?? "";
  if (raw && presets.some((preset) => preset.id === raw)) return raw;
  return presets[0]?.id ?? "";
}

function saveDefaultSkillUrlPresetId(presetId: string) {
  if (!presetId) {
    window.localStorage.removeItem(DEFAULT_SKILL_URL_PRESET_ID_STORAGE_KEY);
    return;
  }
  window.localStorage.setItem(DEFAULT_SKILL_URL_PRESET_ID_STORAGE_KEY, presetId);
}

function fallbackSkillUrlPresetLabel(url: string) {
  try {
    const parsed = new URL(url);
    const parts = parsed.pathname.split("/").filter(Boolean);
    return parts.slice(-2).join("/") || parsed.hostname;
  } catch {
    return "custom-skill";
  }
}

export function MemoryPanel() {
  const { memories, personas, saveMemory, deleteMemory, goBack } = useAppStore();
  const [activePersonaId, setActivePersonaId] = useState(personas[0]?.id ?? "default");
  const [summary, setSummary] = useState("");
  const [importance, setImportance] = useState(3);
  const [expandedMemoryId, setExpandedMemoryId] = useState<string | null>(null);
  const [memoryStatus, setMemoryStatus] = useState<MemoryStatus | null>(null);
  const [showAddForm, setShowAddForm] = useState(false);

  const activePersona = personas.find((p) => p.id === activePersonaId);

  const filteredMemories = useMemo(
    () => memories.filter((m) => m.personaId === activePersonaId),
    [memories, activePersonaId]
  );

  const refreshMemoryStatus = async (id = activePersonaId) => {
    const status = await api.getMemoryStatus(id);
    setMemoryStatus(status);
  };

  useEffect(() => {
    void refreshMemoryStatus(activePersonaId);
    setExpandedMemoryId(null);
  }, [activePersonaId, memories.length]);

  const submit = async () => {
    const value = summary.trim();
    if (!value) return;
    await saveMemory({ personaId: activePersonaId, target: "memory", summary: value, importance });
    await refreshMemoryStatus(activePersonaId);
    setSummary("");
    setShowAddForm(false);
  };

  const removeMemory = async (id: string) => {
    await deleteMemory(id);
    await refreshMemoryStatus(activePersonaId);
  };

  const formatDate = (iso: string) => {
    const d = new Date(iso);
    const now = new Date();
    const diffMs = now.getTime() - d.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    const diffHours = Math.floor(diffMs / 3600000);
    const diffDays = Math.floor(diffMs / 86400000);
    if (diffMins < 1) return "刚刚";
    if (diffMins < 60) return `${diffMins} 分钟前`;
    if (diffHours < 24) return `${diffHours} 小时前`;
    if (diffDays < 7) return `${diffDays} 天前`;
    return d.toLocaleDateString("zh-CN", { month: "short", day: "numeric" });
  };

  const importanceLabel = (level: number) => {
    const labels = ["", "低", "一般", "中", "高", "关键"];
    return labels[level] ?? "中";
  };

  const importanceColorClass = (level: number) => {
    const classes = ["", "imp-low", "imp-normal", "imp-medium", "imp-high", "imp-critical"];
    return classes[level] ?? "imp-medium";
  };

  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button">
          <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
        </button>
        <div className="panel-title-text">
          <Brain size={16} className="panel-title-icon" />
          <span>Memory</span>
          <strong>记忆管理</strong>
        </div>
      </div>

      {/* ── Persona Tabs ── */}
      <div className="memory-persona-tabs-wrap">
        <div className="memory-persona-tabs">
          {personas.map((persona) => {
            const isActive = persona.id === activePersonaId;
            const personaMemoryCount = memories.filter((m) => m.personaId === persona.id).length;
            return (
              <button
                key={persona.id}
                className={`memory-persona-tab ${isActive ? "active" : ""}`}
                onClick={() => setActivePersonaId(persona.id)}
                type="button"
              >
                <span className="memory-tab-avatar">
                  {persona.avatarPath
                    ? <img src={api.assetUrl(persona.avatarPath)} alt="" />
                    : (persona.name || "?").slice(0, 1).toUpperCase()
                  }
                </span>
                <span className="memory-tab-name">{persona.name}</span>
                {personaMemoryCount > 0 && (
                  <span className="memory-tab-badge">{personaMemoryCount}</span>
                )}
              </button>
            );
          })}
        </div>
      </div>

      {/* ── Stats Dashboard ── */}
      {memoryStatus ? (
        <div className="memory-stats-grid">
          <div className="memory-stat-card">
            <div className="memory-stat-icon"><Layers size={16} /></div>
            <div className="memory-stat-body">
              <span className="memory-stat-value">{memoryStatus.total}</span>
              <span className="memory-stat-label">总记忆</span>
            </div>
          </div>
          <div className="memory-stat-card">
            <div className="memory-stat-icon stat-icon-active"><Sparkles size={16} /></div>
            <div className="memory-stat-body">
              <span className="memory-stat-value">{memoryStatus.promptInjected}</span>
              <span className="memory-stat-label">已注入</span>
            </div>
          </div>
          <div className="memory-stat-card">
            <div className="memory-stat-icon stat-icon-safe"><Shield size={16} /></div>
            <div className="memory-stat-body">
              <span className="memory-stat-value">{memoryStatus.promptSafe}</span>
              <span className="memory-stat-label">安全</span>
            </div>
          </div>
          <div className="memory-stat-card">
            <div className="memory-stat-icon stat-icon-blocked"><XCircle size={16} /></div>
            <div className="memory-stat-body">
              <span className="memory-stat-value">{memoryStatus.blockedBySecurityScan}</span>
              <span className="memory-stat-label">已拦截</span>
            </div>
          </div>
        </div>
      ) : null}

      {/* ── Add Memory ── */}
      <div className="memory-add-section">
        {!showAddForm ? (
          <button className="memory-add-trigger" onClick={() => setShowAddForm(true)} type="button">
            <Plus size={16} />
            <span>为 {activePersona?.name ?? "当前角色"} 添加记忆</span>
          </button>
        ) : (
          <div className="memory-add-form">
            <div className="memory-add-form-header">
              <span>添加新记忆</span>
              <button className="icon-only-btn" onClick={() => setShowAddForm(false)} type="button">
                <XCircle size={16} />
              </button>
            </div>
            <textarea
              className="memory-add-textarea"
              value={summary}
              onChange={(e) => setSummary(e.target.value)}
              placeholder="输入要记住的内容..."
              rows={3}
            />
            <div className="memory-add-footer">
              <div className="memory-importance-picker">
                <span className="memory-importance-label">重要性</span>
                <div className="memory-importance-stars">
                  {[1, 2, 3, 4, 5].map((level) => (
                    <button
                      key={level}
                      className={`memory-star-btn ${level <= importance ? "active" : ""}`}
                      onClick={() => setImportance(level)}
                      type="button"
                      title={importanceLabel(level)}
                    >
                      <Star size={14} />
                    </button>
                  ))}
                </div>
              </div>
              <button className="memory-submit-btn" onClick={submit} type="button" disabled={!summary.trim()}>
                <Plus size={14} />
                <span>添加</span>
              </button>
            </div>
          </div>
        )}
      </div>

      {/* ── Memory List (Dropdown) ── */}
      <div className="memory-list-section">
        <div className="memory-list-header">
          <span className="memory-list-title">
            <BookOpen size={14} />
            <span>记忆列表</span>
            <span className="memory-list-count">{filteredMemories.length}</span>
          </span>
        </div>
        {filteredMemories.length === 0 ? (
          <div className="memory-empty-state">
            <Brain size={32} />
            <p>暂无记忆</p>
            <small>为此角色添加记忆，让它记住重要的事情</small>
          </div>
        ) : (
          <div className="memory-dropdown-list">
            {filteredMemories.map((memory) => {
              const isExpanded = expandedMemoryId === memory.id;
              const truncated = memory.summary.length > 60
                ? memory.summary.slice(0, 60) + "..."
                : memory.summary;
              return (
                <div className={`memory-dropdown-item ${isExpanded ? "expanded" : ""}`} key={memory.id}>
                  <button
                    className="memory-dropdown-trigger"
                    onClick={() => setExpandedMemoryId(isExpanded ? null : memory.id)}
                    type="button"
                  >
                    <span className={`memory-importance-dot ${importanceColorClass(memory.importance)}`} />
                    <span className="memory-dropdown-text">
                      {isExpanded ? memory.summary : truncated}
                    </span>
                    <span className="memory-dropdown-meta">
                      <Clock size={11} />
                      <span>{formatDate(memory.createdAt)}</span>
                    </span>
                    <ChevronDown
                      size={14}
                      className="memory-dropdown-arrow"
                      style={{ transform: isExpanded ? "rotate(180deg)" : "rotate(0deg)" }}
                    />
                  </button>
                  {isExpanded && (
                    <div className="memory-dropdown-body">
                      <div className="memory-detail-grid">
                        <div className="memory-detail-item">
                          <Hash size={12} />
                          <span className="memory-detail-label">目标</span>
                          <span className="memory-detail-value">{memory.target ?? "memory"}</span>
                        </div>
                        <div className="memory-detail-item">
                          <Star size={12} />
                          <span className="memory-detail-label">重要性</span>
                          <span className={`memory-detail-value ${importanceColorClass(memory.importance)}`}>
                            {importanceLabel(memory.importance)} ({memory.importance}/5)
                          </span>
                        </div>
                        <div className="memory-detail-item">
                          <Clock size={12} />
                          <span className="memory-detail-label">创建时间</span>
                          <span className="memory-detail-value">{new Date(memory.createdAt).toLocaleString("zh-CN")}</span>
                        </div>
                        <div className="memory-detail-item">
                          <RefreshCw size={12} />
                          <span className="memory-detail-label">更新时间</span>
                          <span className="memory-detail-value">{new Date(memory.updatedAt).toLocaleString("zh-CN")}</span>
                        </div>
                      </div>
                      <div className="memory-detail-actions">
                        <button
                          className="memory-delete-btn"
                          onClick={() => void removeMemory(memory.id)}
                          type="button"
                        >
                          <Trash2 size={13} />
                          <span>删除此记忆</span>
                        </button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </section>
  );
}

export function WorldbooksPanel() {
  const { worldbooks, personas, saveWorldbook, deleteWorldbook, goBack } = useAppStore();
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [sectionKey, setSectionKey] = useState("");
  const [sectionContent, setSectionContent] = useState("");

  const createBook = async () => {
    const trimmedName = name.trim();
    if (!trimmedName) return;
    const now = new Date().toISOString();
    const book: Worldbook = {
      id: "",
      name: trimmedName,
      description,
      boundPersonas: personas[0]?.id ? [personas[0].id] : [],
      sections: sectionKey.trim() || sectionContent.trim()
        ? [{ id: crypto.randomUUID(), key: sectionKey.trim(), content: sectionContent.trim(), enabled: true }]
        : [],
      createdAt: now,
      updatedAt: now
    };
    await saveWorldbook(book);
    setName("");
    setDescription("");
    setSectionKey("");
    setSectionContent("");
  };

  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
        <div className="panel-title-text"><BookOpen size={16} className="panel-title-icon" /><span>Worldbook</span><strong>世界书</strong></div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">新建世界书</div>
        <div className="settings-form">
          <div className="form-group">
            <div className="form-row">
              <label>名称</label>
              <input value={name} onChange={(event) => setName(event.target.value)} />
            </div>
          </div>
          <div className="form-group">
            <div className="form-row">
              <label>描述</label>
              <input value={description} onChange={(event) => setDescription(event.target.value)} />
            </div>
          </div>
          <div className="form-group">
            <div className="form-row">
              <label>小节关键词</label>
              <input value={sectionKey} onChange={(event) => setSectionKey(event.target.value)} />
            </div>
          </div>
          <div className="form-group">
            <label>小节内容</label>
            <textarea value={sectionContent} onChange={(event) => setSectionContent(event.target.value)} />
          </div>
          <div style={{ padding: "0 16px 12px" }}>
            <button className="btn-primary" onClick={createBook} type="button"><Plus size={16} />新建世界书</button>
          </div>
        </div>
      </div>
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">世界书列表</div>
        {worldbooks.length === 0 ? (
          <div className="form-hint" style={{ padding: "12px 16px" }}>暂无世界书</div>
        ) : (
          worldbooks.map((book) => (
            <div className="memory-item" key={book.id}>
              <div className="memory-content">
                <strong>{book.name}</strong>
                <span className="memory-meta">{book.description || "无描述"} · {book.sections.length} 小节 · 绑定 {book.boundPersonas.length} 角色</span>
              </div>
              <button className="btn-danger-outline-sm" onClick={() => void deleteWorldbook(book.id)} type="button">删除</button>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

export function PluginsPanel() {
  const { plugins, togglePlugin, goBack } = useAppStore();
  const [auxiliaryTasks, setAuxiliaryTasks] = useState<PluginAuxiliaryTaskSummary[]>([]);
  useEffect(() => {
    let cancelled = false;
    api.listPluginAuxiliaryTasks()
      .then((tasks) => {
        if (!cancelled) setAuxiliaryTasks(tasks);
      })
      .catch(() => {
        if (!cancelled) setAuxiliaryTasks([]);
      });
    return () => { cancelled = true; };
  }, [plugins]);
  const auxiliaryTasksByPlugin = useMemo(() => {
    return auxiliaryTasks.reduce<Record<string, PluginAuxiliaryTaskSummary[]>>((acc, task) => {
      const key = task.pluginId || task.pluginName;
      if (!key) return acc;
      acc[key] = [...(acc[key] ?? []), task];
      return acc;
    }, {});
  }, [auxiliaryTasks]);
  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
        <div className="panel-title-text"><PlugZap size={16} className="panel-title-icon" /><span>Plugins</span><strong>插件管理</strong></div>
      </div>
      {plugins.length === 0 ? (
        <div className="empty-state compact">
          <div className="empty-icon-wrap"><Puzzle size={48} strokeWidth={1.5} /></div>
          <p>没有已安装的插件</p>
        </div>
      ) : (
        <div className="plugin-list">
          {plugins.map((plugin) => (
            <div className="card plugin-card" key={plugin.id}>
              {(() => {
                const pluginAuxiliaryTasks = auxiliaryTasksByPlugin[plugin.id] ?? auxiliaryTasksByPlugin[plugin.name] ?? [];
                return (
                  <>
              <div className="plugin-header">
                <strong>{plugin.name}</strong>
                <button
                  className={`status-badge ${plugin.enabled ? "enabled" : "disabled"}`}
                  onClick={() => void togglePlugin(plugin.id, !plugin.enabled)}
                  type="button"
                  title={plugin.enabled ? "点击停用" : "点击启用"}
                >
                  {plugin.enabled ? "启用" : "停用"}
                </button>
              </div>
              <p className="plugin-desc">{plugin.description}</p>
              {(plugin.version || plugin.author) ? (
                <div className="memory-meta" style={{ marginBottom: 4 }}>
                  {plugin.version ? `v${plugin.version}` : ""}
                  {plugin.author ? `${plugin.version ? " · " : ""}${plugin.author}` : ""}
                  {plugin.source ? ` · ${plugin.source}` : ""}
                </div>
              ) : null}
              {plugin.homepageUrl ? (
                <a className="plugin-homepage" href={plugin.homepageUrl} target="_blank" rel="noopener noreferrer" style={{ fontSize: "0.85rem", display: "inline-flex", alignItems: "center", gap: 4 }}>
                  <ExternalLink size={12} /> 主页
                </a>
              ) : null}
              {(plugin.kind || plugin.requiresEnv?.length) ? (
                <div className="memory-meta" style={{ marginBottom: 4 }}>
                  {plugin.kind ? `kind: ${plugin.kind}` : ""}
                  {plugin.requiresEnv?.length ? `${plugin.kind ? " · " : ""}env: ${plugin.requiresEnv.join(", ")}` : ""}
                </div>
              ) : null}
              {plugin.providedTools.length > 0 ? (
                <div className="plugin-tools">
                  <span>工具：</span>
                  {plugin.providedTools.map((tool: string) => (
                    <span className="tool-tag" key={tool}>{tool}</span>
                  ))}
                </div>
              ) : null}
              {plugin.providedHooks?.length ? (
                <div className="plugin-tools">
                  <span>Hooks：</span>
                  {plugin.providedHooks.map((hook: string) => (
                    <span className="tool-tag" key={hook}>{hook}</span>
                  ))}
                </div>
              ) : null}
              {pluginAuxiliaryTasks.length ? (
                <div className="plugin-tools">
                  <span>Aux：</span>
                  {pluginAuxiliaryTasks.map((task) => (
                    <span className="tool-tag" key={`${task.pluginId}:${task.key}`} title={task.description || task.displayName}>
                      {task.key}
                    </span>
                  ))}
                </div>
              ) : null}
                  </>
                );
              })()}
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

function runStatusClass(state: string) {
  if (state === "completed") return "enabled";
  if (["failed", "aborted", "pendingApproval"].includes(state)) return "disabled";
  return "warning";
}

function isActiveRunState(state: string) {
  return ["started", "running", "pendingApproval"].includes(state);
}

function compactDetail(value: unknown, maxLength = 220) {
  if (value === null || value === undefined) return "";
  const text = typeof value === "string" ? value : JSON.stringify(value);
  if (!text) return "";
  return text.length > maxLength ? `${text.slice(0, maxLength)}...` : text;
}

function objectDetail(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}

function llmFailoverSummary(value: unknown) {
  const detail = objectDetail(value);
  if (!detail) return compactDetail(value);
  const finalProvider = typeof detail.finalProviderId === "string" ? detail.finalProviderId : "-";
  const failed = Array.isArray(detail.failedProviders) ? detail.failedProviders : [];
  const failedText = failed
    .map((item) => {
      const failure = objectDetail(item);
      if (!failure) return compactDetail(item, 80);
      const provider = typeof failure.providerId === "string" ? failure.providerId : "-";
      const kind = typeof failure.kind === "string" ? failure.kind : "error";
      const message = typeof failure.message === "string" ? failure.message : "";
      return `${provider} (${kind})${message ? `: ${message}` : ""}`;
    })
    .filter(Boolean)
    .join("；");
  return `final provider: ${finalProvider}${failedText ? `；failed: ${failedText}` : ""}`;
}

function subagentFailureSummary(value: unknown) {
  const detail = objectDetail(value);
  if (!detail) return compactDetail(value);
  const role = typeof detail.role === "string" ? detail.role : "leaf";
  const depth = typeof detail.depth === "number" ? detail.depth : "-";
  const state = typeof detail.state === "string" ? detail.state : "failed";
  const task = typeof detail.task === "string" ? detail.task : "";
  const error = typeof detail.error === "string" ? detail.error : "";
  const toolsets = Array.isArray(detail.toolsets) ? detail.toolsets.filter((item): item is string => typeof item === "string") : [];
  return `role: ${role}；depth: ${depth}；state: ${state}${toolsets.length ? `；toolsets: ${toolsets.join(", ")}` : ""}${task ? `；task: ${task}` : ""}${error ? `；error: ${error}` : ""}`;
}

function formatBytes(value?: number | null) {
  const bytes = typeof value === "number" && Number.isFinite(value) ? value : 0;
  if (bytes < 1024) return `${bytes} B`;
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`;
  const mb = kb / 1024;
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`;
}

function latestRunSignal(run: AgentRunRecord) {
  if (run.error) return run.error;
  const latestTool = run.toolEvents?.[run.toolEvents.length - 1];
  if (latestTool?.error) return latestTool.error;
  if (latestTool?.summary) return latestTool.summary;
  const latestPhase = run.phaseEvents?.[run.phaseEvents.length - 1];
  if (latestPhase?.phase === "llm_failover") return llmFailoverSummary(latestPhase.detail);
  if (latestPhase?.phase === "subagent_failed") return subagentFailureSummary(latestPhase.detail);
  if (latestPhase) return `${latestPhase.phase}${latestPhase.detail ? `: ${compactDetail(latestPhase.detail, 120)}` : ""}`;
  const latestCheckpoint = run.checkpoints?.[run.checkpoints.length - 1];
  if (latestCheckpoint) return latestCheckpoint.summary;
  return run.userRequest || "暂无运行摘要";
}

function latestRunActivity(run: AgentRunRecord) {
  const at = run.lastActivityAt ?? run.updatedAt;
  const desc = run.lastActivityDesc?.trim();
  const time = at ? new Date(at).toLocaleString() : "未知时间";
  return desc ? `${desc} · ${time}` : `updated · ${time}`;
}

const AGENT_TOOLSETS = [
  { id: "browser", label: "Browser", description: "浏览器自动化与页面状态" },
  { id: "web", label: "Web", description: "网页搜索、抓取与 HTTP" },
  { id: "file", label: "File", description: "文件读取、写入与搜索" },
  { id: "terminal", label: "Terminal", description: "命令执行与进程管理" },
  { id: "memory", label: "Memory", description: "长期记忆读取与写入" },
  { id: "todo", label: "Todo", description: "任务规划与进度跟踪" },
  { id: "session_search", label: "Session", description: "会话历史与运行轨迹搜索" },
  { id: "delegation", label: "Delegate", description: "子智能体委派" },
  { id: "clarify", label: "Clarify", description: "向用户澄清缺失信息" },
  { id: "cronjob", label: "Cron", description: "计划任务创建与管理" },
  { id: "vision", label: "Vision", description: "图像、截图与视觉理解" },
  { id: "tts", label: "TTS", description: "语音合成" }
];

type PlatformAdapterState = {
  platform?: string;
  status?: string;
  mode?: string;
  updatedAt?: string;
  startedAt?: string | null;
  stoppedAt?: string | null;
  lastError?: string | null;
  receivedCount?: number;
  triggeredCount?: number;
  configured?: boolean;
  enabled?: boolean;
  runtime?: boolean;
  transport?: string;
  capabilities?: string[];
  runtimeAdapter?: boolean;
  runtime_adapter?: boolean;
  messagingGateway?: boolean;
  messaging_gateway?: boolean;
  externalDaemon?: boolean;
  external_daemon?: boolean;
  gatewayPlatform?: string;
  gateway_platform?: string;
  capabilityMatrix?: Record<string, boolean>;
  capability_matrix?: Record<string, boolean>;
};

function platformAdapterMode(adapter?: PlatformAdapterState | null) {
  if (!adapter) return "unknown";
  if (adapter.mode) return adapter.mode;
  if (adapter.messagingGateway || adapter.messaging_gateway) return "messaging_gateway";
  return adapter.runtime ? "runtime" : "send_only";
}

function platformAdapterCapabilityText(adapter?: PlatformAdapterState | null) {
  const matrix = adapter?.capabilityMatrix ?? adapter?.capability_matrix;
  if (!matrix) return "capabilities=n/a";
  const caps = ["send", "receive", "lifecycle", "attachments"]
    .map((key) => `${key}:${matrix[key] ? "yes" : "no"}`);
  const boundary = [
    adapter?.messagingGateway || adapter?.messaging_gateway ? "gateway" : "",
    adapter?.externalDaemon || adapter?.external_daemon ? "external-daemon" : "",
  ].filter(Boolean);
  return [...caps, ...boundary].join(" · ");
}

function managedProcessId(process: ManagedProcessSnapshot) {
  return process.id || process.sessionId || process.session_id || "unknown";
}

function managedProcessConversationId(process: ManagedProcessSnapshot) {
  return process.conversationId ?? process.conversation_id ?? "";
}

function managedProcessRunId(process: ManagedProcessSnapshot) {
  return process.runId ?? process.run_id ?? "";
}

function managedProcessTime(process: ManagedProcessSnapshot) {
  return process.finishedAt ?? process.finished_at ?? process.startedAt ?? process.started_at ?? "";
}

function managedProcessWatchText(process: ManagedProcessSnapshot) {
  const patterns = process.watchPatterns ?? process.watch_patterns ?? [];
  const stats = process.watchStats ?? process.watch_stats ?? {};
  const matchCount = typeof stats.matchCount === "number" ? stats.matchCount : 0;
  const emitCount = typeof stats.emitCount === "number" ? stats.emitCount : 0;
  const notify = process.notifyOnComplete ?? process.notify_on_complete ?? false;
  return [
    notify ? "notify" : "",
    patterns.length ? `watch:${patterns.join(",")}` : "",
    patterns.length ? `match:${matchCount}/emit:${emitCount}` : "",
  ].filter(Boolean).join(" · ") || "watch:off";
}

function asRecord(value: unknown): Record<string, any> {
  return value && typeof value === "object" && !Array.isArray(value) ? value as Record<string, any> : {};
}

function compactJson(value: unknown, limit = 180) {
  const text = JSON.stringify(value ?? {});
  return text.length > limit ? `${text.slice(0, limit)}...` : text;
}

function browserRuntimeLines(status: Record<string, unknown> | null) {
  const provider = asRecord(status?.provider);
  const supervisor = asRecord(status?.supervisor);
  const activeProvider = asRecord(provider.activeProvider ?? provider.hermesResolvedProvider);
  const providers = Array.isArray(provider.providers) ? provider.providers : [];
  const summary = asRecord(supervisor.summary);
  return {
    title: activeProvider.name || activeProvider.id || provider.hermesResolutionReason || "local",
    detail: `providers:${providers.length} · reason:${provider.hermesResolutionReason ?? "n/a"} · supervisor:${summary.status ?? summary.active ?? "n/a"}`,
    raw: compactJson(status)
  };
}

function computerUseRuntimeLines(status: Record<string, unknown> | null) {
  const backend = asRecord(status?.backend);
  const lifecycle = asRecord(status?.lifecycle);
  return {
    title: backend.name || status?.platform || "computer_use",
    detail: `platform:${status?.platform ?? "n/a"} · available:${String(backend.available ?? backend.ok ?? "n/a")} · lifecycle:${lifecycle.status ?? lifecycle.lastStatus ?? "n/a"}`,
    raw: compactJson(status)
  };
}

export function McpPanel() {
  const { mcpServers, capabilityAdapters, agentRuns, conversations, activeConversationId, personas, lastMcpResult, lastMcpToolsResult, callMcpTool, listMcpTools, saveMcpServers, saveCapabilityAdapters, refreshAgentRuns, bootstrap, goBack } = useAppStore();
  const [payload, setPayload] = useState('{"query":"ping"}');
  const [toolName, setToolName] = useState("echo");
  const [selectedServerId, setSelectedServerId] = useState("");
  const [busy, setBusy] = useState(false);
  const [mcpNotice, setMcpNotice] = useState("");
  const [mcpOauthCallback, setMcpOauthCallback] = useState("");
  const [mcpStatus, setMcpStatus] = useState<Record<string, any> | null>(null);
  const [traces, setTraces] = useState<ToolTraceEntry[]>([]);
  const [toolDefinitions, setToolDefinitions] = useState<ToolDefinition[]>([]);
  const [approvals, setApprovals] = useState<ToolApprovalRequest[]>([]);
  const [controlCommands, setControlCommands] = useState<AgentControlCommand[]>([]);
  const [agentQueue, setAgentQueue] = useState<AgentQueuedRequest[]>([]);
  const [agentTodos, setAgentTodos] = useState<AgentTodoItem[]>([]);
  const [managedProcesses, setManagedProcesses] = useState<ManagedProcessSnapshot[]>([]);
  const [browserRuntimeStatus, setBrowserRuntimeStatus] = useState<Record<string, unknown> | null>(null);
  const [computerUseRuntimeStatus, setComputerUseRuntimeStatus] = useState<Record<string, unknown> | null>(null);
  const [scheduledJobs, setScheduledJobs] = useState<ScheduledAgentJob[]>([]);
  const [scheduledPrompt, setScheduledPrompt] = useState("");
  const [scheduledName, setScheduledName] = useState("");
  const [scheduledKind, setScheduledKind] = useState<"once" | "interval" | "cron">("once");
  const [scheduledRunAt, setScheduledRunAt] = useState("");
  const [scheduledInterval, setScheduledInterval] = useState(60);
  const [scheduledCronExpr, setScheduledCronExpr] = useState("0 9 * * *");
  const [scheduledEnabledToolsets, setScheduledEnabledToolsets] = useState("");
  const [scheduledDisabledToolsets, setScheduledDisabledToolsets] = useState("cronjob,clarify");
  const [expandedScheduledOutputJobId, setExpandedScheduledOutputJobId] = useState<string | null>(null);
  const [scheduledOutputsByJob, setScheduledOutputsByJob] = useState<Record<string, ScheduledJobOutputRecord[]>>({});
  const [stateSnapshots, setStateSnapshots] = useState<StateSnapshotManifest[]>([]);
  const [workspaceSnapshots, setWorkspaceSnapshots] = useState<WorkspaceSnapshotManifest[]>([]);
  const [snapshotLabel, setSnapshotLabel] = useState("");
  const [workspaceSnapshotLabel, setWorkspaceSnapshotLabel] = useState("");
  const [deleteNewWorkspaceFiles, setDeleteNewWorkspaceFiles] = useState(false);
  const [snapshotKeep, setSnapshotKeep] = useState(5);
  const [snapshotNotice, setSnapshotNotice] = useState("");
  const [toolApprovalMode, setToolApprovalMode] = useState("risky");
  const [toolMutationCheckpointEnabled, setToolMutationCheckpointEnabled] = useState(true);
  const [trustedToolPatterns, setTrustedToolPatterns] = useState<string[]>([]);
  const [trustedToolPatternDraft, setTrustedToolPatternDraft] = useState("");
  const [trustedCommandPatterns, setTrustedCommandPatterns] = useState<string[]>([]);
  const [trustedCommandPatternDraft, setTrustedCommandPatternDraft] = useState("");
  const [exportedBundlePath, setExportedBundlePath] = useState("");
  const [expandedRunId, setExpandedRunId] = useState<string | null>(null);
  const [expandedDiagnosticRunId, setExpandedDiagnosticRunId] = useState<string | null>(null);
  const [artifactsByRun, setArtifactsByRun] = useState<Record<string, ToolArtifactRecord[]>>({});
  const [mattermostAdapterState, setMattermostAdapterState] = useState<PlatformAdapterState | null>(null);
  const [platformAdapterStates, setPlatformAdapterStates] = useState<PlatformAdapterState[]>([]);
  const server = mcpServers.find((item) => item.id === selectedServerId) ?? mcpServers[0];
  const selectedMcpStatus = useMemo(() => {
    const servers = Array.isArray(mcpStatus?.servers) ? mcpStatus?.servers : [];
    return servers.find((item: Record<string, any>) => item.id === server?.id || item.name === server?.name) ?? null;
  }, [mcpStatus, server?.id, server?.name]);
  const pendingApprovalRunIds = new Set(
    approvals
      .filter((approval) => approval.status === "pending" && approval.runId)
      .map((approval) => approval.runId as string)
  );
  const topLevelAgentRuns = useMemo(() => agentRuns.filter((run) => !run.parentRunId), [agentRuns]);
  const childAgentRuns = useMemo(() => agentRuns.filter((run) => run.parentRunId), [agentRuns]);
  const childAgentRunGroups = useMemo(() => {
    const topLevelById = new Map(topLevelAgentRuns.map((run) => [run.runId, run]));
    const groups = new Map<string, { parentRun: AgentRunRecord | null; runs: AgentRunRecord[]; updatedAt: string }>();
    for (const run of childAgentRuns) {
      const parentId = run.parentRunId ?? "unknown";
      const existing = groups.get(parentId) ?? {
        parentRun: topLevelById.get(parentId) ?? null,
        runs: [],
        updatedAt: run.updatedAt
      };
      existing.runs.push(run);
      if (new Date(run.updatedAt).getTime() > new Date(existing.updatedAt).getTime()) {
        existing.updatedAt = run.updatedAt;
      }
      groups.set(parentId, existing);
    }
    return Array.from(groups.entries())
      .map(([parentRunId, group]) => ({
        parentRunId,
        parentRun: group.parentRun,
        updatedAt: group.updatedAt,
        runs: group.runs
          .sort((a, b) => (a.subagentIndex ?? 0) - (b.subagentIndex ?? 0) || new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime())
          .slice(0, 6)
      }))
      .sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime())
      .slice(0, 6);
  }, [childAgentRuns, topLevelAgentRuns]);
  const resumableRuns = agentRuns
    .filter((run) => !run.parentRunId)
    .filter((run) => !["completed", "running", "started", "aborted"].includes(run.state))
    .filter((run) => !pendingApprovalRunIds.has(run.runId))
    .slice(0, 6);
  const rerunnableRuns = topLevelAgentRuns
    .filter((run) => ["completed", "failed", "aborted"].includes(run.state))
    .slice(0, 4);
  const activeRuns = topLevelAgentRuns
    .filter((run) => ["started", "running"].includes(run.state))
    .slice(0, 6);
  const approvalBlockedRuns = topLevelAgentRuns
    .filter((run) => pendingApprovalRunIds.has(run.runId))
    .slice(0, 4);
  const visibleQueue = agentQueue
    .filter((item) => ["pending", "running", "failed", "canceled"].includes(item.status))
    .slice(0, 8);
  const visibleScheduledJobs = scheduledJobs.slice(0, 8);
  const pendingApprovals = useMemo(
    () => approvals.filter((approval) => approval.status === "pending"),
    [approvals]
  );
  const recentApprovalHistory = useMemo(
    () =>
      approvals
        .filter((approval) => approval.status !== "pending")
        .sort((a, b) => new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime())
        .slice(0, 8),
    [approvals]
  );
  const groupedControlCommands = useMemo(() => {
    const groups: Array<{ category: string; commands: AgentControlCommand[] }> = [];
    for (const command of controlCommands) {
      const category = command.category || "General";
      let group = groups.find((item) => item.category === category);
      if (!group) {
        group = { category, commands: [] };
        groups.push(group);
      }
      group.commands.push(command);
    }
    return groups;
  }, [controlCommands]);
  const recentRuns = topLevelAgentRuns.slice(0, 8);
  const visibleTodos = agentTodos
    .filter((item) => agentRuns.some((run) => run.runId === item.runId && !["completed", "failed", "aborted"].includes(run.state)))
    .slice(0, 12);
  const visibleManagedProcesses = useMemo(
    () => managedProcesses
      .slice()
      .sort((a, b) => String(managedProcessTime(b)).localeCompare(String(managedProcessTime(a))))
      .slice(0, 8),
    [managedProcesses]
  );
  const browserRuntime = useMemo(() => browserRuntimeLines(browserRuntimeStatus), [browserRuntimeStatus]);
  const computerUseRuntime = useMemo(() => computerUseRuntimeLines(computerUseRuntimeStatus), [computerUseRuntimeStatus]);

  useEffect(() => {
    if (!selectedServerId && mcpServers[0]) setSelectedServerId(mcpServers[0].id);
  }, [mcpServers, selectedServerId]);

  const refreshTraces = async () => {
    const next = await api.listToolTraces();
    setTraces(next.slice(-8).reverse());
  };

  const refreshToolDefinitions = async () => {
    setToolDefinitions(await api.listToolDefinitions());
  };

  const refreshMcpStatus = async () => {
    setMcpStatus(await api.getMcpStatus());
  };

  const refreshApprovals = async () => {
    setApprovals(await api.listToolApprovals());
  };

  const refreshControlCommands = async () => {
    setControlCommands(await api.listAgentControlCommands());
  };

  const refreshAgentQueue = async () => {
    setAgentQueue(await api.listAgentQueue());
  };

  const refreshAgentTodos = async () => {
    setAgentTodos(await api.listAgentTodos());
  };

  const refreshManagedProcesses = async () => {
    setManagedProcesses(await api.listManagedProcesses());
  };

  const refreshBrowserComputerRuntime = async () => {
    const [browserStatus, computerStatus] = await Promise.all([
      api.browserRuntimeStatus(),
      api.computerUseRuntimeStatus()
    ]);
    setBrowserRuntimeStatus(browserStatus);
    setComputerUseRuntimeStatus(computerStatus);
  };

  const refreshMattermostAdapter = async () => {
    const status = await api.platformAdapterStatus();
    const adapters = Array.isArray(status.adapters) ? (status.adapters as PlatformAdapterState[]) : [];
    setPlatformAdapterStates(adapters);
    const mattermost = adapters.find((adapter) => adapter.platform === "mattermost");
    setMattermostAdapterState(mattermost ?? await api.mattermostAdapterStatus());
  };

  const refreshScheduledJobs = async () => {
    setScheduledJobs(await api.listScheduledAgentJobs());
  };

  const refreshStateSnapshots = async () => {
    setStateSnapshots(await api.listStateSnapshots());
  };

  const refreshWorkspaceSnapshots = async () => {
    setWorkspaceSnapshots(await api.listWorkspaceSnapshots());
  };

  const refreshTrustedTools = async () => {
    const config = await api.getConfig();
    setToolApprovalMode(config.chat.toolApprovalMode ?? "risky");
    setToolMutationCheckpointEnabled(config.chat.toolMutationCheckpointEnabled ?? true);
    setTrustedToolPatterns(config.chat.trustedToolPatterns ?? []);
    setTrustedCommandPatterns(config.chat.trustedCommandPatterns ?? []);
  };

  const refreshAgentRuntime = async () => {
    await Promise.all([refreshTraces(), refreshApprovals(), refreshControlCommands(), refreshAgentQueue(), refreshAgentTodos(), refreshManagedProcesses(), refreshBrowserComputerRuntime(), refreshMattermostAdapter(), refreshScheduledJobs(), refreshStateSnapshots(), refreshWorkspaceSnapshots(), refreshTrustedTools(), refreshAgentRuns()]);
  };

  const refreshRegistryFromServers = async () => {
    setBusy(true);
    try {
      setToolDefinitions(await api.refreshToolRegistry());
      await refreshTraces();
      await refreshApprovals();
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    void refreshTraces();
    void refreshMcpStatus();
    void refreshToolDefinitions();
    void refreshApprovals();
    void refreshAgentQueue();
    void refreshAgentTodos();
    void refreshManagedProcesses();
    void refreshBrowserComputerRuntime();
    void refreshMattermostAdapter();
    void refreshScheduledJobs();
    void refreshStateSnapshots();
    void refreshWorkspaceSnapshots();
    void refreshTrustedTools();
    void refreshAgentRuns();
  }, [refreshAgentRuns]);

  useEffect(() => {
    if (!expandedDiagnosticRunId || artifactsByRun[expandedDiagnosticRunId]) return;
    let canceled = false;
    void api.listToolArtifactsForRun(expandedDiagnosticRunId).then((items) => {
      if (canceled) return;
      setArtifactsByRun((current) => ({ ...current, [expandedDiagnosticRunId]: items }));
    });
    return () => {
      canceled = true;
    };
  }, [artifactsByRun, expandedDiagnosticRunId]);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ ok: boolean; count: number; error?: string | null }>(
      "synthchat-tool-registry-event",
      () => {
        void refreshToolDefinitions();
      }
    ).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen("synthchat-managed-process-event", () => {
      void refreshManagedProcesses();
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let unlisten: (() => void) | null = null;
    void listen<{ platform?: string; state?: PlatformAdapterState }>(
      "synthchat-platform-adapter-event",
      (event) => {
        if (event.payload.state) {
          const nextState = event.payload.state;
          setPlatformAdapterStates((current) => {
            const platform = nextState.platform;
            if (!platform) return current;
            const replaced = current.map((adapter) => adapter.platform === platform ? { ...adapter, ...nextState } : adapter);
            return replaced.some((adapter) => adapter.platform === platform) ? replaced : [...replaced, nextState];
          });
          if (nextState.platform === "mattermost") {
            setMattermostAdapterState(nextState);
          }
          return;
        }
        void refreshMattermostAdapter();
      }
    ).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  const runTool = async () => {
    if (!server) return;
    setBusy(true);
    try {
      let parsed: unknown = {};
      try {
        parsed = JSON.parse(payload);
      } catch {
        parsed = { text: payload };
      }
      await callMcpTool(server.id, toolName, parsed, server.timeoutSeconds);
      await refreshTraces();
      await refreshApprovals();
    } finally {
      setBusy(false);
    }
  };

  const startMattermostAdapter = async () => {
    await startPlatformAdapter("mattermost");
  };

  const stopMattermostAdapter = async () => {
    await stopPlatformAdapter("mattermost");
  };

  const startPlatformAdapter = async (platform: string) => {
    setBusy(true);
    try {
      const state = await api.startPlatformAdapter(platform);
      if (platform === "mattermost") setMattermostAdapterState(state);
      await refreshMattermostAdapter();
    } finally {
      setBusy(false);
    }
  };

  const stopPlatformAdapter = async (platform: string) => {
    setBusy(true);
    try {
      const state = await api.stopPlatformAdapter(platform);
      if (platform === "mattermost") setMattermostAdapterState(state);
      await refreshMattermostAdapter();
    } finally {
      setBusy(false);
    }
  };

  const stopManagedProcess = async (processId: string) => {
    setBusy(true);
    try {
      await api.stopManagedProcess(processId);
      await refreshManagedProcesses();
    } finally {
      setBusy(false);
    }
  };

  const approve = async (approvalId: string) => {
    setBusy(true);
    try {
      await api.approveToolCall(approvalId, server?.timeoutSeconds);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const approveAlways = async (approvalId: string) => {
    setBusy(true);
    try {
      await api.approveToolCallAlways(approvalId, server?.timeoutSeconds);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const approveServer = async (approvalId: string) => {
    setBusy(true);
    try {
      await api.approveToolCallServer(approvalId, server?.timeoutSeconds);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const addTrustedTool = async () => {
    const pattern = trustedToolPatternDraft.trim();
    if (!pattern) return;
    setBusy(true);
    try {
      const config = await api.addTrustedToolPattern(pattern);
      setTrustedToolPatterns(config.chat.trustedToolPatterns ?? []);
      setTrustedToolPatternDraft("");
    } finally {
      setBusy(false);
    }
  };

  const updateToolApprovalMode = async (mode: string) => {
    setBusy(true);
    try {
      const config = await api.getConfig();
      await api.saveConfig({ ...config, chat: { ...config.chat, toolApprovalMode: mode } });
      setToolApprovalMode(mode);
    } finally {
      setBusy(false);
    }
  };

  const updateToolMutationCheckpoint = async (enabled: boolean) => {
    setBusy(true);
    try {
      const config = await api.getConfig();
      await api.saveConfig({ ...config, chat: { ...config.chat, toolMutationCheckpointEnabled: enabled } });
      setToolMutationCheckpointEnabled(enabled);
    } finally {
      setBusy(false);
    }
  };

  const removeTrustedTool = async (pattern: string) => {
    setBusy(true);
    try {
      const config = await api.removeTrustedToolPattern(pattern);
      setTrustedToolPatterns(config.chat.trustedToolPatterns ?? []);
    } finally {
      setBusy(false);
    }
  };

  const addTrustedCommand = async () => {
    const pattern = trustedCommandPatternDraft.trim();
    if (!pattern) return;
    setBusy(true);
    try {
      const config = await api.getConfig();
      const nextPatterns = Array.from(new Set([...(config.chat.trustedCommandPatterns ?? []), pattern]));
      await api.saveConfig({ ...config, chat: { ...config.chat, trustedCommandPatterns: nextPatterns } });
      setTrustedCommandPatterns(nextPatterns);
      setTrustedCommandPatternDraft("");
    } finally {
      setBusy(false);
    }
  };

  const removeTrustedCommand = async (pattern: string) => {
    setBusy(true);
    try {
      const config = await api.getConfig();
      const nextPatterns = (config.chat.trustedCommandPatterns ?? []).filter((item) => item !== pattern);
      await api.saveConfig({ ...config, chat: { ...config.chat, trustedCommandPatterns: nextPatterns } });
      setTrustedCommandPatterns(nextPatterns);
    } finally {
      setBusy(false);
    }
  };

  const deny = async (approvalId: string) => {
    setBusy(true);
    try {
      await api.denyToolCall(approvalId, "User denied from MCP panel.");
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const resumeRun = async (runId: string, checkpointId?: string | null) => {
    setBusy(true);
    try {
      await api.resumeAgentRun(runId, checkpointId);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const rerunRun = async (runId: string) => {
    setBusy(true);
    try {
      await api.rerunAgentRun(runId);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const abortRun = async (runId: string) => {
    setBusy(true);
    try {
      await api.abortAgentRun(runId, "User aborted from MCP panel.");
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const drainQueue = async () => {
    setBusy(true);
    try {
      setAgentQueue(await api.drainAgentQueue());
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const cancelQueueItem = async (id: string) => {
    setBusy(true);
    try {
      await api.cancelAgentQueueItem(id);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const clearFinishedQueueItems = async () => {
    setBusy(true);
    try {
      setAgentQueue(await api.clearFinishedAgentQueueItems());
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const createScheduledJob = async () => {
    const prompt = scheduledPrompt.trim();
    if (!prompt) return;
    const personaId = personas[0]?.id ?? "default";
    const conversationId = activeConversationId ?? conversations[0]?.id ?? null;
    const runAt = scheduledKind === "once"
      ? (scheduledRunAt ? new Date(scheduledRunAt).toISOString() : new Date(Date.now() + 60_000).toISOString())
      : null;
    setBusy(true);
    try {
      await api.saveScheduledAgentJob({
        id: "",
        name: scheduledName.trim(),
        conversationId,
        personaId,
        prompt,
        scheduleKind: scheduledKind,
        intervalMinutes: scheduledKind === "interval" ? Math.max(1, scheduledInterval) : null,
        cronExpr: scheduledKind === "cron" ? scheduledCronExpr.trim() : null,
        runAt,
        enabledToolsets: scheduledEnabledToolsets.split(",").map((item) => item.trim()).filter(Boolean),
        disabledToolsets: scheduledDisabledToolsets.split(",").map((item) => item.trim()).filter(Boolean),
        enabled: true,
        status: "scheduled",
        lastCompletedAt: null,
        lastRunStatus: null,
        lastOutput: null,
        lastOutputPath: null,
        lastError: null,
        runCount: 0,
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString()
      });
      setScheduledPrompt("");
      setScheduledName("");
      setScheduledEnabledToolsets("");
      setScheduledDisabledToolsets("cronjob,clarify");
      await refreshScheduledJobs();
    } finally {
      setBusy(false);
    }
  };

  const tickScheduledJobs = async () => {
    setBusy(true);
    try {
      await api.tickScheduledAgentJobs();
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const toggleScheduledJob = async (job: ScheduledAgentJob) => {
    setBusy(true);
    try {
      await api.setScheduledAgentJobEnabled(job.id, !job.enabled);
      await refreshScheduledJobs();
    } finally {
      setBusy(false);
    }
  };

  const deleteScheduledJob = async (id: string) => {
    setBusy(true);
    try {
      await api.deleteScheduledAgentJob(id);
      await refreshScheduledJobs();
      setScheduledOutputsByJob((current) => {
        const next = { ...current };
        delete next[id];
        return next;
      });
      if (expandedScheduledOutputJobId === id) setExpandedScheduledOutputJobId(null);
    } finally {
      setBusy(false);
    }
  };

  const toggleScheduledOutputs = async (jobId: string) => {
    if (expandedScheduledOutputJobId === jobId) {
      setExpandedScheduledOutputJobId(null);
      return;
    }
    setExpandedScheduledOutputJobId(jobId);
    setScheduledOutputsByJob((current) => ({ ...current, [jobId]: current[jobId] ?? [] }));
    const outputs = await api.listScheduledJobOutputs(jobId);
    setScheduledOutputsByJob((current) => ({ ...current, [jobId]: outputs }));
  };

  const createSnapshot = async () => {
    setBusy(true);
    try {
      const label = snapshotLabel.trim() || "manual";
      const snapshot = await api.createStateSnapshot(label);
      setSnapshotNotice(`已创建快照 ${snapshot.id}`);
      setSnapshotLabel("");
      await refreshStateSnapshots();
    } finally {
      setBusy(false);
    }
  };

  const pruneSnapshots = async () => {
    setBusy(true);
    try {
      const deleted = await api.pruneStateSnapshots(snapshotKeep);
      setSnapshotNotice(`已裁剪 ${deleted} 个旧快照`);
      await refreshStateSnapshots();
    } finally {
      setBusy(false);
    }
  };

  const restoreSnapshot = async (snapshotId: string) => {
    if (!window.confirm(`恢复 ${snapshotId} 会覆盖当前本地状态，并自动创建 pre-restore 快照。继续？`)) return;
    setBusy(true);
    try {
      const result = await api.restoreStateSnapshot(snapshotId);
      setSnapshotNotice(`已恢复 ${result.restored.id}，恢复前快照 ${result.preRestore.id}`);
      await bootstrap();
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const createWorkspaceSnapshot = async () => {
    setBusy(true);
    try {
      const label = workspaceSnapshotLabel.trim() || "manual-workspace";
      const snapshot = await api.createWorkspaceSnapshot(label);
      setSnapshotNotice(`已创建 workspace 快照 ${snapshot.id}，files=${snapshot.fileCount ?? 0}`);
      setWorkspaceSnapshotLabel("");
      await refreshWorkspaceSnapshots();
    } finally {
      setBusy(false);
    }
  };

  const restoreWorkspaceSnapshot = async (snapshotId: string) => {
    const deleteHint = deleteNewWorkspaceFiles ? "，并删除快照后新增的非排除文件" : "";
    if (!window.confirm(`恢复 workspace 快照 ${snapshotId} 会覆盖同名文件${deleteHint}，并自动创建 pre-restore 快照。继续？`)) return;
    setBusy(true);
    try {
      const result = await api.restoreWorkspaceSnapshot(snapshotId, deleteNewWorkspaceFiles);
      setSnapshotNotice(`已恢复 workspace ${result.restored.id}，恢复 ${result.restoredFiles ?? 0} 个文件，删除新增 ${result.removedNewFiles ?? 0} 个`);
      await refreshWorkspaceSnapshots();
    } finally {
      setBusy(false);
    }
  };

  const exportRunBundle = async (runId: string) => {
    setBusy(true);
    try {
      setExportedBundlePath(await api.exportAgentRunBundle(runId));
    } finally {
      setBusy(false);
    }
  };

  const diagnoseRun = async (runId: string) => {
    setBusy(true);
    try {
      await api.diagnoseAgentRun(runId);
      await refreshAgentRuntime();
    } finally {
      setBusy(false);
    }
  };

  const updateTimeout = async (value: number) => {
    if (!server) return;
    await saveMcpServers(
      mcpServers.map((item) =>
        item.id === server.id ? { ...item, timeoutSeconds: Math.max(1, value) } : item
      )
    );
  };

  const updateProtocol = async (protocol: "oneShotJson" | "mcpJsonRpc" | "mcpJsonRpcLine") => {
    if (!server) return;
    await saveMcpServers(mcpServers.map((item) => (item.id === server.id ? { ...item, protocol } : item)));
  };

  const toggleParallelToolCalls = async () => {
    if (!server) return;
    await saveMcpServers(
      mcpServers.map((item) =>
        item.id === server.id
          ? { ...item, supportsParallelToolCalls: !item.supportsParallelToolCalls }
          : item
      )
    );
  };

  const togglePersistentSession = async () => {
    if (!server) return;
    await saveMcpServers(
      mcpServers.map((item) =>
        item.id === server.id
          ? { ...item, persistentSession: !item.persistentSession }
          : item
      )
    );
    await refreshMcpStatus();
  };

  const resetPersistentSession = async () => {
    if (!server) return;
    setBusy(true);
    try {
      const result = await api.resetMcpPersistentSession(server.id);
      const closed = Array.isArray(result.closed) ? result.closed.length : 0;
      const missing = Array.isArray(result.missing) ? result.missing.length : 0;
      setMcpNotice(`已重置 MCP persistent session：closed=${closed} · missing=${missing}`);
      await refreshMcpStatus();
    } finally {
      setBusy(false);
    }
  };

  const toggleAdapter = async (adapter: CapabilityAdapter) => {
    await saveCapabilityAdapters(
      capabilityAdapters.map((item) =>
        item.name === adapter.name ? { ...item, enabled: !item.enabled } : item
      )
    );
  };

  const discoverTools = async () => {
    if (!server) return;
    setBusy(true);
    try {
      await listMcpTools(server.id, server.timeoutSeconds);
    } finally {
      setBusy(false);
    }
  };

  const clearMcpOauthCache = async () => {
    if (!server) return;
    if (!window.confirm(`清理 ${server.name || server.id} 的 MCP OAuth 缓存？清理后需要重新认证。`)) return;
    setBusy(true);
    try {
      const result = await api.removeMcpOauthTokens(server.id);
      const removed = Array.isArray(result.removed) ? result.removed.length : 0;
      const missing = Array.isArray(result.missing) ? result.missing.length : 0;
      setMcpNotice(`已清理 OAuth 缓存：removed=${removed} · missing=${missing}`);
      await refreshToolDefinitions();
      await refreshMcpStatus();
    } finally {
      setBusy(false);
    }
  };

  const refreshMcpOauthCache = async () => {
    if (!server) return;
    setBusy(true);
    try {
      const result = await api.refreshMcpOauthTokens(server.id);
      setMcpNotice(result?.success ? "已刷新 OAuth token。" : "OAuth token 刷新完成。");
      await refreshToolDefinitions();
      await refreshMcpStatus();
    } finally {
      setBusy(false);
    }
  };

  const startMcpOauthLogin = async () => {
    if (!server) return;
    setBusy(true);
    try {
      const result = await api.startMcpOauthLogin(server.id);
      const authorizationUrl = String(result?.authorizationUrl ?? "");
      if (authorizationUrl) {
        window.open(authorizationUrl, "_blank", "noopener,noreferrer");
      }
      const listener = result?.callbackListener;
      const listenerText = listener?.listening ? "本地回调监听已启动，浏览器授权完成后会自动兑换 token。" : "本地回调监听未启动，请将回调 URL 或 code 粘贴到下方。";
      setMcpNotice(`已生成 OAuth 授权链接。${listenerText} redirect=${result?.redirectUri ?? "n/a"}`);
      await refreshMcpStatus();
    } finally {
      setBusy(false);
    }
  };

  const finishMcpOauthLogin = async () => {
    if (!server) return;
    const value = mcpOauthCallback.trim();
    if (!value) {
      setMcpNotice("请先粘贴 OAuth callback URL 或 code。");
      return;
    }
    setBusy(true);
    try {
      const result = await api.finishMcpOauthLogin(server.id, value);
      setMcpNotice(result?.success ? "OAuth 登录完成，token 已写入缓存。" : "OAuth 登录流程已完成。");
      setMcpOauthCallback("");
      await refreshToolDefinitions();
      await refreshMcpStatus();
    } finally {
      setBusy(false);
    }
  };

  const approvalModeHint = {
    risky: "默认允许非危险工具，仅拦截高风险或工具声明需要审批的调用；hardline 风险始终阻断",
    smart: "高风险调用先由辅助 LLM 判断；安全时自动放行，危险时阻断，不确定时等待确认",
    always: "所有外部工具调用都暂停等待确认；hardline 风险始终阻断",
    never: "默认允许外部工具直接执行；hardline 风险仍会阻断"
  }[toolApprovalMode] ?? "未知审批模式";

  return (
    <section className="primary-panel embedded-panel">
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
        <div className="panel-title-text"><Puzzle size={16} className="panel-title-icon" /><span>MCP</span><strong>MCP 扩展</strong></div>
      </div>
      {server ? (
        <div className="mcp-console">
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">服务器信息</div>
            {mcpNotice ? <p className="form-hint">{mcpNotice}</p> : null}
            {selectedMcpStatus ? (
              <div className="adapter-row trace-row" style={{ margin: "0 16px 12px" }}>
                <span className="row-icon indigo"><PlugZap size={17} /></span>
                <div className="adapter-info">
                  <strong>{selectedMcpStatus.name ?? selectedMcpStatus.id}</strong>
                  <small>
                    auth={selectedMcpStatus.auth ?? "none"} · oauth={selectedMcpStatus.oauthStatus?.state ?? "n/a"} · cache={selectedMcpStatus.oauthStatus?.tokenStatus?.cacheState ?? "n/a"}
                  </small>
                  <code>
                    refreshReady={String(selectedMcpStatus.oauthStatus?.tokenStatus?.refreshReady ?? false)} · risk={selectedMcpStatus.oauthStatus?.tokenStatus?.refreshRisk ?? "n/a"}
                  </code>
                  <code>
                    persistent={String(selectedMcpStatus.persistentSession?.active ?? false)} · calls={selectedMcpStatus.persistentSession?.calls ?? 0}
                  </code>
                  <code>
                    httpSession={String(selectedMcpStatus.httpSession?.active ?? false)} · tail={selectedMcpStatus.httpSession?.idTail ?? "n/a"}
                  </code>
                </div>
                <span className={`status-badge ${selectedMcpStatus.connected ? "enabled" : selectedMcpStatus.needsRefresh ? "warning" : "disabled"}`}>
                  {selectedMcpStatus.status ?? "unknown"}
                </span>
                <button className="btn-secondary" disabled={busy} onClick={() => void refreshMcpStatus()} type="button">
                  <RefreshCw size={15} />
                  刷新状态
                </button>
              </div>
            ) : null}
            <div className="form-group">
              <div className="form-row">
                <label>Server</label>
                <select value={server.id} onChange={(event) => setSelectedServerId(event.target.value)}>
                  {mcpServers.map((item) => (
                    <option key={item.id} value={item.id}>{item.name} · {item.id}</option>
                  ))}
                </select>
              </div>
            </div>
            <div className="form-group">
              <div className="form-row">
                <label>Command</label>
                <input readOnly value={`${server.command} ${server.args.join(" ")}`} />
              </div>
            </div>
            <div className="form-group">
              <div className="form-row">
                <label>Protocol</label>
                <select
                  value={server.protocol ?? "oneShotJson"}
                  onChange={(event) => void updateProtocol(event.target.value as "oneShotJson" | "mcpJsonRpc" | "mcpJsonRpcLine")}
                >
                  <option value="oneShotJson">oneShotJson</option>
                  <option value="mcpJsonRpc">mcpJsonRpc</option>
                  <option value="mcpJsonRpcLine">mcpJsonRpcLine</option>
                </select>
              </div>
            </div>
            <div className="form-group">
              <div className="form-row">
                <label>超时秒数</label>
                <input
                  min={1}
                  type="number"
                  value={server.timeoutSeconds}
                  onChange={(event) => void updateTimeout(Number(event.target.value))}
                />
              </div>
            </div>
            <div className="form-group">
              <label className="check-row">
                <input
                  checked={Boolean(server.supportsParallelToolCalls)}
                  onChange={() => void toggleParallelToolCalls()}
                  type="checkbox"
                />
                <span>允许并行工具调用</span>
              </label>
              <p className="form-hint">仅在该 MCP server 的工具彼此独立且线程安全时开启。</p>
            </div>
            <div className="form-group">
              <label className="check-row">
                <input
                  checked={Boolean(server.persistentSession)}
                  onChange={() => void togglePersistentSession()}
                  type="checkbox"
                />
                <span>复用 stdio JSON-RPC session</span>
              </label>
              <p className="form-hint">仅对 stdio MCP JSON-RPC 工具调用生效；配置变化或调用失败会自动重建 session。</p>
              <div className="inline-actions" style={{ paddingTop: 8 }}>
                <button
                  className="btn-secondary"
                  disabled={busy || (!server.persistentSession && !selectedMcpStatus?.persistentSession?.active)}
                  onClick={() => void resetPersistentSession()}
                  type="button"
                >
                  <RefreshCw size={15} />
                  重置 persistent session
                </button>
              </div>
            </div>
            <div className="inline-actions" style={{ padding: "0 16px 12px" }}>
              <button className="btn-secondary" disabled={busy} onClick={() => void startMcpOauthLogin()} type="button">
                <ExternalLink size={15} />
                OAuth 登录
              </button>
              <button className="btn-secondary" disabled={busy} onClick={() => void refreshMcpOauthCache()} type="button">
                <RefreshCw size={15} />
                刷新 OAuth
              </button>
              <button className="btn-secondary" disabled={busy} onClick={() => void clearMcpOauthCache()} type="button">
                <Trash2 size={15} />
                清理 OAuth 缓存
              </button>
            </div>
            <div className="form-group">
              <div className="form-row">
                <label>OAuth 回调</label>
                <input
                  placeholder="粘贴 callback URL 或 authorization code"
                  value={mcpOauthCallback}
                  onChange={(event) => setMcpOauthCallback(event.target.value)}
                />
                <button className="btn-secondary" disabled={busy || !mcpOauthCallback.trim()} onClick={() => void finishMcpOauthLogin()} type="button">
                  完成授权
                </button>
              </div>
            </div>
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">Browser / Computer Use Runtime</div>
            <div className="adapter-list">
              <div className="adapter-row trace-row">
                <span className="row-icon indigo"><Globe size={17} /></span>
                <div className="adapter-info">
                  <strong>{browserRuntime.title}</strong>
                  <small>{browserRuntime.detail}</small>
                  <code>{browserRuntime.raw}</code>
                </div>
                <button className="btn-secondary" disabled={busy} onClick={() => void refreshBrowserComputerRuntime()} type="button">
                  <RefreshCw size={15} />
                  刷新
                </button>
              </div>
              <div className="adapter-row trace-row">
                <span className="row-icon neutral"><Bot size={17} /></span>
                <div className="adapter-info">
                  <strong>{computerUseRuntime.title}</strong>
                  <small>{computerUseRuntime.detail}</small>
                  <code>{computerUseRuntime.raw}</code>
                </div>
              </div>
            </div>
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">Managed Processes</div>
            <div className="adapter-list">
              {visibleManagedProcesses.length === 0 ? (
                <p className="form-hint">暂无后台进程；terminal background/process start 会显示在这里。</p>
              ) : (
                visibleManagedProcesses.map((process) => {
                  const id = managedProcessId(process);
                  const status = process.status ?? "unknown";
                  const running = status === "running";
                  const stdoutTail = process.stdoutTail ?? process.stdout_tail ?? [];
                  const stderrTail = process.stderrTail ?? process.stderr_tail ?? [];
                  const latestLine = [...stderrTail.slice(-1), ...stdoutTail.slice(-1)].filter(Boolean).at(0);
                  return (
                    <div className="adapter-row trace-row" key={id}>
                      <span className="row-icon neutral"><Terminal size={17} /></span>
                      <div className="adapter-info">
                        <strong>{process.label || id}</strong>
                        <small>
                          {process.backend ?? "local"} · {process.envType ?? process.env_type ?? "local"} · {managedProcessWatchText(process)}
                        </small>
                        <code>
                          id={id} · run={managedProcessRunId(process) || "none"} · conversation={managedProcessConversationId(process) || "none"}
                        </code>
                        {latestLine ? <small>{String(latestLine)}</small> : null}
                      </div>
                      <span className={`status-badge ${running ? "enabled" : status === "exited" ? "disabled" : "warning"}`}>
                        {status}
                      </span>
                      {running ? (
                        <button className="btn-secondary" disabled={busy} onClick={() => void stopManagedProcess(id)} type="button">
                          停止
                        </button>
                      ) : null}
                    </div>
                  );
                })
              )}
            </div>
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">平台 Adapter</div>
            <div className="adapter-list">
              <div className="adapter-row trace-row">
                <span className="row-icon indigo"><PlugZap size={17} /></span>
                <div className="adapter-info">
                  <strong>Mattermost</strong>
                  <small>
                    {platformAdapterMode(mattermostAdapterState)} · {mattermostAdapterState?.transport ?? "websocket"} · received {mattermostAdapterState?.receivedCount ?? 0} · triggered {mattermostAdapterState?.triggeredCount ?? 0}
                  </small>
                  <small>{platformAdapterCapabilityText(mattermostAdapterState)}</small>
                  <code>
                    updatedAt={mattermostAdapterState?.updatedAt ?? "n/a"}
                  </code>
                  {mattermostAdapterState?.lastError ? (
                    <small>{mattermostAdapterState.lastError}</small>
                  ) : null}
                </div>
                <span className={`status-badge ${mattermostAdapterState?.status === "running" ? "enabled" : mattermostAdapterState?.status === "starting" || mattermostAdapterState?.status === "reconnecting" ? "warning" : "disabled"}`}>
                  {mattermostAdapterState?.status ?? "stopped"}
                </span>
                <button className="btn-secondary" disabled={busy} onClick={() => void refreshMattermostAdapter()} type="button">
                  <RefreshCw size={15} />
                  刷新
                </button>
                <button className="btn-secondary" disabled={busy || mattermostAdapterState?.status === "running" || mattermostAdapterState?.status === "starting"} onClick={() => void startMattermostAdapter()} type="button">
                  启动
                </button>
                <button className="btn-secondary" disabled={busy || mattermostAdapterState?.status === "stopped"} onClick={() => void stopMattermostAdapter()} type="button">
                  停止
                </button>
              </div>
              {platformAdapterStates.filter((adapter) => adapter.platform && adapter.platform !== "mattermost").map((adapter) => (
                <div className="adapter-row trace-row" key={adapter.platform}>
                  <span className="row-icon neutral"><PlugZap size={17} /></span>
                  <div className="adapter-info">
                    <strong>{adapter.platform}</strong>
                    <small>
                      {platformAdapterMode(adapter)} · {adapter.transport ?? "unknown"}
                    </small>
                    <small>{platformAdapterCapabilityText(adapter)}</small>
                    <code>
                      configured={adapter.configured ? "true" : "false"} · enabled={adapter.enabled ? "true" : "false"}
                    </code>
                  </div>
                  <span className={`status-badge ${adapter.configured ? "enabled" : "disabled"}`}>
                    {adapter.status ?? "unknown"}
                  </span>
                  {adapter.runtime ? (
                    <>
                      <button className="btn-secondary" disabled={busy || adapter.status === "running" || adapter.status === "starting"} onClick={() => void startPlatformAdapter(adapter.platform ?? "")} type="button">
                        启动
                      </button>
                      <button className="btn-secondary" disabled={busy || adapter.status === "stopped"} onClick={() => void stopPlatformAdapter(adapter.platform ?? "")} type="button">
                        停止
                      </button>
                    </>
                  ) : null}
                </div>
              ))}
            </div>
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">能力适配器</div>
            {capabilityAdapters.length === 0 ? (
              <p className="form-hint">暂无能力适配器</p>
            ) : (
              <div className="adapter-list">
                {capabilityAdapters.map((adapter) => (
                  <button className="adapter-row" key={adapter.name} onClick={() => void toggleAdapter(adapter)} type="button">
                    <span className="row-icon indigo"><PlugZap size={17} /></span>
                    <div className="adapter-info">
                      <strong>{adapter.name}</strong>
                      <small>{adapter.mcpServer === "__builtin" ? "内置能力" : `${adapter.mcpServer} · ${adapter.mcpTool}`}</small>
                    </div>
                    <span className={`status-badge ${adapter.enabled ? "enabled" : "disabled"}`}>
                      {adapter.enabled ? "已启用" : "未启用"}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">工具注册表</div>
            <div className="inline-actions" style={{ padding: "0 16px 10px" }}>
              <button className="btn-secondary" onClick={refreshRegistryFromServers} type="button" disabled={busy}>
                <RefreshCw size={16} />
                从 MCP 刷新
              </button>
            </div>
            {toolDefinitions.length === 0 ? (
              <p className="form-hint">暂无注册工具</p>
            ) : (
              <div className="tool-tags">
                {toolDefinitions.map((tool) => (
                  <button
                    className="tool-tag-btn"
                    key={tool.name}
                    onClick={() => {
                      setSelectedServerId(tool.serverId === "capability" ? selectedServerId : tool.serverId);
                      setToolName(tool.source === "capability" ? tool.name : tool.toolName);
                      setPayload("{}");
                    }}
                    title={tool.description}
                    type="button"
                  >
                    {tool.name}
                  </button>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">运行中 Agent</div>
            {activeRuns.length === 0 ? (
              <p className="form-hint">暂无运行中的 agent run</p>
            ) : (
              <div className="adapter-list">
                {activeRuns.map((run) => (
                  <div className="adapter-row trace-row" key={`active-${run.runId}`}>
                    <span className="status-badge warning">{run.state}</span>
                    <div className="adapter-info">
                      <strong>{run.agentId} · {run.runId}</strong>
                      <small>{run.userRequest ? run.userRequest.slice(0, 140) : "正在执行任务"}</small>
                      <small>最近活动：{latestRunActivity(run)}</small>
                      <code>{run.conversationId}</code>
                    </div>
                    <button className="btn-secondary" disabled={busy} onClick={() => void abortRun(run.runId)} type="button">
                      <XCircle size={15} />
                      中止
                    </button>
                    <button className="btn-secondary" disabled={busy} onClick={() => void exportRunBundle(run.runId)} type="button">
                      导出
                    </button>
                    <button className="btn-secondary" disabled={busy} onClick={() => void diagnoseRun(run.runId)} type="button">
                      诊断
                    </button>
                  </div>
                ))}
              </div>
            )}
            {exportedBundlePath ? <p className="form-hint">最近导出：{exportedBundlePath}</p> : null}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">子智能体运行</div>
            {childAgentRuns.length === 0 ? (
              <p className="form-hint">暂无子智能体运行记录</p>
            ) : (
              <div className="adapter-list">
                {childAgentRunGroups.map((group) => (
                  <div className="trace-row" key={`child-group-${group.parentRunId}`} style={{ alignItems: "stretch", flexDirection: "column" }}>
                    <div className="adapter-info">
                      <strong>{group.parentRun?.agentId ?? "parent"} · {group.parentRunId}</strong>
                      <small>{group.parentRun?.userRequest || "父级运行记录未在列表中"}</small>
                      <code>{group.runs.length} child run(s) · updated {new Date(group.updatedAt).toLocaleString()}</code>
                    </div>
                    <div className="adapter-list" style={{ marginTop: 8 }}>
                      {group.runs.map((run) => (
                        <div className="adapter-row trace-row" key={`child-${run.runId}`}>
                          <span className={`status-badge ${runStatusClass(run.state)}`}>{run.state}</span>
                          <div className="adapter-info">
                            <strong>
                              {run.subagentRole ?? "leaf"} · {run.subagentCanDelegate ? "delegate" : "leaf"} · depth {run.subagentDepth ?? 1} · #
                              {run.subagentIndex ?? "-"} · {run.runId}
                            </strong>
                            <small>{run.subagentTask || run.userRequest || "子任务执行"}</small>
                            {run.subagentToolsets?.length ? <code>{run.subagentToolsets.join(", ")}</code> : <code>default leaf scope</code>}
                          </div>
                          {isActiveRunState(run.state) ? (
                            <button className="btn-secondary" disabled={busy} onClick={() => void abortRun(run.runId)} type="button">
                              <XCircle size={15} />
                              中止
                            </button>
                          ) : null}
                          <button className="btn-secondary" disabled={busy} onClick={() => void exportRunBundle(run.runId)} type="button">
                            导出
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void diagnoseRun(run.runId)} type="button">
                            诊断
                          </button>
                        </div>
                      ))}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">Agent 控制命令</div>
            {groupedControlCommands.length === 0 ? (
              <p className="form-hint">暂无控制命令</p>
            ) : (
              <div className="adapter-list">
                {groupedControlCommands.map((group) => (
                  <div className="adapter-row trace-row" key={group.category}>
                    <span className="status-badge enabled">{group.category}</span>
                    <div className="adapter-info">
                      {group.commands.map((command) => {
                        const primary = `/${command.name}${command.argsHint ? ` ${command.argsHint}` : ""}`;
                        const aliases = command.aliases.map((alias) => `/${alias}`).join(" · ");
                        return (
                          <div key={command.name}>
                            <strong>{primary}</strong>
                            <small>{command.description}</small>
                            {aliases ? <code>{aliases}</code> : null}
                          </div>
                        );
                      })}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">Agent Todo</div>
            {visibleTodos.length === 0 ? (
              <p className="form-hint">暂无运行中 todo</p>
            ) : (
              <div className="adapter-list">
                {visibleTodos.map((todo) => (
                  <div className="adapter-row trace-row" key={todo.id}>
                    <span className={`status-badge ${todo.status === "completed" ? "enabled" : todo.status === "blocked" ? "disabled" : "warning"}`}>{todo.status}</span>
                    <div className="adapter-info">
                      <strong>{todo.content}</strong>
                      <small>{todo.runId}</small>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">
              <span>Agent 请求队列</span>
              <div className="row-actions">
                <button className="btn-secondary" disabled={busy} onClick={() => void clearFinishedQueueItems()} type="button">
                  清理终态
                </button>
                <button className="btn-secondary" disabled={busy} onClick={() => void drainQueue()} type="button">
                  <RefreshCw size={14} />
                  Drain
                </button>
              </div>
            </div>
            {visibleQueue.length === 0 ? (
              <p className="form-hint">暂无排队请求</p>
            ) : (
              <div className="adapter-list">
                {visibleQueue.map((item) => (
                  <div className="adapter-row trace-row" key={item.id}>
                    <span className={`status-badge ${["failed", "canceled"].includes(item.status) ? "disabled" : "warning"}`}>{item.status}</span>
                    <div className="adapter-info">
                      <strong>{item.personaId} · {item.id}</strong>
                      <small>{item.content.slice(0, 140)}</small>
                      {item.error ? <small>{item.error}</small> : null}
                      <code>{item.conversationId}</code>
                    </div>
                    {item.status === "pending" ? (
                      <button className="btn-secondary" disabled={busy} onClick={() => void cancelQueueItem(item.id)} type="button">
                        <XCircle size={14} />
                        取消
                      </button>
                    ) : null}
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">
              <span>计划 Agent 任务</span>
              <button className="btn-secondary" disabled={busy} onClick={() => void tickScheduledJobs()} type="button">
                <RefreshCw size={14} />
                Tick
              </button>
            </div>
            <div className="settings-form">
              <div className="form-group">
                <div className="form-row">
                  <label>名称</label>
                  <input value={scheduledName} onChange={(event) => setScheduledName(event.target.value)} placeholder="可选" />
                </div>
              </div>
              <div className="form-group">
                <label>Prompt</label>
                <textarea value={scheduledPrompt} onChange={(event) => setScheduledPrompt(event.target.value)} />
              </div>
              <div className="form-group">
                <div className="form-row">
                  <label>计划</label>
                  <select value={scheduledKind} onChange={(event) => setScheduledKind(event.target.value as "once" | "interval" | "cron")}>
                    <option value="once">一次</option>
                    <option value="interval">循环</option>
                    <option value="cron">Cron</option>
                  </select>
                </div>
              </div>
              {scheduledKind === "once" ? (
                <div className="form-group">
                  <div className="form-row">
                    <label>执行时间</label>
                    <input type="datetime-local" value={scheduledRunAt} onChange={(event) => setScheduledRunAt(event.target.value)} />
                  </div>
                </div>
              ) : scheduledKind === "interval" ? (
                <div className="form-group">
                  <div className="form-row">
                    <label>间隔分钟</label>
                    <input min={1} type="number" value={scheduledInterval} onChange={(event) => setScheduledInterval(Number(event.target.value))} />
                  </div>
                </div>
              ) : (
                <div className="form-group">
                  <div className="form-row">
                    <label>Cron</label>
                    <input value={scheduledCronExpr} onChange={(event) => setScheduledCronExpr(event.target.value)} placeholder="0 9 * * 1-5" />
                  </div>
                </div>
              )}
              <div className="form-group">
                <div className="form-row">
                  <label>允许 Toolsets</label>
                  <input value={scheduledEnabledToolsets} onChange={(event) => setScheduledEnabledToolsets(event.target.value)} placeholder="留空继承 Agent，例如 web,browser" />
                </div>
              </div>
              <div className="form-group">
                <div className="form-row">
                  <label>禁用 Toolsets</label>
                  <input value={scheduledDisabledToolsets} onChange={(event) => setScheduledDisabledToolsets(event.target.value)} placeholder="cronjob,clarify" />
                </div>
              </div>
              <div className="inline-actions" style={{ padding: "0 16px 12px" }}>
                <button className="btn-primary" disabled={busy || !scheduledPrompt.trim()} onClick={() => void createScheduledJob()} type="button">
                  <Plus size={15} />
                  新建
                </button>
              </div>
            </div>
            {visibleScheduledJobs.length === 0 ? (
              <p className="form-hint">暂无计划任务</p>
            ) : (
              <div className="adapter-list">
                {visibleScheduledJobs.map((job) => {
                  const expanded = expandedScheduledOutputJobId === job.id;
                  const outputs = scheduledOutputsByJob[job.id] ?? [];
                  return (
                    <div className="adapter-row trace-row" key={job.id} style={{ flexDirection: "column", alignItems: "stretch" }}>
                      <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
                        <span className={`status-badge ${job.enabled ? "enabled" : "disabled"}`}>{job.status}</span>
                        <div className="adapter-info" style={{ flex: 1 }}>
                          <strong>{job.name || job.id}</strong>
                          <small>{job.scheduleKind === "interval" ? `每 ${job.intervalMinutes ?? "?"} 分钟` : job.scheduleKind === "cron" ? `cron · ${job.cronExpr ?? "未设置"}` : `一次 · ${job.runAt ?? "未设置"}`}</small>
                          <small>{job.prompt.slice(0, 140)}</small>
                          <code>{job.nextRunAt ? `next ${job.nextRunAt}` : `runs ${job.runCount}`}</code>
                          {(job.enabledToolsets.length || job.disabledToolsets.length) ? (
                            <small>Toolsets：{job.enabledToolsets.length ? `允许 ${job.enabledToolsets.join(",")}` : "继承 Agent"} · 禁用 {job.disabledToolsets.join(",") || "-"}</small>
                          ) : null}
                          {job.lastRunStatus ? <small>最近运行：{job.lastRunStatus} · {job.lastCompletedAt ?? "-"}</small> : null}
                          {job.lastError ? <small>{job.lastError}</small> : null}
                          {job.lastOutput ? <small>{job.lastOutput.slice(0, 180)}</small> : null}
                          {job.lastOutputPath ? <code>{job.lastOutputPath}</code> : null}
                        </div>
                        <div className="inline-actions">
                          {job.lastOutputPath ? (
                            <button className="btn-secondary" disabled={busy} onClick={() => void api.openLocalFile(job.lastOutputPath || "")} type="button">
                              打开输出
                            </button>
                          ) : null}
                          <button className="btn-secondary" disabled={busy} onClick={() => void toggleScheduledOutputs(job.id)} type="button">
                            {expanded ? "收起历史" : "输出历史"}
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void toggleScheduledJob(job)} type="button">
                            {job.enabled ? "暂停" : "启用"}
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void deleteScheduledJob(job.id)} title="删除" type="button">
                            <Trash2 size={14} />
                          </button>
                        </div>
                      </div>
                      {expanded ? (
                        <div className="adapter-list" style={{ marginTop: 10 }}>
                          {outputs.length === 0 ? (
                            <p className="form-hint">暂无历史输出</p>
                          ) : outputs.map((output) => (
                            <div className="adapter-row trace-row" key={output.path}>
                              <span className={`status-badge ${output.status === "completed" ? "enabled" : "disabled"}`}>{output.status}</span>
                              <div className="adapter-info">
                                <strong>{output.fileName}</strong>
                                <small>{new Date(output.modifiedAt).toLocaleString()} · {Math.max(1, Math.ceil(output.sizeBytes / 1024))} KB</small>
                                <code>{output.path}</code>
                              </div>
                              <button className="btn-secondary" disabled={busy} onClick={() => void api.openLocalFile(output.path)} type="button">
                                打开
                              </button>
                            </div>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">状态快照</div>
            <div className="inline-actions" style={{ marginBottom: 10 }}>
              <input
                value={snapshotLabel}
                onChange={(event) => setSnapshotLabel(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void createSnapshot();
                  }
                }}
                placeholder="快照标签"
              />
              <button className="btn-secondary" disabled={busy} onClick={() => void createSnapshot()} type="button">
                创建
              </button>
            </div>
            <div className="inline-actions" style={{ marginBottom: 10 }}>
              <label style={{ fontSize: 12, color: "var(--text-muted)" }}>保留</label>
              <input
                min={1}
                max={50}
                type="number"
                value={snapshotKeep}
                onChange={(event) => setSnapshotKeep(Math.max(1, Number(event.target.value) || 1))}
                style={{ maxWidth: 90 }}
              />
              <button className="btn-secondary" disabled={busy} onClick={() => void pruneSnapshots()} type="button">
                裁剪旧快照
              </button>
              <button className="btn-secondary" disabled={busy} onClick={() => void refreshStateSnapshots()} type="button">
                刷新
              </button>
            </div>
            {snapshotNotice ? <p className="form-hint">{snapshotNotice}</p> : null}
            {stateSnapshots.length === 0 ? (
              <p className="form-hint">暂无状态快照</p>
            ) : (
              <div className="adapter-list">
                {stateSnapshots.map((snapshot) => (
                  <div className="adapter-row trace-row" key={snapshot.id}>
                    <span className="status-badge enabled">snapshot</span>
                    <div className="adapter-info">
                      <strong>{snapshot.label || snapshot.id}</strong>
                      <small>{snapshot.createdAt ? new Date(snapshot.createdAt).toLocaleString() : snapshot.id}</small>
                      {snapshot.statePath ? <code>{snapshot.statePath}</code> : null}
                    </div>
                    <div className="inline-actions">
                      {snapshot.statePath ? (
                        <button className="btn-secondary" disabled={busy} onClick={() => void api.revealLocalFile(snapshot.statePath || "")} type="button">
                          定位
                        </button>
                      ) : null}
                      <button className="btn-secondary" disabled={busy} onClick={() => void restoreSnapshot(snapshot.id)} type="button">
                        恢复
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">Workspace 快照</div>
            <div className="inline-actions" style={{ marginBottom: 10 }}>
              <input
                value={workspaceSnapshotLabel}
                onChange={(event) => setWorkspaceSnapshotLabel(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void createWorkspaceSnapshot();
                  }
                }}
                placeholder="workspace 快照标签"
              />
              <button className="btn-secondary" disabled={busy} onClick={() => void createWorkspaceSnapshot()} type="button">
                创建
              </button>
              <button className="btn-secondary" disabled={busy} onClick={() => void refreshWorkspaceSnapshots()} type="button">
                刷新
              </button>
            </div>
            <label className="checkbox-row" style={{ marginBottom: 10 }}>
              <input
                type="checkbox"
                checked={deleteNewWorkspaceFiles}
                onChange={(event) => setDeleteNewWorkspaceFiles(event.target.checked)}
              />
              <span>恢复时删除快照后新增的非排除文件</span>
            </label>
            {workspaceSnapshots.length === 0 ? (
              <p className="form-hint">暂无 workspace 快照</p>
            ) : (
              <div className="adapter-list">
                {workspaceSnapshots.map((snapshot) => (
                  <div className="adapter-row trace-row" key={snapshot.id}>
                    <span className={`status-badge ${snapshot.truncated ? "disabled" : "enabled"}`}>
                      {snapshot.truncated ? "truncated" : "workspace"}
                    </span>
                    <div className="adapter-info">
                      <strong>{snapshot.label || snapshot.id}</strong>
                      <small>
                        {snapshot.createdAt ? new Date(snapshot.createdAt).toLocaleString() : snapshot.id}
                        {" · files "}
                        {snapshot.fileCount ?? 0}
                        {" · skipped "}
                        {snapshot.skippedFiles ?? 0}/{snapshot.skippedDirs ?? 0}
                      </small>
                      {snapshot.root ? <code>{snapshot.root}</code> : null}
                    </div>
                    <div className="inline-actions">
                      {snapshot.snapshotPath ? (
                        <button className="btn-secondary" disabled={busy} onClick={() => void api.revealLocalFile(snapshot.snapshotPath || "")} type="button">
                          定位
                        </button>
                      ) : null}
                      <button className="btn-secondary" disabled={busy} onClick={() => void restoreWorkspaceSnapshot(snapshot.id)} type="button">
                        恢复
                      </button>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">工具审批策略</div>
            <div className="form-group">
              <div className="form-row">
                <label>审批模式</label>
                <select
                  disabled={busy}
                  value={toolApprovalMode}
                  onChange={(event) => void updateToolApprovalMode(event.target.value)}
                >
                  <option value="risky">仅高风险</option>
                  <option value="smart">智能审批</option>
                  <option value="always">全部审批</option>
                  <option value="never">默认允许（never）</option>
                </select>
              </div>
              <p className="form-hint">{approvalModeHint}</p>
            </div>
            <div className="form-group">
              <label className="checkbox-row">
                <input
                  type="checkbox"
                  checked={toolMutationCheckpointEnabled}
                  disabled={busy}
                  onChange={(event) => void updateToolMutationCheckpoint(event.target.checked)}
                />
                写入/执行类工具调用前自动创建 state + workspace 快照
              </label>
              <p className="form-hint">开启后，可能修改文件或状态的工具会先生成恢复点；读取、搜索和页面快照类工具不会额外创建。</p>
            </div>
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">可信工具规则</div>
            <div className="inline-actions" style={{ marginBottom: 10 }}>
              <input
                value={trustedToolPatternDraft}
                onChange={(event) => setTrustedToolPatternDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void addTrustedTool();
                  }
                }}
                placeholder="server.tool、server.* 或 *"
              />
              <button className="btn-secondary" disabled={busy} onClick={() => void addTrustedTool()} type="button">
                添加
              </button>
            </div>
            <p className="form-hint">用于跳过后续审批；常用格式为当前工具、整个服务器或全部外部工具。hardline 风险不会被可信规则绕过。</p>
            {trustedToolPatterns.length === 0 ? (
              <p className="form-hint">暂无可信工具规则</p>
            ) : (
              <div className="adapter-list">
                {trustedToolPatterns.map((pattern) => (
                  <div className="adapter-row trace-row" key={pattern}>
                    <span className="status-badge enabled">trusted</span>
                    <div className="adapter-info">
                      <strong>{pattern}</strong>
                      <small>匹配后续工具调用时跳过审批</small>
                    </div>
                    <button className="btn-secondary" disabled={busy} onClick={() => void removeTrustedTool(pattern)} type="button">
                      移除
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">可信命令规则</div>
            <div className="inline-actions" style={{ marginBottom: 10 }}>
              <input
                value={trustedCommandPatternDraft}
                onChange={(event) => setTrustedCommandPatternDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void addTrustedCommand();
                  }
                }}
                placeholder="命令文本或带 * 的模式"
              />
              <button className="btn-secondary" disabled={busy} onClick={() => void addTrustedCommand()} type="button">
                添加
              </button>
            </div>
            <p className="form-hint">只匹配 terminal、process start/run、execute_code 的命令文本。hardline 风险不会被可信规则绕过。</p>
            {trustedCommandPatterns.length === 0 ? (
              <p className="form-hint">暂无可信命令规则</p>
            ) : (
              <div className="adapter-list">
                {trustedCommandPatterns.map((pattern) => (
                  <div className="adapter-row trace-row" key={pattern}>
                    <span className="status-badge enabled">trusted</span>
                    <div className="adapter-info">
                      <strong>{pattern}</strong>
                      <small>匹配后续命令调用时跳过审批</small>
                    </div>
                    <button className="btn-secondary" disabled={busy} onClick={() => void removeTrustedCommand(pattern)} type="button">
                      移除
                    </button>
                  </div>
                ))}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">待审批工具调用</div>
            {pendingApprovals.length === 0 ? (
              <p className="form-hint">暂无待审批调用</p>
            ) : (
              <div className="adapter-list">
                {pendingApprovals.map((approval) => (
                  <div className="adapter-row trace-row" key={approval.id} style={{ flexDirection: "column", alignItems: "stretch" }}>
                    <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
                    <span className="status-badge disabled">待审批</span>
                    <div className="adapter-info" style={{ flex: 1 }}>
                      <strong>{approval.serverId} · {approval.toolName}</strong>
                      <small>审批原因：{approval.reason}</small>
                      <small>{approval.runId ? `run ${approval.runId}` : "无关联 run"} · {new Date(approval.createdAt).toLocaleString()}</small>
                    </div>
                    <div className="inline-actions">
                      <button className="btn-secondary" disabled={busy} onClick={() => void deny(approval.id)} type="button">拒绝</button>
                      <button className="btn-secondary" disabled={busy} onClick={() => void approveAlways(approval.id)} type="button">批准并记住</button>
                      <button className="btn-secondary" disabled={busy} onClick={() => void approveServer(approval.id)} type="button">信任服务器</button>
                      <button disabled={busy} onClick={() => void approve(approval.id)} type="button">批准</button>
                    </div>
                    </div>
                    <code style={{ marginTop: 8 }}>{compactDetail(approval.payload, 900)}</code>
                  </div>
                ))}
              </div>
            )}
            {recentApprovalHistory.length > 0 ? (
              <div className="adapter-list" style={{ marginTop: 12 }}>
                {recentApprovalHistory.map((approval) => (
                  <div className="adapter-row trace-row" key={`approval-history-${approval.id}`}>
                    <span className={`status-badge ${approval.status === "completed" || approval.status === "approved" ? "enabled" : "disabled"}`}>{approval.status}</span>
                    <div className="adapter-info">
                      <strong>{approval.serverId} · {approval.toolName}</strong>
                      <small>{approval.reason}</small>
                      <small>{new Date(approval.updatedAt).toLocaleString()}{approval.runId ? ` · run ${approval.runId}` : ""}</small>
                      {approval.error ? <small>{approval.error}</small> : null}
                    </div>
                    <code>{compactDetail(approval.result ?? approval.payload, 260)}</code>
                  </div>
                ))}
              </div>
            ) : null}
            {rerunnableRuns.length > 0 ? (
              <div className="adapter-list" style={{ marginTop: 10 }}>
                {rerunnableRuns.map((run) => (
                  <div className="adapter-row trace-row" key={`rerun-${run.runId}`}>
                    <span className={`status-badge ${run.state === "completed" ? "enabled" : "disabled"}`}>{run.state}</span>
                    <div className="adapter-info">
                      <strong>{run.agentId} · {run.runId}</strong>
                      <small>{run.userRequest ? run.userRequest.slice(0, 140) : run.error ?? "基于原用户请求创建新的 run"}</small>
                    </div>
                    <button className="btn-secondary" disabled={busy} onClick={() => void rerunRun(run.runId)} type="button">
                      重新执行
                    </button>
                    <button className="btn-secondary" disabled={busy} onClick={() => void exportRunBundle(run.runId)} type="button">
                      导出
                    </button>
                    <button className="btn-secondary" disabled={busy} onClick={() => void diagnoseRun(run.runId)} type="button">
                      诊断
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">最近 Agent Run 复盘</div>
            {recentRuns.length === 0 ? (
              <p className="form-hint">暂无 agent run</p>
            ) : (
              <div className="adapter-list">
                {recentRuns.map((run) => {
                  const expanded = expandedDiagnosticRunId === run.runId;
                  const latestCheckpoint = run.checkpoints?.[run.checkpoints.length - 1];
                  const runArtifacts = artifactsByRun[run.runId] ?? [];
                  return (
                    <div className="adapter-row trace-row" key={`diagnostic-${run.runId}`} style={{ flexDirection: "column", alignItems: "stretch" }}>
                      <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
                        <span className={`status-badge ${runStatusClass(run.state)}`}>{run.state}</span>
                        <div className="adapter-info" style={{ flex: 1 }}>
                          <strong>{run.agentId} · {run.runId}</strong>
                          <small>{latestRunSignal(run)}</small>
                          <small>最近活动：{latestRunActivity(run)}</small>
                          <small>
                            phases {run.phaseEvents?.length ?? 0} · tools {run.toolEvents?.length ?? 0} · checkpoints {run.checkpoints?.length ?? 0}
                          </small>
                          <code>{new Date(run.updatedAt).toLocaleString()} · {run.conversationId}</code>
                        </div>
                        <div className="inline-actions">
                          <button className="btn-secondary" disabled={busy} onClick={() => setExpandedDiagnosticRunId(expanded ? null : run.runId)} type="button">
                            {expanded ? "收起" : "展开"}
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void exportRunBundle(run.runId)} type="button">
                            导出
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void diagnoseRun(run.runId)} type="button">
                            诊断
                          </button>
                        </div>
                      </div>
                      {expanded ? (
                        <div className="adapter-list" style={{ marginTop: 10 }}>
                          {run.error ? (
                            <div className="adapter-row trace-row">
                              <span className="status-badge disabled">error</span>
                              <div className="adapter-info">
                                <strong>运行错误</strong>
                                <small>{run.error}</small>
                              </div>
                            </div>
                          ) : null}
                          {latestCheckpoint ? (
                            <div className="adapter-row trace-row">
                              <span className="status-badge enabled">checkpoint</span>
                              <div className="adapter-info">
                                <strong>{latestCheckpoint.state} · iteration {latestCheckpoint.iteration}</strong>
                                <small>{latestCheckpoint.summary}</small>
                                <code>{latestCheckpoint.checkpointId}</code>
                              </div>
                            </div>
                          ) : null}
                          {(run.phaseEvents ?? []).slice(-5).reverse().map((phase, index) => {
                            const isFailover = phase.phase === "llm_failover";
                            const isSubagentFailure = phase.phase === "subagent_failed";
                            const badge = isFailover ? "fallback" : isSubagentFailure ? "subagent" : phase.phase;
                            const title = isFailover ? "LLM provider fallback" : isSubagentFailure ? "Subagent failure" : new Date(phase.updatedAt).toLocaleString();
                            const detail = isFailover ? llmFailoverSummary(phase.detail) : isSubagentFailure ? subagentFailureSummary(phase.detail) : compactDetail(phase.detail) || "无详情";
                            return (
                              <div className="adapter-row trace-row" key={`${run.runId}-phase-${phase.updatedAt}-${index}`}>
                                <span className={`status-badge ${isFailover || isSubagentFailure ? "disabled" : "warning"}`}>{badge}</span>
                                <div className="adapter-info">
                                  <strong>{title}</strong>
                                  <small>{detail}</small>
                                  {isFailover || isSubagentFailure ? <code>{new Date(phase.updatedAt).toLocaleString()}</code> : null}
                                </div>
                              </div>
                            );
                          })}
                          {runArtifacts.slice(0, 5).map((artifact) => (
                            <div className="adapter-row trace-row" key={`${run.runId}-artifact-${artifact.path}`}>
                              <span className="status-badge enabled">artifact</span>
                              <div className="adapter-info">
                                <strong>{artifact.fileName || "tool output"}</strong>
                                <small>
                                  {formatBytes(artifact.sizeBytes)}
                                  {artifact.modifiedAt ? ` · ${new Date(artifact.modifiedAt).toLocaleString()}` : ""}
                                </small>
                                <code>{artifact.path}</code>
                                {artifact.contentPreview ? <small>{artifact.contentPreview.slice(0, 220)}</small> : null}
                              </div>
                              <div className="inline-actions">
                                <button className="btn-secondary" disabled={busy} onClick={() => void api.openLocalFile(artifact.path)} type="button">
                                  打开
                                </button>
                                <button className="btn-secondary" disabled={busy} onClick={() => void api.revealLocalFile(artifact.path)} type="button">
                                  定位
                                </button>
                              </div>
                            </div>
                          ))}
                          {(run.toolEvents ?? []).slice(-5).reverse().map((event, index) => (
                            <div className="adapter-row trace-row" key={`${run.runId}-tool-${event.callId ?? event.referenceId ?? index}`}>
                              <span className={`status-badge ${event.ok ? "enabled" : "disabled"}`}>{event.ok ? "ok" : "fail"}</span>
                              <div className="adapter-info">
                                <strong>{event.serverId} · {event.toolName}</strong>
                                <small>{event.summary || event.error || event.title}</small>
                                {event.error ? <small>{event.error}</small> : null}
                                <code>{event.elapsedMs}ms{event.checkpointId ? ` · ${event.checkpointId}` : ""}</code>
                              </div>
                            </div>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">Agent Run 恢复</div>
            {resumableRuns.length === 0 ? (
              <p className="form-hint">{approvalBlockedRuns.length > 0 ? "有运行正在等待工具审批，请先处理待审批调用" : "暂无可恢复运行"}</p>
            ) : (
              <div className="adapter-list">
                {resumableRuns.map((run) => {
                  const latestCheckpoint = run.checkpoints?.[run.checkpoints.length - 1];
                  const isExpanded = expandedRunId === run.runId;
                  return (
                    <div className="adapter-row trace-row" key={run.runId} style={{ flexDirection: "column", alignItems: "stretch" }}>
                      <div style={{ display: "flex", gap: 10, alignItems: "center" }}>
                        <span className={`status-badge ${run.state === "pendingApproval" ? "disabled" : "warning"}`}>
                          {run.state}
                        </span>
                        <div className="adapter-info" style={{ flex: 1 }}>
                          <strong>{run.agentId} · {run.runId}</strong>
                          <small>{latestCheckpoint ? `${latestCheckpoint.state}: ${latestCheckpoint.summary}` : run.error ?? "等待恢复"}</small>
                          {run.userRequest ? <small>{run.userRequest.slice(0, 120)}</small> : null}
                          <code>{run.conversationId}</code>
                        </div>
                        <div className="inline-actions">
                          <button className="btn-secondary" disabled={busy || !run.checkpoints?.length} onClick={() => setExpandedRunId(isExpanded ? null : run.runId)} type="button">
                            {isExpanded ? "收起" : "Checkpoint"}
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void resumeRun(run.runId)} type="button">
                            <RefreshCw size={15} />
                            最新恢复
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void exportRunBundle(run.runId)} type="button">
                            导出
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void diagnoseRun(run.runId)} type="button">
                            诊断
                          </button>
                        </div>
                      </div>
                      {isExpanded && run.checkpoints?.length ? (
                        <div className="adapter-list" style={{ marginTop: 10 }}>
                          {run.checkpoints.slice().reverse().map((checkpoint) => (
                            <div className="adapter-row trace-row" key={checkpoint.checkpointId}>
                              <span className="status-badge enabled">{checkpoint.iteration}</span>
                              <div className="adapter-info">
                                <strong>{checkpoint.state}</strong>
                                <small>{checkpoint.summary}</small>
                                <code>{checkpoint.checkpointId}</code>
                              </div>
                              <button className="btn-secondary" disabled={busy} onClick={() => void resumeRun(run.runId, checkpoint.checkpointId)} type="button">
                                从此恢复
                              </button>
                            </div>
                          ))}
                        </div>
                      ) : null}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
          <div className="card" style={{ margin: "0 16px 12px" }}>
            <div className="card-header">工具调用</div>
            <div className="form-group">
              <div className="form-row">
                <label>工具名称</label>
                <input value={toolName} onChange={(event) => setToolName(event.target.value)} />
              </div>
            </div>
            <div className="form-group">
              <label>Payload JSON</label>
              <textarea value={payload} onChange={(event) => setPayload(event.target.value)} />
            </div>
            <div className="inline-actions">
              <button onClick={runTool} type="button" disabled={busy}>
                <PlugZap size={16} />
                {busy ? "调用中..." : "调用工具"}
              </button>
              <button className="btn-secondary" onClick={discoverTools} type="button" disabled={busy}>
                <Search size={16} />
                发现工具
              </button>
            </div>
          </div>
        </div>
      ) : (
        <div className="empty-state compact">
          <div className="empty-icon-wrap"><PlugZap size={48} strokeWidth={1.5} /></div>
          <p>暂无 MCP server</p>
          <span className="form-hint">配置文件会自动创建一个 Echo JSON 示例</span>
        </div>
      )}
      {lastMcpToolsResult ? (
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header">工具列表</div>
          <div className="tool-tags">
            {lastMcpToolsResult.tools.length === 0 ? (
              <span className="form-hint">{lastMcpToolsResult.error ?? "没有发现工具"}</span>
            ) : (
              lastMcpToolsResult.tools.map((tool) => (
                <button className="tool-tag-btn" key={tool.name} onClick={() => setToolName(tool.name)} type="button">
                  {tool.name}
                </button>
              ))
            )}
          </div>
        </div>
      ) : null}
      {lastMcpResult ? (
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header">调用结果</div>
          <pre className="result-preview">{JSON.stringify(lastMcpResult, null, 2)}</pre>
        </div>
      ) : (
        <div className="card" style={{ margin: "0 16px 12px" }}>
          <div className="card-header">调用结果</div>
          <p className="form-hint">还没有工具调用结果</p>
        </div>
      )}
      <div className="card" style={{ margin: "0 16px 12px" }}>
        <div className="card-header">最近工具 Trace</div>
        {traces.length === 0 ? (
          <p className="form-hint">暂无工具调用记录</p>
        ) : (
          <div className="adapter-list">
            {traces.map((trace) => (
              <div className="adapter-row trace-row" key={trace.id}>
                <span className={`status-badge ${trace.ok ? "enabled" : "disabled"}`}>
                  {trace.ok ? "成功" : trace.timedOut ? "超时" : "失败"}
                </span>
                <div className="adapter-info">
                  <strong>{trace.serverId} · {trace.toolName}</strong>
                  <small>{trace.event.summary}</small>
                  {trace.event.path ? <code>{trace.event.path}</code> : null}
                </div>
                <small>{trace.elapsedMs}ms</small>
              </div>
            ))}
          </div>
        )}
      </div>
    </section>
  );
}

export function AgentsPanel() {
  const {
    agents,
    mcpServers,
    skills,
    llmProviders,
    refreshAgents,
    saveAgent,
    deleteAgent,
    goBack
  } = useAppStore();
  const [selectedId, setSelectedId] = useState("");
  const [draft, setDraft] = useState<AgentDefinition | null>(null);
  const [saving, setSaving] = useState(false);
  const [search, setSearch] = useState("");
  const [skillSearch, setSkillSearch] = useState("");
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);
  const [plannerTraces, setPlannerTraces] = useState<PlannerTraceRecord[]>([]);
  const [routerTraces, setRouterTraces] = useState<ToolRouterTraceRecord[]>([]);

  const didInitRef = useRef(false);
  useEffect(() => {
    if (!didInitRef.current && agents.length > 0) {
      didInitRef.current = true;
      if (!selectedId && agents[0]) {
        setSelectedId(agents[0].id);
        setDraft({ ...agents[0] });
      }
    }
  }, [agents, selectedId]);

  useEffect(() => {
    const agent = agents.find((a) => a.id === selectedId);
    if (agent) setDraft({ ...agent });
  }, [selectedId, agents]);

  useEffect(() => {
    const provider = llmProviders.find((item) => item.id === draft?.llmProvider);
    if (!provider) {
      setCatalogModels([]);
      return;
    }
    let cancelled = false;
    api.detectProviderModels(provider).then((result) => {
      if (!cancelled) setCatalogModels(result.models ?? []);
    }).catch(() => {
      if (!cancelled) setCatalogModels([]);
    });
    return () => {
      cancelled = true;
    };
  }, [draft?.llmProvider, llmProviders]);

  useEffect(() => {
    void api.listPlannerTraces().then((items) => setPlannerTraces(items.slice(0, 8)));
    void api.listToolRouterTraces().then((items) => setRouterTraces(items.slice(0, 8)));
  }, []);

  const update = <K extends keyof AgentDefinition>(key: K, value: AgentDefinition[K]) => {
    setDraft((prev) => (prev ? { ...prev, [key]: value } : prev));
  };

  const save = async () => {
    if (!draft) return;
    setSaving(true);
    try {
      const saved = await saveAgent(draft);
      setSelectedId(saved.id);
      setDraft({ ...saved });
    } finally {
      setSaving(false);
    }
  };

  const remove = async (id: string) => {
    if (agents.length <= 1) return;
    await deleteAgent(id);
    const remaining = useAppStore.getState().agents;
    if (selectedId === id) {
      const next = remaining[0] ?? null;
      setSelectedId(next?.id ?? "");
      setDraft(next ? { ...next } : null);
    }
  };

  const duplicateAgent = (source: AgentDefinition) => {
    setDraft({
      ...source,
      id: "",
      name: `${source.name} (副本)`,
      isDefault: false,
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString()
    });
    setSelectedId("");
  };

  const createNew = () => {
    const newAgent: AgentDefinition = {
      id: "",
      name: "新智能体",
      description: "",
      workspaceDir: "",
      llmProvider: "",
      llmModel: "",
      enabled: true,
      isDefault: false,
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
    };
    setDraft(newAgent);
    setSelectedId("");
  };

  const createFromDefault = () => {
    const defaultAgent = agents.find((a) => a.isDefault) ?? agents[0];
    if (!defaultAgent) return;
    duplicateAgent(defaultAgent);
  };

  const toggleMcpServer = (serverId: string) => {
    if (!draft) return;
    const enabled = draft.enabledMcpServers.includes(serverId)
      ? draft.enabledMcpServers.filter((s) => s !== serverId)
      : [...draft.enabledMcpServers, serverId];
    update("enabledMcpServers", enabled);
  };

  const toggleSkill = (skillId: string) => {
    if (!draft) return;
    const enabled = draft.enabledSkills.includes(skillId)
      ? draft.enabledSkills.filter((s) => s !== skillId)
      : [...draft.enabledSkills, skillId];
    update("enabledSkills", enabled);
  };

  const toggleEnabledToolset = (toolsetId: string) => {
    if (!draft) return;
    const enabledToolsets = draft.enabledToolsets.includes(toolsetId)
      ? draft.enabledToolsets.filter((item) => item !== toolsetId)
      : [...draft.enabledToolsets, toolsetId];
    update("enabledToolsets", enabledToolsets);
    if (!draft.enabledToolsets.includes(toolsetId)) {
      update("disabledToolsets", draft.disabledToolsets.filter((item) => item !== toolsetId));
    }
  };

  const toggleDisabledToolset = (toolsetId: string) => {
    if (!draft) return;
    const disabledToolsets = draft.disabledToolsets.includes(toolsetId)
      ? draft.disabledToolsets.filter((item) => item !== toolsetId)
      : [...draft.disabledToolsets, toolsetId];
    update("disabledToolsets", disabledToolsets);
    if (!draft.disabledToolsets.includes(toolsetId)) {
      update("enabledToolsets", draft.enabledToolsets.filter((item) => item !== toolsetId));
    }
  };

  const toggleBento = (key: "enabled" | "isDefault" | "mcpEnabled" | "skillsEnabled" | "allowShell") => {
    if (!draft) return;
    update(key, !draft[key]);
  };

  const filtered = agents.filter((a) =>
    `${a.name} ${a.description}`.toLowerCase().includes(search.toLowerCase())
  );
  const filteredSkills = useMemo(() => filterSkillsByQuery(skills, skillSearch), [skillSearch, skills]);

  const activeAgentCount = agents.filter((a) => a.enabled).length;
  const selectedProvider = draft
    ? llmProviders.find((item) => item.id === draft.llmProvider && item.enabled) ?? null
    : null;

  return (
    <section className="primary-panel embedded-panel" style={{ padding: 0 }}>
      <div className="panel-title action-title" style={{ padding: "var(--space-lg) var(--space-xl) var(--space-sm)", borderBottom: "1px solid var(--divider)" }}>
        <button className="icon-only-btn" onClick={() => useAppStore.getState().setSection("settings", "agent")} title="返回" type="button"><ChevronRight size={19} style={{ transform: "rotate(180deg)" }} /></button>
        <div className="panel-title-text"><Bot size={16} className="panel-title-icon" /><span>Agents</span><strong>智能体</strong></div>
      </div>

      <div className="agent-summary-grid" style={{ padding: "var(--space-md) var(--space-xl)" }}>
        <div className="agent-summary-item" onClick={() => { if (agents[0]) { setSelectedId(agents[0].id); setDraft({ ...agents[0] }); } }} style={{ cursor: "pointer" }}>
          <span className="agent-summary-icon indigo"><Bot size={18} /></span>
          <div className="agent-summary-text"><strong>{agents.length}</strong><small>智能体总数</small></div>
        </div>
        <div className="agent-summary-item">
          <span className="agent-summary-icon green"><Sparkles size={18} /></span>
          <div className="agent-summary-text"><strong>{activeAgentCount}</strong><small>已启用</small></div>
        </div>
        <div className="agent-summary-item">
          <span className="agent-summary-icon blue"><PlugZap size={18} /></span>
          <div className="agent-summary-text"><strong>{mcpServers.length}</strong><small>MCP 服务器</small></div>
        </div>
        <div className="agent-summary-item">
          <span className="agent-summary-icon purple"><Sparkles size={18} /></span>
          <div className="agent-summary-text"><strong>{skills.length}</strong><small>技能总数</small></div>
        </div>
      </div>

      <div className="agent-split" style={{ margin: "0 var(--space-xl) var(--space-xl)" }}>
        <div className="agent-sidebar">
          <div className="agent-sidebar-head">
            <div className="search-bar" style={{ margin: 0, flex: 1 }}>
              <Search size={15} />
              <input value={search} onChange={(e) => setSearch(e.target.value)} placeholder="搜索智能体..." />
            </div>
          </div>
          <div className="agent-list-scroll">
            {filtered.length === 0 ? (
              <p className="form-hint" style={{ padding: "24px 16px", textAlign: "center" }}>暂无匹配的智能体</p>
            ) : (
              filtered.map((agent) => (
                <div
                  className={`agent-card-item${agent.id === selectedId ? " active" : ""}`}
                  key={agent.id}
                  onClick={() => { setSelectedId(agent.id); setDraft({ ...agent }); }}
                  role="button"
                  tabIndex={0}
                  onKeyDown={(e) => { if (e.key === "Enter") { setSelectedId(agent.id); setDraft({ ...agent }); } }}
                >
                  <span className="agent-card-icon"><Sparkles size={18} /></span>
                  <div className="agent-card-info">
                    <span className="agent-card-name">{agent.name}{agent.isDefault ? " ★" : ""}</span>
                    <span className="agent-card-meta">{agent.llmProvider || "跟随角色"} · {agent.llmModel || "未指定模型"}</span>
                  </div>
                  <div className="agent-card-right-col">
                    <span className={`agent-card-badge ${agent.enabled ? "enabled" : "disabled"}`}>
                      {agent.enabled ? "ON" : "OFF"}
                    </span>
                    <button
                      className="agent-card-action-tag"
                      onClick={(e) => { e.stopPropagation(); duplicateAgent(agents.find((a) => a.id === agent.id) ?? agent); }}
                      type="button"
                      title="复制此智能体"
                    >
                      复制
                    </button>
                    {agent.id && agents.length > 1 ? (
                      <button
                        className="agent-card-action-tag danger"
                        onClick={(e) => { e.stopPropagation(); void remove(agent.id); }}
                        type="button"
                        title="删除此智能体"
                      >
                        删除
                      </button>
                    ) : null}
                  </div>
                </div>
              ))
            )}
          </div>
          <div style={{ padding: "8px 16px", borderTop: "1px solid var(--divider)" }}>
            <div className="inline-actions" style={{ width: "100%" }}>
              <button onClick={createNew} type="button" style={{ display: "flex", alignItems: "center", justifyContent: "center", gap: 6, flex: 1, padding: "10px", border: "1px dashed var(--divider)", borderRadius: "var(--radius-md)", background: "transparent", color: "var(--text-3)", fontSize: 13, cursor: "pointer", transition: "all 0.15s ease" }} onMouseEnter={(e) => { e.currentTarget.style.borderColor = "var(--primary)"; e.currentTarget.style.color = "var(--primary)"; e.currentTarget.style.background = "var(--primary-glow)"; }} onMouseLeave={(e) => { e.currentTarget.style.borderColor = "var(--divider)"; e.currentTarget.style.color = "var(--text-3)"; e.currentTarget.style.background = "transparent"; }}>
                <Plus size={15} /> 新建智能体
              </button>
              <button className="btn-secondary" onClick={createFromDefault} type="button" style={{ flex: 1, fontSize: 13, padding: "10px 12px" }}>
                复制默认
              </button>
            </div>
          </div>
        </div>

        {draft ? (
          <div className="agent-detail">
            <div className="agent-detail-header">
              <div className="agent-detail-title">
                <span className="agent-detail-title-icon"><Sparkles size={22} /></span>
                <div className="agent-detail-title-info">
                  <strong>{draft.name || "新智能体"}</strong>
                  <small>{draft.id ? `ID: ${draft.id.slice(0, 12)}...` : "尚未保存"}</small>
                </div>
              </div>
              <div className="agent-detail-actions">
                <button className="btn-primary" onClick={save} type="button" disabled={saving} style={{ fontSize: 13, padding: "8px 20px" }}>
                  {saving ? "保存中..." : "保存"}
                </button>
                <button className="btn-secondary" onClick={() => draft && duplicateAgent(draft)} type="button" style={{ fontSize: 13, padding: "8px 16px" }}>
                  复制
                </button>
                {draft.id && agents.length > 1 ? (
                  <button className="btn-danger-outline" onClick={() => void remove(draft.id)} type="button" style={{ fontSize: 13, padding: "8px 16px" }}>
                    <Trash2 size={14} /> 删除
                  </button>
                ) : null}
              </div>
            </div>

            <div className="agent-section">
              <div className="agent-section-head"><Edit3 size={16} /><strong>基本信息</strong></div>
              <div className="agent-form-row">
                <div className="agent-field"><label>名称</label><input value={draft.name} onChange={(e) => update("name", e.target.value)} placeholder="智能体名称" /></div>
                <div className="agent-field"><label>工作目录</label><input value={draft.workspaceDir} onChange={(e) => update("workspaceDir", e.target.value)} placeholder="留空使用默认目录" /></div>
              </div>
              <div className="agent-form-row single" style={{ marginTop: 12 }}>
                <div className="agent-field"><label>描述</label><input value={draft.description} onChange={(e) => update("description", e.target.value)} placeholder="智能体描述" /></div>
              </div>
            </div>

            <div className="agent-section">
              <div className="agent-section-head"><Bot size={16} /><strong>LLM 配置</strong></div>
              <div className="agent-form-row">
                <div className="agent-field">
                  <label>服务商</label>
                  <select
                    value={draft.llmProvider}
                    onChange={(e) => {
                      const nextProvider = llmProviders.find((item) => item.id === e.target.value);
                      setDraft((current) => current ? {
                        ...current,
                        llmProvider: e.target.value,
                        llmModel: nextProvider?.model ?? current.llmModel
                      } : current);
                    }}
                  >
                    <option value="">跟随通讯录角色</option>
                    {llmProviders.map((p) => (<option key={p.id} value={p.id}>{p.name}</option>))}
                  </select>
                </div>
                <div className="agent-field">
                  <label>模型 ID</label>
                  <div className="model-select-row">
                    {catalogModels.length > 0 ? (
                      <select
                        value={catalogModels.some((model) => model.id === draft.llmModel) ? draft.llmModel : ""}
                        onChange={(e) => {
                          const value = e.target.value;
                          if (value) update("llmModel", value);
                        }}
                      >
                        <option value="">从目录选择模型</option>
                        {catalogModels.map((model) => (
                          <option key={model.id} value={model.id}>{model.name || model.id}{model.family ? ` (${model.family})` : ""}</option>
                        ))}
                      </select>
                    ) : null}
                    <input
                      value={draft.llmModel}
                      onChange={(e) => update("llmModel", e.target.value)}
                      placeholder={catalogModels.length > 0 ? "或手动输入" : "模型 ID"}
                    />
                  </div>
                </div>
              </div>
            </div>

            <div className="agent-section">
              <div className="agent-section-head"><Sparkles size={16} /><strong>能力开关</strong></div>
              <div className="agent-bento-grid">
                <button className={`agent-bento-card ${draft.enabled ? "active" : ""}`} onClick={() => toggleBento("enabled")} type="button">
                  <Bot size={24} /><span className="agent-bento-label">启用智能体</span><span className="agent-bento-status" />
                </button>
                <button className={`agent-bento-card ${draft.isDefault ? "active" : ""}`} onClick={() => toggleBento("isDefault")} type="button">
                  <Sparkles size={24} /><span className="agent-bento-label">默认智能体</span><span className="agent-bento-status" />
                </button>
                <button className={`agent-bento-card ${draft.mcpEnabled ? "active" : ""}`} onClick={() => toggleBento("mcpEnabled")} type="button">
                  <PlugZap size={24} /><span className="agent-bento-label">MCP 工具</span><span className="agent-bento-status" />
                </button>
                <button className={`agent-bento-card ${draft.skillsEnabled ? "active" : ""}`} onClick={() => toggleBento("skillsEnabled")} type="button">
                  <BookOpen size={24} /><span className="agent-bento-label">技能加载</span><span className="agent-bento-status" />
                </button>
                <button className={`agent-bento-card ${draft.allowShell ? "active" : ""}`} onClick={() => toggleBento("allowShell")} type="button">
                  <Terminal size={24} /><span className="agent-bento-label">Shell 命令</span><span className="agent-bento-status" />
                </button>
              </div>
            </div>

            <div className="agent-section">
              <div className="agent-section-head"><RefreshCw size={16} /><strong>运行限制</strong></div>
              <div className="agent-form-row">
                <div className="agent-field"><label>Fallback 最大工具迭代</label><input min={1} max={90} type="number" value={draft.maxToolIterations} onChange={(e) => update("maxToolIterations", Number(e.target.value))} /></div>
                <div className="agent-field"><label>最大子智能体</label><input min={1} max={20} type="number" value={draft.maxSubagents} onChange={(e) => update("maxSubagents", Math.max(1, Number(e.target.value)))} /></div>
                <div className="agent-field"><label>最大子层级</label><input min={1} max={4} type="number" value={draft.maxSubagentDepth ?? 1} onChange={(e) => update("maxSubagentDepth", Number(e.target.value))} /></div>
              </div>
            </div>

            {mcpServers.length > 0 ? (
              <div className="agent-section">
                <div className="agent-section-head"><PlugZap size={16} /><strong>MCP 服务器</strong><small>{draft.enabledMcpServers.length}/{mcpServers.length} 已启用</small></div>
                <div className="agent-toggle-grid">
                  {mcpServers.map((s) => (
                    <button className={`agent-toggle-item ${draft.enabledMcpServers.includes(s.id) ? "active" : ""}`} key={s.id} onClick={() => toggleMcpServer(s.id)} type="button" title={s.name}>
                      <span className="agent-toggle-item-label"><PlugZap size={16} /><span>{s.name}</span></span>
                      <span className="agent-toggle-dot" />
                    </button>
                  ))}
                </div>
              </div>
            ) : null}

            <div className="agent-section">
              <div className="agent-section-head">
                <Puzzle size={16} />
                <strong>Toolsets</strong>
                <small>{draft.enabledToolsets.length ? `允许 ${draft.enabledToolsets.length}` : "默认全部"} · 禁用 {draft.disabledToolsets.length}</small>
              </div>
              <div className="agent-toggle-grid">
                {AGENT_TOOLSETS.map((toolset) => {
                  const allowed = draft.enabledToolsets.includes(toolset.id);
                  const denied = draft.disabledToolsets.includes(toolset.id);
                  return (
                    <div className="agent-toggle-item" key={toolset.id} title={toolset.description} style={{ flexDirection: "column", alignItems: "stretch", gap: 8 }}>
                      <span className="agent-toggle-item-label"><Puzzle size={16} /><span>{toolset.label}</span></span>
                      <div className="inline-actions">
                        <button className={`btn-secondary ${allowed ? "active" : ""}`} onClick={() => toggleEnabledToolset(toolset.id)} type="button">
                          允许
                        </button>
                        <button className={`btn-secondary ${denied ? "active" : ""}`} onClick={() => toggleDisabledToolset(toolset.id)} type="button">
                          禁用
                        </button>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>

            {skills.length > 0 ? (
              <div className="agent-section">
                <div className="agent-section-head">
                  <Sparkles size={16} /><strong>技能</strong>
                  <small>
                    {draft.enabledSkills.length}/{skills.length} 已启用
                    {skillSearch.trim() ? ` · ${filteredSkills.length} 匹配` : ""}
                  </small>
                </div>
                <div className="search-bar" style={{ marginBottom: 12 }}>
                  <Search size={16} />
                  <input
                    value={skillSearch}
                    onChange={(event) => setSkillSearch(event.target.value)}
                    placeholder="搜索技能名称 / ID / 描述"
                  />
                </div>
                {filteredSkills.length === 0 ? (
                  <p className="form-hint">没有匹配的技能</p>
                ) : (
                  <div className="agent-toggle-grid">
                    {filteredSkills.map((s) => (
                      <button className={`agent-toggle-item ${draft.enabledSkills.includes(s.id) ? "active" : ""}`} key={s.id} onClick={() => toggleSkill(s.id)} type="button" title={s.description}>
                        <span className="agent-toggle-item-label"><Sparkles size={16} /><span>{s.name}</span></span>
                        <span className="agent-toggle-dot" />
                      </button>
                    ))}
                  </div>
                )}
              </div>
            ) : null}

            {(plannerTraces.length > 0 || routerTraces.length > 0) ? (
              <div className="agent-section">
                <div className="agent-section-head"><RefreshCw size={16} /><strong>运行轨迹</strong></div>
                <div className="agent-trace-grid">
                  {plannerTraces.map((trace) => (
                    <div className="agent-trace-card" key={trace.id}>
                      <span className="agent-trace-indicator ok" />
                      <div className="agent-trace-info">
                        <strong>{trace.agentId} · iteration {trace.iteration + 1}</strong>
                        <small>{trace.parsedStep}</small>
                      </div>
                      <span className="agent-trace-time">{new Date(trace.createdAt).toLocaleTimeString()}</span>
                    </div>
                  ))}
                  {routerTraces.map((trace) => (
                    <div className="agent-trace-card" key={trace.id} title={trace.error || trace.output || trace.prompt}>
                      <span className={`agent-trace-indicator ${trace.status === "completed" ? "ok" : "error"}`} />
                      <div className="agent-trace-info">
                        <strong>{trace.semanticIntent}</strong>
                        <small>{trace.error || trace.output || "已生成工具计划"}</small>
                      </div>
                      <span className="agent-trace-time">{new Date(trace.createdAt).toLocaleTimeString()}</span>
                    </div>
                  ))}
                </div>
              </div>
            ) : null}
          </div>
        ) : (
          <div className="agent-detail-empty">
            <div className="empty-icon-wrap"><Sparkles size={36} /></div>
            <p>选择左侧智能体查看详情<br/>或点击「新建」创建新智能体</p>
          </div>
        )}
      </div>
    </section>
  );
}

export function SkillsPanel() {
  const {
    agents,
    skills,
    skillBundles,
    marketplaceSkills,
    refreshSkillsForAgent,
    saveSkillConfig,
    refreshSkillBundles,
    installSkillBundle,
    refreshMarketplaceSkills,
    installMarketplaceSkill,
    installExternalSkillUrl,
    goBack
  } = useAppStore();
  const [tab, setTab] = useState<"installed" | "bundles" | "marketplace">("installed");
  const [agentId, setAgentId] = useState(agents[0]?.id ?? "");
  const [installedSearchQuery, setInstalledSearchQuery] = useState("");
  const [searchQuery, setSearchQuery] = useState("");
  const [marketplaceSource, setMarketplaceSource] = useState<"local" | "tap" | "all">("local");
  const [skillUrlPresets, setSkillUrlPresets] = useState<SkillUrlPreset[]>(() => loadSkillUrlPresets());
  const [defaultSkillUrlPresetId, setDefaultSkillUrlPresetId] = useState(() => {
    const presets = loadSkillUrlPresets();
    return loadDefaultSkillUrlPresetId(presets);
  });
  const [editingUrlPresetId, setEditingUrlPresetId] = useState<string | null>(null);
  const [urlPresetLabel, setUrlPresetLabel] = useState("");
  const [installLocalPath, setInstallLocalPath] = useState("");
  const [installUrl, setInstallUrl] = useState<string>(() => {
    const presets = loadSkillUrlPresets();
    const presetId = loadDefaultSkillUrlPresetId(presets);
    return presets.find((preset) => preset.id === presetId)?.url ?? presets[0]?.url ?? "";
  });
  const [installName, setInstallName] = useState("");
  const [installCategory, setInstallCategory] = useState(() => {
    const presets = loadSkillUrlPresets();
    const presetId = loadDefaultSkillUrlPresetId(presets);
    return presets.find((preset) => preset.id === presetId)?.category ?? "";
  });
  const [installForce, setInstallForce] = useState(false);
  const [installNotice, setInstallNotice] = useState("");
  const [tapRepo, setTapRepo] = useState("");
  const [tapPath, setTapPath] = useState("skills/");
  const [tapNotice, setTapNotice] = useState("");
  const [snapshotPath, setSnapshotPath] = useState("");
  const [snapshotNotice, setSnapshotNotice] = useState("");
  const [installRecords, setInstallRecords] = useState<SkillInstallRecord[]>([]);
  const [auditLog, setAuditLog] = useState<SkillAuditLogEntry[]>([]);
  const [skillTaps, setSkillTaps] = useState<SkillTap[]>([]);
  const [tapStatuses, setTapStatuses] = useState<Record<string, SkillTapStatus>>({});
  const [tapMarketplaceSkills, setTapMarketplaceSkills] = useState<MarketplaceSkill[]>([]);
  const [updateChecks, setUpdateChecks] = useState<Record<string, SkillUpdateCheck>>({});
  const [expandedSkill, setExpandedSkill] = useState<string | null>(null);
  const [skillConfigDraft, setSkillConfigDraft] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const defaultSkillUrlPreset = useMemo(
    () => skillUrlPresets.find((preset) => preset.id === defaultSkillUrlPresetId) ?? skillUrlPresets[0] ?? null,
    [defaultSkillUrlPresetId, skillUrlPresets]
  );

  useEffect(() => {
    if (agentId) void refreshSkillsForAgent(agentId);
  }, [agentId, refreshSkillsForAgent]);

  useEffect(() => {
    saveSkillUrlPresets(skillUrlPresets);
  }, [skillUrlPresets]);

  useEffect(() => {
    if (!skillUrlPresets.some((preset) => preset.id === defaultSkillUrlPresetId)) {
      const fallbackId = skillUrlPresets[0]?.id ?? "";
      setDefaultSkillUrlPresetId(fallbackId);
      saveDefaultSkillUrlPresetId(fallbackId);
      return;
    }
    saveDefaultSkillUrlPresetId(defaultSkillUrlPresetId);
  }, [defaultSkillUrlPresetId, skillUrlPresets]);

  useEffect(() => {
    void refreshSkillBundles();
  }, [refreshSkillBundles]);

  useEffect(() => {
    if (tab === "marketplace" && marketplaceSkills.length === 0) {
      void refreshMarketplaceSkills(undefined, marketplaceSource);
    }
  }, [marketplaceSkills.length, marketplaceSource, refreshMarketplaceSkills, tab]);

  useEffect(() => {
    if (tab === "marketplace") {
      void refreshInstallRecords();
    }
  }, [tab]);

  const refreshInstallRecords = async () => {
    const records = await api.listSkillInstallRecords() as SkillInstallRecord[];
    setInstallRecords(records);
    const log = await api.listSkillAuditLog(8) as SkillAuditLogEntry[];
    setAuditLog(log);
    const taps = await api.listSkillTaps() as SkillTap[];
    setSkillTaps(taps);
  };

  const handleInstallBundle = async (bundleId: string) => {
    setBusy(true);
    try {
      await installSkillBundle(bundleId, agentId || undefined);
    } finally {
      setBusy(false);
    }
  };

  const handleMarketplaceSearch = async () => {
    setBusy(true);
    try {
      await refreshMarketplaceSkills(searchQuery || undefined, marketplaceSource);
    } finally {
      setBusy(false);
    }
  };

  const handleInstallMarketplace = async (skill: MarketplaceSkill) => {
    setBusy(true);
    try {
      if (skill.downloadUrl.startsWith("http://") || skill.downloadUrl.startsWith("https://")) {
        await installExternalSkillUrl(
          skill.downloadUrl,
          skill.name,
          skill.tags.includes("tap") ? "tap" : undefined,
          agentId || undefined,
          installForce
        );
      } else {
        await installMarketplaceSkill(skill.id, agentId || undefined);
      }
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshInstallRecords();
    } finally {
      setBusy(false);
    }
  };

  const handleInstallExternalUrl = async () => {
    const url = installUrl.trim();
    if (!url) return;
    setBusy(true);
    setInstallNotice("");
    try {
      await installExternalSkillUrl(
        url,
        installName.trim() || undefined,
        installCategory.trim() || undefined,
        agentId || undefined,
        installForce
      );
      setInstallNotice("已安装并启用");
      setInstallUrl("");
      setInstallName("");
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshMarketplaceSkills(searchQuery || undefined, marketplaceSource);
      await refreshInstallRecords();
    } catch (error) {
      setInstallNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const applyInstallUrlPreset = (preset: SkillUrlPreset) => {
    setInstallUrl(preset.url);
    setInstallCategory(preset.category);
  };

  const editInstallUrlPreset = (preset: SkillUrlPreset) => {
    setEditingUrlPresetId(preset.id);
    setUrlPresetLabel(preset.label);
    setInstallUrl(preset.url);
    setInstallCategory(preset.category);
  };

  const resetInstallUrlPresetEditor = () => {
    setEditingUrlPresetId(null);
    setUrlPresetLabel("");
  };

  const handleSaveInstallUrlPreset = () => {
    const url = installUrl.trim();
    if (!url) return;
    const preset: SkillUrlPreset = {
      id: editingUrlPresetId ?? `preset-${crypto.randomUUID()}`,
      label: urlPresetLabel.trim() || fallbackSkillUrlPresetLabel(url),
      url,
      category: installCategory.trim()
    };
    setSkillUrlPresets((current) => {
      if (editingUrlPresetId) {
        return current.map((item) => item.id === editingUrlPresetId ? preset : item);
      }
      return [...current, preset];
    });
    resetInstallUrlPresetEditor();
  };

  const handleDeleteInstallUrlPreset = (presetId: string) => {
    setSkillUrlPresets((current) => current.filter((preset) => preset.id !== presetId));
    if (editingUrlPresetId === presetId) resetInstallUrlPresetEditor();
  };

  const handleSetDefaultInstallUrlPreset = (presetId: string) => {
    setDefaultSkillUrlPresetId(presetId);
  };

  const handleInstallExternalPath = async () => {
    const sourcePath = installLocalPath.trim();
    if (!sourcePath) return;
    setBusy(true);
    setInstallNotice("");
    try {
      await api.installExternalSkillFile(
        sourcePath,
        installName.trim() || undefined,
        installCategory.trim() || undefined,
        agentId || undefined,
        installForce
      );
      setInstallNotice("已从本地目录/文件安装并启用");
      setInstallLocalPath("");
      setInstallName("");
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshMarketplaceSkills(searchQuery || undefined, marketplaceSource);
      await refreshInstallRecords();
    } catch (error) {
      setInstallNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleExportSnapshot = async () => {
    const path = snapshotPath.trim();
    if (!path) return;
    setBusy(true);
    setSnapshotNotice("");
    try {
      const exportedPath = await api.exportSkillSnapshot(path) as string;
      setSnapshotNotice(`已导出到 ${exportedPath || path}`);
    } catch (error) {
      setSnapshotNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleImportSnapshot = async () => {
    const path = snapshotPath.trim();
    if (!path) return;
    setBusy(true);
    setSnapshotNotice("");
    try {
      const imported = await api.importSkillSnapshot(path) as number;
      setSnapshotNotice(`已导入 ${imported} 个 skill`);
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshMarketplaceSkills(searchQuery || undefined, marketplaceSource);
      await refreshInstallRecords();
    } catch (error) {
      setSnapshotNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleAddSkillTap = async () => {
    const repo = tapRepo.trim();
    if (!repo) return;
    setBusy(true);
    setTapNotice("");
    try {
      const tap = await api.addSkillTap(repo, tapPath.trim() || undefined) as SkillTap;
      setTapNotice(`已添加 ${tap.repo}`);
      setTapRepo("");
      setTapPath("skills/");
      await refreshInstallRecords();
    } catch (error) {
      setTapNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleRemoveSkillTap = async (repo: string) => {
    setBusy(true);
    setTapNotice("");
    try {
      const removed = await api.removeSkillTap(repo) as boolean;
      setTapNotice(removed ? `已移除 ${repo}` : `未找到 ${repo}`);
      await refreshInstallRecords();
    } catch (error) {
      setTapNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleRefreshTapMarketplace = async () => {
    setBusy(true);
    setTapNotice("");
    try {
      const results = await api.listSkillTapMarketplace(searchQuery || undefined) as MarketplaceSkill[];
      setTapMarketplaceSkills(results);
      setTapNotice(`已发现 ${results.length} 个 tap skill`);
    } catch (error) {
      setTapNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleCheckSkillTaps = async () => {
    setBusy(true);
    setTapNotice("");
    try {
      const checks = await api.checkSkillTaps() as SkillTapStatus[];
      setTapStatuses(Object.fromEntries(checks.map((check) => [check.repo, check])));
      const ok = checks.filter((check) => check.status === "ok").length;
      setTapNotice(`已检查 ${checks.length} 个 tap：${ok} ok / ${checks.length - ok} error`);
    } catch (error) {
      setTapNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleInstallTapSkill = async (skill: MarketplaceSkill) => {
    setBusy(true);
    setInstallNotice("");
    try {
      await installExternalSkillUrl(
        skill.downloadUrl,
        skill.name,
        "tap",
        agentId || undefined,
        installForce
      );
      setInstallNotice(`已安装 ${skill.name}`);
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshMarketplaceSkills(searchQuery || undefined, marketplaceSource);
      await refreshInstallRecords();
    } catch (error) {
      setInstallNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(false);
    }
  };

  const handleCheckExternalSkill = async (skillId: string) => {
    setBusy(true);
    try {
      const record = installRecords.find((item) => item.skillId === skillId);
      const checks = record?.identifier.startsWith("http")
        ? await api.checkRemoteSkillUpdates(skillId) as SkillUpdateCheck[]
        : await api.checkSkillUpdates(skillId) as SkillUpdateCheck[];
      setUpdateChecks((prev) => ({ ...prev, [skillId]: checks[0] }));
    } finally {
      setBusy(false);
    }
  };

  const handleUpdateExternalSkill = async (skillId: string) => {
    setBusy(true);
    try {
      const record = installRecords.find((item) => item.skillId === skillId);
      if (record?.identifier.startsWith("http")) {
        await api.updateRemoteSkillsFromSources(skillId, agentId || undefined, installForce);
      } else {
        await api.updateSkillsFromSources(skillId, agentId || undefined, installForce);
      }
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshInstallRecords();
      const checks = record?.identifier.startsWith("http")
        ? await api.checkRemoteSkillUpdates(skillId) as SkillUpdateCheck[]
        : await api.checkSkillUpdates(skillId) as SkillUpdateCheck[];
      setUpdateChecks((prev) => ({ ...prev, [skillId]: checks[0] }));
    } finally {
      setBusy(false);
    }
  };

  const handleDeleteExternalSkill = async (skillId: string) => {
    if (!window.confirm(`删除 ${skillId} 的 external skill 文件和安装记录？`)) return;
    setBusy(true);
    try {
      await api.uninstallExternalSkills(skillId, true);
      if (agentId) await refreshSkillsForAgent(agentId);
      await refreshInstallRecords();
      setUpdateChecks((prev) => {
        const next = { ...prev };
        delete next[skillId];
        return next;
      });
    } finally {
      setBusy(false);
    }
  };

  const handleSaveSkillConfig = async (skillId: string) => {
    if (!agentId) return;
    await saveSkillConfig(agentId, skillId, skillConfigDraft);
    setExpandedSkill(null);
  };

  const openSkillConfig = (skill: EnhancedSkillSummary) => {
    setExpandedSkill(skill.id);
    setSkillConfigDraft({ ...skill.config });
  };

  const enabledCount = skills.filter((s) => s.enabled).length;
  const filteredInstalledSkills = useMemo(
    () => filterSkillsByQuery(skills, installedSearchQuery),
    [installedSearchQuery, skills]
  );
  const filteredEnabledCount = filteredInstalledSkills.filter((s) => s.enabled).length;

  return (
    <section className="primary-panel embedded-panel" style={{ padding: 0 }}>
      <div className="panel-title action-title" style={{ padding: "var(--space-lg) var(--space-xl) 0", borderBottom: "none" }}>
        <div className="panel-title-text"><Sparkles size={16} className="panel-title-icon" /><span>Skills</span><strong>技能</strong></div>
      </div>

      {/* Agent Selector */}
      <div className="skill-agent-selector">
        <label>安装到</label>
        <select value={agentId} onChange={(e) => setAgentId(e.target.value)}>
          {agents.map((a) => (
            <option key={a.id} value={a.id}>{a.name}{a.isDefault ? " (默认)" : ""}</option>
          ))}
        </select>
      </div>

      {/* Tabs */}
      <div className="skills-tabs">
        <button className={`skills-tab ${tab === "installed" ? "active" : ""}`} onClick={() => setTab("installed")} type="button">
          <Sparkles size={15} /> 已安装
          <span className="skills-tab-count">{skills.length}</span>
        </button>
        <button className={`skills-tab ${tab === "bundles" ? "active" : ""}`} onClick={() => setTab("bundles")} type="button">
          <Download size={15} /> 技能包
          <span className="skills-tab-count">{skillBundles.length}</span>
        </button>
        <button className={`skills-tab ${tab === "marketplace" ? "active" : ""}`} onClick={() => setTab("marketplace")} type="button">
          <Search size={15} /> 市场
          <span className="skills-tab-count">{marketplaceSkills.length}</span>
        </button>
      </div>

      {/* Content */}
      <div className="skills-content">
        <div className="skill-config-panel" style={{ marginBottom: 16 }}>
          <div className="card-header" style={{ marginBottom: 12 }}>
            <Download size={15} /> 技能快照
          </div>
          <div className="settings-form">
            <div className="form-row">
              <label>JSON 路径</label>
              <input
                value={snapshotPath}
                onChange={(e) => setSnapshotPath(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void handleExportSnapshot();
                }}
                placeholder="D:\\path\\skills-snapshot.json"
              />
            </div>
            <div className="inline-actions">
              <button className="btn-primary" onClick={handleExportSnapshot} disabled={busy || !snapshotPath.trim()} type="button">
                <Download size={14} /> 导出
              </button>
              <button className="btn-secondary" onClick={handleImportSnapshot} disabled={busy || !snapshotPath.trim()} type="button">
                <RefreshCw size={14} /> 导入
              </button>
            </div>
            {snapshotNotice ? <p className="form-hint">{snapshotNotice}</p> : null}
          </div>
        </div>

        {/* Installed Skills Tab */}
        {tab === "installed" ? (
          skills.length === 0 ? (
            <div className="empty-state compact" style={{ minHeight: 200 }}>
              <div className="empty-icon-wrap"><Sparkles size={36} strokeWidth={1.5} /></div>
              <p>该智能体暂无技能</p>
            </div>
          ) : (
            <>
              <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12, marginBottom: 12, flexWrap: "wrap" }}>
                <span style={{ fontSize: 13, color: "var(--text-3)" }}>
                  {enabledCount}/{skills.length} 已启用
                  {installedSearchQuery.trim() ? ` · ${filteredEnabledCount}/${filteredInstalledSkills.length} 匹配` : ""}
                </span>
                <div className="search-bar" style={{ minWidth: 280, flex: "1 1 280px", marginLeft: "auto" }}>
                  <Search size={16} />
                  <input
                    value={installedSearchQuery}
                    onChange={(event) => setInstalledSearchQuery(event.target.value)}
                    placeholder="搜索技能名称 / ID / 描述"
                  />
                </div>
              </div>
              {filteredInstalledSkills.length === 0 ? (
                <div className="empty-state compact" style={{ minHeight: 200 }}>
                  <div className="empty-icon-wrap"><Search size={36} strokeWidth={1.5} /></div>
                  <p>没有匹配的技能</p>
                </div>
              ) : (
                filteredInstalledSkills.map((skill) => (
                  <div key={skill.id}>
                    <div
                      className="skill-card"
                      onClick={() => openSkillConfig(skill)}
                      role="button"
                      tabIndex={0}
                      onKeyDown={(e) => { if (e.key === "Enter") openSkillConfig(skill); }}
                    >
                      <span className="skill-card-icon">
                        <Sparkles size={20} />
                      </span>
                      <div className="skill-card-info">
                        <strong>{skill.name}</strong>
                        <small>
                          {skill.description}
                          {skill.version ? ` · v${skill.version}` : ""}
                          {skill.author ? ` · ${skill.author}` : ""}
                        </small>
                      </div>
                      <div className="skill-card-right">
                        <span className={`status-badge ${skill.enabled ? "enabled" : "disabled"}`}>
                          {skill.enabled ? "启用" : "停用"}
                        </span>
                        <ChevronRight size={16} style={{ color: "var(--text-3)" }} />
                      </div>
                    </div>
                    {expandedSkill === skill.id ? (
                      <div className="skill-config-panel">
                        <div className="card-header" style={{ marginBottom: 12 }}>技能配置 — {skill.name}</div>
                        <div className="settings-form">
                          {Object.keys(skill.config).length === 0 ? (
                            <p className="form-hint">该技能无可配置项</p>
                          ) : (
                            Object.entries(skill.config).map(([key, value]) => (
                              <div className="form-group" key={key}>
                                <div className="form-row">
                                  <label>{key}</label>
                                  <input
                                    value={skillConfigDraft[key] ?? value}
                                    onChange={(e) => setSkillConfigDraft((prev) => ({ ...prev, [key]: e.target.value }))}
                                  />
                                </div>
                              </div>
                            ))
                          )}
                          <div className="inline-actions">
                            <button onClick={() => handleSaveSkillConfig(skill.id)} type="button">保存配置</button>
                            <button className="btn-secondary" onClick={() => setExpandedSkill(null)} type="button">取消</button>
                          </div>
                        </div>
                      </div>
                    ) : null}
                  </div>
                ))
              )}
            </>
          )
        ) : null}

        {/* Skill Bundles Tab */}
        {tab === "bundles" ? (
          skillBundles.length === 0 ? (
            <div className="empty-state compact" style={{ minHeight: 200 }}>
              <div className="empty-icon-wrap"><Download size={36} strokeWidth={1.5} /></div>
              <p>暂无技能包</p>
            </div>
          ) : (
            skillBundles.map((bundle) => (
              <div className="skill-bundle-card" key={bundle.id}>
                <div className="skill-bundle-info">
                  <strong>{bundle.name}</strong>
                  <small>{bundle.description} · {bundle.skillIds.length} 个技能</small>
                </div>
                <button
                  className="btn-primary"
                  onClick={() => handleInstallBundle(bundle.id)}
                  type="button"
                  disabled={busy}
                  style={{ fontSize: 13, padding: "8px 16px", flexShrink: 0 }}
                >
                  <Download size={14} /> 安装
                </button>
              </div>
            ))
          )
        ) : null}

        {/* Marketplace Tab */}
        {tab === "marketplace" ? (
          <>
            <div className="skill-config-panel" style={{ marginBottom: 16 }}>
              <div className="card-header" style={{ marginBottom: 12 }}>
                <Plus size={15} /> 从本地导入
              </div>
              <div className="settings-form">
                <div className="form-row">
                  <label>本地路径</label>
                  <input
                    value={installLocalPath}
                    onChange={(e) => setInstallLocalPath(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") void handleInstallExternalPath(); }}
                    placeholder="SKILL.md 文件或完整 skill 目录"
                  />
                </div>
                <div className="form-row">
                  <label>名称</label>
                  <input value={installName} onChange={(e) => setInstallName(e.target.value)} placeholder="留空使用 frontmatter" />
                </div>
                <div className="form-row">
                  <label>分类</label>
                  <input value={installCategory} onChange={(e) => setInstallCategory(e.target.value)} placeholder="例如 docs/research" />
                </div>
                <label className="checkbox-row">
                  <input type="checkbox" checked={installForce} onChange={(e) => setInstallForce(e.target.checked)} />
                  <span>允许覆盖审计阻断或同名 skill</span>
                </label>
                <div className="inline-actions">
                  <button className="btn-primary" onClick={handleInstallExternalPath} disabled={busy || !installLocalPath.trim()} type="button">
                    <Plus size={14} /> 导入本地 Skill
                  </button>
                </div>
                {installNotice ? <p className="form-hint">{installNotice}</p> : null}
              </div>
            </div>
            <div className="skill-config-panel" style={{ marginBottom: 16 }}>
              <div className="card-header" style={{ marginBottom: 12 }}>
                <ExternalLink size={15} /> 从 URL 安装
              </div>
              <div className="settings-form">
                <div className="form-row">
                  <label>常用 URL</label>
                  <div style={{ display: "grid", gap: 8 }}>
                    {skillUrlPresets.map((preset) => (
                      <div className="adapter-row trace-row" key={preset.id}>
                        <span className={`status-badge ${preset.id === defaultSkillUrlPresetId ? "enabled" : "warning"}`}>
                          {preset.id === defaultSkillUrlPresetId ? "默认" : "预设"}
                        </span>
                        <div className="adapter-info">
                          <strong>{preset.label}</strong>
                          <code>{preset.url}</code>
                          {preset.category ? <small>分类: {preset.category}</small> : null}
                        </div>
                        <div className="inline-actions">
                          <button className="btn-secondary" onClick={() => applyInstallUrlPreset(preset)} type="button">使用</button>
                          <button className="btn-secondary" onClick={() => editInstallUrlPreset(preset)} type="button">编辑</button>
                          <button
                            className="btn-secondary"
                            onClick={() => handleSetDefaultInstallUrlPreset(preset.id)}
                            disabled={preset.id === defaultSkillUrlPresetId}
                            type="button"
                          >
                            设为默认
                          </button>
                          <button className="btn-secondary" onClick={() => handleDeleteInstallUrlPreset(preset.id)} type="button" title="删除预设">
                            <Trash2 size={14} />
                          </button>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>
                <div className="form-row">
                  <label>{editingUrlPresetId ? "编辑预设名称" : "新增预设名称"}</label>
                  <input
                    value={urlPresetLabel}
                    onChange={(e) => setUrlPresetLabel(e.target.value)}
                    placeholder={defaultSkillUrlPreset?.label || "例如 hermes-agent"}
                  />
                </div>
                <div className="form-row">
                  <label>公开 URL / SKILL.md URL</label>
                  <input
                    value={installUrl}
                    onChange={(e) => setInstallUrl(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") void handleInstallExternalUrl(); }}
                    placeholder="GitHub blob/tree、raw SKILL.md 或 skills.sh 具体技能页"
                  />
                </div>
                <p className="form-hint">支持 GitHub blob/tree 页面、raw `SKILL.md` 直链、`skills.sh` 具体技能详情页；首页或目录页不能直接安装。</p>
                <div className="form-row">
                  <label>名称</label>
                  <input value={installName} onChange={(e) => setInstallName(e.target.value)} placeholder="留空使用 frontmatter" />
                </div>
                <div className="form-row">
                  <label>分类</label>
                  <input value={installCategory} onChange={(e) => setInstallCategory(e.target.value)} placeholder="例如 docs/research" />
                </div>
                <label className="checkbox-row">
                  <input type="checkbox" checked={installForce} onChange={(e) => setInstallForce(e.target.checked)} />
                  <span>允许覆盖审计阻断或同名 skill</span>
                </label>
                <div className="inline-actions">
                  <button className="btn-secondary" onClick={handleSaveInstallUrlPreset} disabled={!installUrl.trim()} type="button">
                    <Plus size={14} /> {editingUrlPresetId ? "更新预设" : "保存为预设"}
                  </button>
                  {editingUrlPresetId ? (
                    <button className="btn-secondary" onClick={resetInstallUrlPresetEditor} type="button">
                      取消编辑
                    </button>
                  ) : null}
                  <button className="btn-primary" onClick={handleInstallExternalUrl} disabled={busy || !installUrl.trim()} type="button">
                    <Download size={14} /> 安装 URL
                  </button>
                </div>
                {installNotice ? <p className="form-hint">{installNotice}</p> : null}
              </div>
            </div>
            <div className="skill-config-panel" style={{ marginBottom: 16 }}>
              <div className="card-header" style={{ marginBottom: 12 }}>
                <PlugZap size={15} /> Custom taps
              </div>
              <div className="settings-form">
                <div className="form-row">
                  <label>Repo</label>
                  <input
                    value={tapRepo}
                    onChange={(e) => setTapRepo(e.target.value)}
                    onKeyDown={(e) => { if (e.key === "Enter") void handleAddSkillTap(); }}
                    placeholder="owner/repo"
                  />
                </div>
                <div className="form-row">
                  <label>Path</label>
                  <input value={tapPath} onChange={(e) => setTapPath(e.target.value)} placeholder="skills/" />
                </div>
                <div className="inline-actions">
                  <button className="btn-primary" onClick={handleAddSkillTap} disabled={busy || !tapRepo.trim()} type="button">
                    <Plus size={14} /> 添加 tap
                  </button>
                  <button className="btn-secondary" onClick={handleRefreshTapMarketplace} disabled={busy || skillTaps.length === 0} type="button">
                    <RefreshCw size={14} /> 刷新 taps
                  </button>
                  <button className="btn-secondary" onClick={handleCheckSkillTaps} disabled={busy || skillTaps.length === 0} type="button">
                    <Terminal size={14} /> 检查 taps
                  </button>
                </div>
                {tapNotice ? <p className="form-hint">{tapNotice}</p> : null}
                {skillTaps.map((tap) => {
                  const status = tapStatuses[tap.repo];
                  return (
                    <div className="adapter-row trace-row" key={tap.repo}>
                      <span className={`status-badge ${status?.status === "error" ? "disabled" : "enabled"}`}>
                        {status?.status ?? "tap"}
                      </span>
                      <div className="adapter-info">
                        <strong>{tap.repo}</strong>
                        <code>{tap.path}</code>
                        {status ? <small>{status.entryCount} entries · {status.detail}</small> : null}
                      </div>
                      <div className="inline-actions">
                        <button className="btn-secondary" disabled={busy} onClick={() => void handleRemoveSkillTap(tap.repo)} title="删除 tap" type="button">
                          <Trash2 size={14} />
                        </button>
                      </div>
                    </div>
                  );
                })}
                {tapMarketplaceSkills.map((skill) => (
                  <div className="marketplace-card" key={skill.id}>
                    <span className="marketplace-icon"><Sparkles size={20} /></span>
                    <div className="marketplace-info">
                      <strong>{skill.name}</strong>
                      <small>{skill.description} · {skill.author || "tap"} · v{skill.version}</small>
                      <code>{skill.downloadUrl}</code>
                    </div>
                    <button
                      className="btn-primary"
                      onClick={() => void handleInstallTapSkill(skill)}
                      type="button"
                      disabled={busy}
                      style={{ fontSize: 13, padding: "8px 16px", flexShrink: 0 }}
                    >
                      <Download size={14} /> 安装
                    </button>
                  </div>
                ))}
              </div>
            </div>
            {installRecords.length > 0 ? (
              <div className="skill-config-panel" style={{ marginBottom: 16 }}>
                <div className="card-header" style={{ marginBottom: 12 }}>
                  <Download size={15} /> External 安装记录
                </div>
                <div className="settings-form">
                  {installRecords.map((record) => {
                    const check = updateChecks[record.skillId];
                    return (
                      <div className="adapter-row trace-row" key={record.skillId}>
                        <span className={`status-badge ${record.auditStatus === "ok" ? "enabled" : "disabled"}`}>
                          {check?.status ?? record.auditStatus}
                        </span>
                        <div className="adapter-info">
                          <strong>{record.name}</strong>
                          <small>{record.skillId} · {record.source} · {new Date(record.installedAt).toLocaleString()}</small>
                          {check ? <code>{check.detail}</code> : <code>{record.identifier}</code>}
                        </div>
                        <div className="inline-actions">
                          <button className="btn-secondary" disabled={busy} onClick={() => void handleCheckExternalSkill(record.skillId)} title="检查更新" type="button">
                            <RefreshCw size={14} />
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void handleUpdateExternalSkill(record.skillId)} title="更新" type="button">
                            <Download size={14} />
                          </button>
                          <button className="btn-secondary" disabled={busy} onClick={() => void handleDeleteExternalSkill(record.skillId)} title="删除" type="button">
                            <Trash2 size={14} />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ) : null}
            {auditLog.length > 0 ? (
              <div className="skill-config-panel" style={{ marginBottom: 16 }}>
                <div className="card-header" style={{ marginBottom: 12 }}>
                  <Terminal size={15} /> 最近审计日志
                </div>
                <div className="settings-form">
                  {auditLog.map((entry, index) => {
                    const status = entry.auditStatus ?? (entry.type === "skill_uninstall" ? "removed" : "recorded");
                    return (
                      <div className="adapter-row trace-row" key={`${entry.createdAt ?? "log"}-${entry.skillId ?? index}`}>
                        <span className={`status-badge ${status === "ok" ? "enabled" : "disabled"}`}>
                          {status}
                        </span>
                        <div className="adapter-info">
                          <strong>{entry.name ?? entry.skillId ?? entry.type ?? "skill event"}</strong>
                          <small>
                            {entry.type ?? "skill_event"}
                            {entry.source ? ` · ${entry.source}` : ""}
                            {entry.createdAt ? ` · ${new Date(entry.createdAt).toLocaleString()}` : ""}
                          </small>
                          <code>
                            {entry.identifier
                              ?? entry.installPath
                              ?? (typeof entry.findingCount === "number" ? `${entry.findingCount} finding(s)` : JSON.stringify(entry))}
                          </code>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>
            ) : null}
            <div className="search-bar" style={{ margin: "0 0 16px" }}>
              <Search size={17} />
              <input
                value={searchQuery}
                onChange={(e) => setSearchQuery(e.target.value)}
                onKeyDown={(e) => { if (e.key === "Enter") handleMarketplaceSearch(); }}
                placeholder="搜索技能..."
              />
              <select
                value={marketplaceSource}
                onChange={(e) => setMarketplaceSource(e.target.value as "local" | "tap" | "all")}
                title="搜索来源"
              >
                <option value="local">Local</option>
                <option value="tap">Taps</option>
                <option value="all">All</option>
              </select>
              <button onClick={handleMarketplaceSearch} type="button" disabled={busy}>
                <Search size={14} />
              </button>
            </div>
            {marketplaceSkills.length === 0 ? (
              <div className="empty-state compact" style={{ minHeight: 200 }}>
                <div className="empty-icon-wrap"><Search size={36} strokeWidth={1.5} /></div>
                <p>没有匹配的技能</p>
                <span className="form-hint">清空搜索词后可浏览本地可发现技能</span>
              </div>
            ) : (
              marketplaceSkills.map((skill) => (
                <div className="marketplace-card" key={skill.id}>
                  <span className="marketplace-icon"><Sparkles size={20} /></span>
                  <div className="marketplace-info">
                    <strong>{skill.name}</strong>
                    <small>{skill.description} · v{skill.version} · {skill.author}</small>
                    {skill.tags.length > 0 ? (
                      <div className="marketplace-tags">
                        {skill.tags.slice(0, 5).map((tag) => (
                          <span className="marketplace-tag" key={tag}>{tag}</span>
                        ))}
                      </div>
                    ) : null}
                  </div>
                  <button
                    className="btn-primary"
                    onClick={() => handleInstallMarketplace(skill)}
                    type="button"
                    disabled={busy}
                    style={{ fontSize: 13, padding: "8px 16px", flexShrink: 0 }}
                  >
                    <Download size={14} /> 安装
                  </button>
                </div>
              ))
            )}
          </>
        ) : null}
      </div>
    </section>
  );
}
