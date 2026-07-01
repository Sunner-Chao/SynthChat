import React, { useState, useEffect, useRef, useCallback, useMemo } from "react";
import { Bot, Plus, Trash2, Save, Cpu, Settings2, ShieldAlert, FolderOpen, Wrench, ChevronRight, Puzzle, Users } from "lucide-react";
import { api } from "../lib/api";
import { useAppStore } from "../lib/store";
import type { AgentDefinition, ModelCatalogEntry } from "../lib/types";

const clampNumber = (value: number, min: number, max: number) => {
  if (!Number.isFinite(value)) return min;
  return Math.min(max, Math.max(min, value));
};

export function AgentsManagerPanel() {
  const {
    agents, refreshAgents, saveAgent, deleteAgent, goBack, llmProviders, config, saveConfig,
    focusedAgentId, setFocusedAgentId, setMcpPanelMode, setSkillsPanelMode, setSection
  } = useAppStore();

  const [draft, setDraft] = useState<AgentDefinition | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);
  // Delegation settings (from ChatConfig)
  const chatCfg = config?.chat;
  const [delegationMax, setDelegationMax] = useState(chatCfg?.delegationMaxConcurrentChildren ?? 3);
  const [delegationStrategy, setDelegationStrategy] = useState(chatCfg?.delegationStrategy ?? "auto");
  const [delegationOrch, setDelegationOrch] = useState(chatCfg?.delegationOrchestratorEnabled !== false);
  const [delegationAutoApprove, setDelegationAutoApprove] = useState(chatCfg?.delegationSubagentAutoApprove === true);
  const [delegationInheritMcp, setDelegationInheritMcp] = useState(chatCfg?.delegationInheritMcpToolsets !== false);
  const [delegationProviderId, setDelegationProviderId] = useState(chatCfg?.delegationSubagentProviderId ?? "");
  const [delegationModel, setDelegationModel] = useState(chatCfg?.delegationSubagentModel ?? "");
  // Ref to reliably track "creating new" mode across renders (avoids stale closure)
  const isCreatingNewRef = useRef(false);

  const isCreatingNew = isCreatingNewRef.current;
  const selectedProvider = useMemo(
    () => llmProviders.find((provider) => provider.id === draft?.llmProvider) ?? null,
    [draft?.llmProvider, llmProviders]
  );

  useEffect(() => {
    void refreshAgents();
  }, [refreshAgents]);

  useEffect(() => {
    if (!selectedProvider) {
      setCatalogModels([]);
      return;
    }
    let cancelled = false;
    api.detectProviderModels(selectedProvider).then((result) => {
      if (!cancelled) setCatalogModels(result.models ?? []);
    }).catch(() => {
      if (!cancelled) setCatalogModels([]);
    });
    return () => {
      cancelled = true;
    };
  }, [selectedProvider]);

  // Synchronize draft with focusedAgentId
  useEffect(() => {
    // When creating a new unsaved agent, don't auto-select or overwrite draft
    if (isCreatingNewRef.current) return;

    if (focusedAgentId) {
      const agent = agents.find(a => a.id === focusedAgentId);
      if (agent) {
        setDraft(agent);
      }
    } else if (agents.length > 0) {
      setFocusedAgentId(agents[0].id);
    } else {
      setDraft(null);
    }
  }, [agents, focusedAgentId, setFocusedAgentId]);

  const handleSelect = useCallback((agent: AgentDefinition) => {
    isCreatingNewRef.current = false;
    setFocusedAgentId(agent.id);
  }, [setFocusedAgentId]);

  const handleCreate = useCallback(() => {
    const newId = `agent-${Date.now()}`;
    const newAgent: AgentDefinition = {
      id: newId,
      name: "新智能体 (New Agent)",
      description: "A specialized assistant",
      workspaceDir: "",
      llmProvider: "",
      llmModel: "",
      enabled: true,
      isDefault: false,
      mcpEnabled: true,
      skillsEnabled: true,
      allowShell: false,
      maxSubagents: 4,
      maxSubagentDepth: 1,
      maxToolIterations: 90,
      skillsDir: "",
      enabledSkills: [],
      enabledMcpServers: [],
      enabledToolsets: [],
      disabledToolsets: [],
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    isCreatingNewRef.current = true;
    setDraft(newAgent);
    setFocusedAgentId(null);
  }, [setFocusedAgentId]);

  const handleSave = useCallback(async () => {
    if (!draft) return;
    if (!draft.name.trim()) {
      setError("智能体名称不能为空");
      return;
    }
    setError(null);
    setSaving(true);
    try {
      const saved = await saveAgent(draft);
      // Save delegation settings to chat config
      if (config) {
        await saveConfig({
          ...config,
          chat: {
            ...config.chat,
            delegationMaxConcurrentChildren: delegationMax,
            delegationStrategy,
            delegationOrchestratorEnabled: delegationOrch,
            delegationSubagentAutoApprove: delegationAutoApprove,
            delegationInheritMcpToolsets: delegationInheritMcp,
            delegationSubagentProviderId: delegationProviderId,
            delegationSubagentModel: delegationModel,
          }
        });
      }
      isCreatingNewRef.current = false;
      setFocusedAgentId(saved.id);
    } catch (e) {
      setError(`保存失败: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  }, [draft, saveAgent, setFocusedAgentId]);

  const handleDelete = useCallback(async () => {
    if (!draft) return;
    // For unsaved new agents, just discard without calling backend
    if (isCreatingNew) {
      isCreatingNewRef.current = false;
      setDraft(null);
      setFocusedAgentId(agents.length > 0 ? agents[0].id : null);
      return;
    }
    if (!window.confirm("确定要彻底删除该智能体吗？")) return;
    setError(null);
    const deletedId = draft.id;
    const previousDraft = draft;
    // Optimistic clear
    isCreatingNewRef.current = false;
    setDraft(null);
    setFocusedAgentId(null);
    try {
      await deleteAgent(deletedId);
    } catch (e) {
      // Rollback on failure
      setError(`删除失败: ${String(e)}`);
      setDraft(previousDraft);
      setFocusedAgentId(deletedId);
    }
  }, [draft, isCreatingNew, agents, deleteAgent, setFocusedAgentId]);

  const handleChange = useCallback((field: keyof AgentDefinition, value: any) => {
    if (draft) {
      setDraft({ ...draft, [field]: value });
    }
  }, [draft]);

  const handleProviderChange = useCallback((providerId: string) => {
    const nextProvider = llmProviders.find((provider) => provider.id === providerId);
    setDraft((current) => current ? {
      ...current,
      llmProvider: providerId,
      llmModel: nextProvider?.model ?? ""
    } : current);
  }, [llmProviders]);

  const goToLocalMcp = useCallback(() => {
    setMcpPanelMode("local");
    setSection("mcp");
  }, [setMcpPanelMode, setSection]);

  const goToLocalSkills = useCallback(() => {
    setSkillsPanelMode("local");
    setSection("skills");
  }, [setSkillsPanelMode, setSection]);

  return (
    <section className="primary-panel embedded-panel settings-form mcp-console" style={{ display: "flex", flexDirection: "column", height: "100%", padding: 0 }}>
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button">
          <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
        </button>
        <div className="panel-title-text">
          <Bot size={16} className="panel-title-icon" />
          <span>Agent Workspace</span>
          <strong>智能体管理</strong>
        </div>
      </div>

      <div style={{ display: "flex", flex: 1, overflow: "hidden" }}>
        {/* Left Sidebar - Agent List */}
        <div className="beautiful-sidebar" style={{ width: "260px", display: "flex", flexDirection: "column" }}>
          <div style={{ padding: "16px", borderBottom: "1px solid var(--divider)" }}>
            <button className="btn-primary beautiful-btn-primary" onClick={handleCreate} style={{ width: "100%", justifyContent: "center", display: "flex", gap: 6 }}>
              <Plus size={16} /> 创建新智能体
            </button>
          </div>
          <div style={{ overflowY: "auto", flex: 1 }}>
            {isCreatingNew && (
              <div
                className="adapter-row beautiful-row active"
                style={{ cursor: "pointer", padding: "12px 16px", display: "grid", gridTemplateColumns: "auto 1fr" }}
              >
                <span className="row-icon indigo"><Bot size={18} /></span>
                <div className="adapter-info">
                  <strong style={{ display: "block", marginBottom: 2 }}>{draft?.name || "新智能体"}</strong>
                  <small style={{ opacity: 0.7, color: "var(--primary)" }}>[新建中...]</small>
                </div>
              </div>
            )}
            {agents.map(agent => (
              <div
                key={agent.id}
                className={`adapter-row beautiful-row ${focusedAgentId === agent.id && !isCreatingNew ? "active" : ""}`}
                onClick={() => handleSelect(agent)}
                style={{ cursor: "pointer", padding: "12px 16px", display: "grid", gridTemplateColumns: "auto 1fr" }}
              >
                <span className="row-icon indigo"><Bot size={18} /></span>
                <div className="adapter-info">
                  <strong style={{ display: "block", marginBottom: 2 }}>{agent.name} {agent.isDefault && "⭐"}</strong>
                  <small style={{ opacity: 0.7 }}>{agent.llmModel}</small>
                </div>
              </div>
            ))}
          </div>
        </div>

        {/* Right Area - Editor */}
        <div style={{ flex: 1, overflowY: "auto", padding: "24px", background: "var(--background)" }}>
          {draft ? (
            <div style={{ maxWidth: "800px", margin: "0 auto" }}>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "20px" }}>
                <h2 style={{ margin: 0, fontSize: "1.25rem", fontWeight: 600 }}>{draft.name || "Unnamed Agent"}</h2>
                <div style={{ display: "flex", gap: "12px" }}>
                  {/* Hide delete for unsaved new agents, show as outline for existing */}
                  {!isCreatingNew && (
                    <button className="btn-danger-outline" onClick={handleDelete} title="彻底删除" style={{ minWidth: 120, justifyContent: "center", display: "inline-flex", alignItems: "center", padding: "8px 16px", fontSize: 14, borderRadius: "var(--radius-md)", border: "1px solid var(--danger)", background: "transparent", color: "var(--danger)", cursor: "pointer", transition: "all var(--duration-fast)" }}>
                      <Trash2 size={15} style={{ marginRight: 4 }} /> 删除
                    </button>
                  )}
                  <button className="btn-primary beautiful-btn-primary" onClick={handleSave} disabled={saving} style={{ minWidth: 120, justifyContent: "center" }}>
                    {saving ? "保存中..." : <><Save size={15} style={{ marginRight: 4 }} /> 保存配置</>}
                  </button>
                </div>
              </div>

              {error && (
                <div style={{ marginBottom: 16, padding: "10px 14px", borderRadius: "var(--radius-md)", background: "rgba(239, 68, 68, 0.08)", border: "1px solid rgba(239, 68, 68, 0.2)", color: "var(--danger)", fontSize: 13 }}>
                  {error}
                </div>
              )}

              <div className="card beautiful-card" style={{ marginBottom: "20px" }}>
                <div className="card-header"><Settings2 size={15} style={{ marginRight: 6 }}/> 基础配置 (Basic Profile)</div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)", fontWeight: 500 }}>智能体名称 (Name) *</label>
                  <input className="text-input" value={draft.name} onChange={e => handleChange("name", e.target.value)} placeholder="请输入名称" style={{ width: "100%", maxWidth: "400px" }} />
                </div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)", fontWeight: 500 }}>描述 (Description)</label>
                  <textarea className="text-input" value={draft.description} onChange={e => handleChange("description", e.target.value)} rows={3} style={{ width: "100%", maxWidth: "600px" }} />
                </div>
                <label className="adapter-row" style={{ cursor: "pointer", padding: "16px 20px", display: "grid", gridTemplateColumns: "auto 1fr auto", borderTop: "1px solid var(--divider)" }}>
                  <div className="adapter-info">
                    <strong>设为默认智能体</strong>
                    <small>新对话将默认使用该智能体进行交互</small>
                  </div>
                  <input type="checkbox" className="beautiful-checkbox" checked={draft.isDefault} onChange={e => handleChange("isDefault", e.target.checked)} />
                </label>
              </div>

              <div className="card beautiful-card" style={{ marginBottom: "20px" }}>
                <div className="card-header"><Cpu size={15} style={{ marginRight: 6 }}/> 模型引擎 (Engine & Model)</div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)", fontWeight: 500 }}>LLM 提供商 (Provider)</label>
                  <select className="select-input" value={draft.llmProvider} onChange={e => handleProviderChange(e.target.value)} style={{ width: "100%", maxWidth: "400px" }}>
                    <option value="">跟随通讯录角色</option>
                    {llmProviders.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
                  </select>
                </div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)", fontWeight: 500 }}>模型标识 (Model)</label>
                  <div className="model-select-row" style={{ maxWidth: "600px" }}>
                    {catalogModels.length > 0 ? (
                      <select
                        className="select-input"
                        value={catalogModels.some((model) => model.id === draft.llmModel) ? draft.llmModel : ""}
                        onChange={e => {
                          const value = e.target.value;
                          if (value) handleChange("llmModel", value);
                        }}
                      >
                        <option value="">从目录选择模型</option>
                        {catalogModels.map((model) => (
                          <option key={model.id} value={model.id}>{model.name || model.id}{model.family ? ` (${model.family})` : ""}</option>
                        ))}
                      </select>
                    ) : null}
                    <input
                      className="text-input"
                      value={draft.llmModel}
                      onChange={e => handleChange("llmModel", e.target.value)}
                      placeholder={catalogModels.length > 0 ? "或手动输入模型 ID" : "模型 ID"}
                      style={{ width: "100%" }}
                    />
                  </div>
                </div>
              </div>

              <div className="card beautiful-card" style={{ marginBottom: "20px" }}>
                <div className="card-header"><Wrench size={15} style={{ marginRight: 6 }}/> 局部能力扩展 (Local Capabilities)</div>

                <div className="adapter-row" style={{ padding: "16px 20px", display: "flex", alignItems: "center", justifyContent: "space-between", borderBottom: "1px solid var(--divider)" }}>
                  <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
                    <span className="row-icon blue"><Puzzle size={18} /></span>
                    <div className="adapter-info">
                      <strong>MCP 协议服务</strong>
                      <small>已为当前智能体启用 {draft.enabledMcpServers?.length || 0} 个服务</small>
                    </div>
                  </div>
                  <button className="btn-secondary" onClick={goToLocalMcp}>前往配置</button>
                </div>

                <div className="adapter-row" style={{ padding: "16px 20px", display: "flex", alignItems: "center", justifyContent: "space-between", borderBottom: "1px solid var(--divider)" }}>
                  <div style={{ display: "flex", alignItems: "center", gap: "12px" }}>
                    <span className="row-icon purple"><FolderOpen size={18} /></span>
                    <div className="adapter-info">
                      <strong>Python 技能包 (Skills)</strong>
                      <small>已为当前智能体启用 {draft.enabledSkills?.length || 0} 个技能</small>
                    </div>
                  </div>
                  <button className="btn-secondary" onClick={goToLocalSkills}>前往配置</button>
                </div>

                <div className="adapter-row" style={{ padding: "16px 20px", display: "grid", gridTemplateColumns: "auto 1fr", gap: "12px", borderBottom: "1px solid var(--divider)" }}>
                  <span className="row-icon indigo"><Settings2 size={18} /></span>
                  <div className="adapter-info">
                    <strong>Agent 调度限制</strong>
                    <small>控制当前 Agent 的子任务数量和层级；fallback 工具预算会与绑定角色双向同步</small>
                    <div className="agent-form-row" style={{ marginTop: 12 }}>
                      <div className="agent-field">
                        <label>Fallback 最大工具迭代</label>
                        <input
                          min={1}
                          max={90}
                          type="number"
                          value={draft.maxToolIterations}
                          onChange={e => handleChange("maxToolIterations", clampNumber(Number(e.target.value), 1, 90))}
                        />
                      </div>
                      <div className="agent-field">
                        <label>最大子 Agent</label>
                        <input
                          min={1}
                          max={20}
                          type="number"
                          value={draft.maxSubagents}
                          onChange={e => handleChange("maxSubagents", clampNumber(Number(e.target.value), 1, 20))}
                        />
                      </div>
                      <div className="agent-field">
                        <label>最大子层级</label>
                        <input
                          min={1}
                          max={4}
                          type="number"
                          value={draft.maxSubagentDepth ?? 1}
                          onChange={e => handleChange("maxSubagentDepth", clampNumber(Number(e.target.value), 1, 4))}
                        />
                      </div>
                    </div>
                  </div>
                </div>

                <label className="adapter-row" style={{ cursor: "pointer", padding: "16px 20px", display: "grid", gridTemplateColumns: "auto 1fr auto", background: draft.allowShell ? "var(--error-glow)" : "transparent" }}>
                  <span className="row-icon" style={{ color: draft.allowShell ? "var(--error)" : "var(--text-3)" }}><ShieldAlert size={18} /></span>
                  <div className="adapter-info">
                    <strong style={{ color: draft.allowShell ? "var(--error)" : "inherit" }}>允许终端命令执行 (危险)</strong>
                    <small style={{ color: draft.allowShell ? "var(--error)" : "inherit", opacity: 0.8 }}>授权智能体直接在当前系统执行任意 Shell 命令</small>
                  </div>
                  <input type="checkbox" className="beautiful-checkbox" checked={draft.allowShell} onChange={e => handleChange("allowShell", e.target.checked)} />
                </label>

              </div>

              {/* Delegation Settings */}
              <div className="card beautiful-card" style={{ marginBottom: "20px" }}>
                <div className="card-header"><Users size={15} style={{ marginRight: 6 }}/> 子智能体委派 (Delegation)</div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <div className="agent-form-row">
                    <div className="agent-field">
                      <label>并发子任务</label>
                      <input min={1} max={16} type="number" value={delegationMax} onChange={e => setDelegationMax(clampNumber(Number(e.target.value), 1, 16))} />
                    </div>
                    <div className="agent-field">
                      <label>协作策略</label>
                      <select value={delegationStrategy} onChange={e => setDelegationStrategy(e.target.value)}>
                        <option value="auto">自动平衡</option>
                        <option value="single_agent_chat">单 Agent 对话</option>
                        <option value="router_specialists">路由专家</option>
                        <option value="planner_executor">规划-执行</option>
                        <option value="supervisor_dynamic">动态监督者</option>
                        <option value="peer_handoff">Peer Handoff</option>
                        <option value="mixture_consensus">MoA 共识</option>
                      </select>
                    </div>
                  </div>
                  <div className="agent-form-row" style={{ marginTop: 12 }}>
                    <div className="agent-field">
                      <label>子任务服务商</label>
                      <select value={delegationProviderId} onChange={e => setDelegationProviderId(e.target.value)}>
                        <option value="">继承父 Agent</option>
                        {llmProviders.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
                      </select>
                    </div>
                    <div className="agent-field">
                      <label>子任务模型</label>
                      <input value={delegationModel} onChange={e => setDelegationModel(e.target.value)} placeholder="留空继承父 Agent 模型" />
                    </div>
                  </div>
                </div>
                <label className="adapter-row" style={{ cursor: "pointer", padding: "12px 20px", display: "grid", gridTemplateColumns: "auto 1fr auto", borderTop: "1px solid var(--divider)" }}>
                  <div className="adapter-info"><strong>允许 Orchestrator</strong><small>启用编排器模式协调多 Agent</small></div>
                  <input type="checkbox" className="beautiful-checkbox" checked={delegationOrch} onChange={e => setDelegationOrch(e.target.checked)} />
                </label>
                <label className="adapter-row" style={{ cursor: "pointer", padding: "12px 20px", display: "grid", gridTemplateColumns: "auto 1fr auto", borderTop: "1px solid var(--divider)" }}>
                  <div className="adapter-info"><strong>子任务自动审批</strong><small>关闭时危险工具调用会自动拒绝</small></div>
                  <input type="checkbox" className="beautiful-checkbox" checked={delegationAutoApprove} onChange={e => setDelegationAutoApprove(e.target.checked)} />
                </label>
                <label className="adapter-row" style={{ cursor: "pointer", padding: "12px 20px", display: "grid", gridTemplateColumns: "auto 1fr auto", borderTop: "1px solid var(--divider)" }}>
                  <div className="adapter-info"><strong>继承 MCP 工具</strong><small>子任务保留父 Agent 的 MCP 能力</small></div>
                  <input type="checkbox" className="beautiful-checkbox" checked={delegationInheritMcp} onChange={e => setDelegationInheritMcp(e.target.checked)} />
                </label>
              </div>
            </div>
          ) : (
            <div style={{ height: "100%", display: "flex", alignItems: "center", justifyContent: "center", color: "var(--text-3)" }}>
              <div style={{ textAlign: "center" }}>
                <Bot size={48} style={{ opacity: 0.2, margin: "0 auto 16px" }} />
                <h3>No Agent Selected</h3>
                <p>Select an agent from the sidebar or create a new one.</p>
              </div>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}
