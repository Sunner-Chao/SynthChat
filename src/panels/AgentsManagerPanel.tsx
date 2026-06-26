import React, { useState, useEffect } from "react";
import { Bot, Plus, Trash2, Save, Cpu, Settings2, ShieldAlert, FolderOpen, Wrench, ChevronRight, Puzzle } from "lucide-react";
import { useAppStore } from "../lib/store";
import { AgentDefinition } from "../lib/types";

export function AgentsManagerPanel() {
  const { 
    agents, refreshAgents, saveAgent, deleteAgent, goBack, llmProviders,
    focusedAgentId, setFocusedAgentId, setMcpPanelMode, setSkillsPanelMode, setSection
  } = useAppStore();
  
  const [draft, setDraft] = useState<AgentDefinition | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    void refreshAgents();
  }, [refreshAgents]);

  // Synchronize draft with focusedAgentId
  useEffect(() => {
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

  const handleSelect = (agent: AgentDefinition) => {
    setFocusedAgentId(agent.id);
  };

  const handleCreate = () => {
    const newId = `agent-${Date.now()}`;
    const newAgent: AgentDefinition = {
      id: newId,
      name: "新智能体 (New Agent)",
      description: "A specialized assistant",
      workspaceDir: "",
      llmProvider: "openai",
      llmModel: "gpt-4o",
      enabled: true,
      isDefault: false,
      mcpEnabled: true,
      skillsEnabled: true,
      allowShell: false,
      maxSubagents: 3,
      maxSubagentDepth: 2,
      maxToolIterations: 20,
      skillsDir: "",
      enabledSkills: [],
      enabledMcpServers: [],
      enabledToolsets: [],
      disabledToolsets: [],
      createdAt: new Date().toISOString(),
      updatedAt: new Date().toISOString(),
    };
    // Let's optimisticly set draft first, we don't save to backend yet
    setDraft(newAgent);
    setFocusedAgentId(null); // Clear focus so draft doesn't get overridden by effect
  };

  const handleSave = async () => {
    if (!draft) return;
    setSaving(true);
    try {
      const saved = await saveAgent(draft);
      setFocusedAgentId(saved.id);
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async () => {
    if (!draft || !window.confirm("确定要彻底删除该智能体吗？")) return;
    await deleteAgent(draft.id);
    // focus will be handled by store logic
  };

  const handleChange = (field: keyof AgentDefinition, value: any) => {
    if (draft) {
      setDraft({ ...draft, [field]: value });
    }
  };

  const goToLocalMcp = () => {
    setMcpPanelMode("local");
    setSection("mcp");
  };

  const goToLocalSkills = () => {
    setSkillsPanelMode("local");
    setSection("skills");
  };

  // Determine active item (draft without id match, or focused)
  const isCreatingNew = draft && !agents.find(a => a.id === draft.id);

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
                  <strong style={{ display: "block", marginBottom: 2 }}>{draft.name}</strong>
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
                  <button className="btn-secondary" style={{ color: "var(--error)" }} onClick={handleDelete} title="彻底删除">
                    <Trash2 size={15} style={{ marginRight: 4 }} /> 删除
                  </button>
                  <button className="btn-primary beautiful-btn-primary" onClick={handleSave} disabled={saving}>
                    {saving ? "Saving..." : <><Save size={15} style={{ marginRight: 4 }} /> 保存配置</>}
                  </button>
                </div>
              </div>

              <div className="card beautiful-card" style={{ marginBottom: "20px" }}>
                <div className="card-header"><Settings2 size={15} style={{ marginRight: 6 }}/> 基础配置 (Basic Profile)</div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)", fontWeight: 500 }}>智能体名称 (Name)</label>
                  <input className="text-input" value={draft.name} onChange={e => handleChange("name", e.target.value)} style={{ width: "100%", maxWidth: "400px" }} />
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
                  <select className="select-input" value={draft.llmProvider} onChange={e => handleChange("llmProvider", e.target.value)} style={{ width: "100%", maxWidth: "400px" }}>
                    {llmProviders.map(p => <option key={p.id} value={p.id}>{p.name}</option>)}
                  </select>
                </div>
                <div className="form-group" style={{ padding: "16px 20px" }}>
                  <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)", fontWeight: 500 }}>模型标识 (Model)</label>
                  <input className="text-input" value={draft.llmModel} onChange={e => handleChange("llmModel", e.target.value)} style={{ width: "100%", maxWidth: "400px" }} />
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

                <label className="adapter-row" style={{ cursor: "pointer", padding: "16px 20px", display: "grid", gridTemplateColumns: "auto 1fr auto", background: draft.allowShell ? "var(--error-glow)" : "transparent" }}>
                  <span className="row-icon" style={{ color: draft.allowShell ? "var(--error)" : "var(--text-3)" }}><ShieldAlert size={18} /></span>
                  <div className="adapter-info">
                    <strong style={{ color: draft.allowShell ? "var(--error)" : "inherit" }}>允许终端命令执行 (危险)</strong>
                    <small style={{ color: draft.allowShell ? "var(--error)" : "inherit", opacity: 0.8 }}>授权智能体直接在当前系统执行任意 Shell 命令</small>
                  </div>
                  <input type="checkbox" className="beautiful-checkbox" checked={draft.allowShell} onChange={e => handleChange("allowShell", e.target.checked)} />
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
