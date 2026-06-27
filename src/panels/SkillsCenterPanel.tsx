import React, { useState, useEffect } from "react";
import { Wand2, Download, CheckCircle2, PackageSearch, Layers, ExternalLink, Settings2, Loader2, Sparkles, ChevronRight, UserSquare2 } from "lucide-react";
import { api } from "../lib/api";
import { useAppStore } from "../lib/store";

export function SkillsCenterPanel() {
  const { 
    skills, marketplaceSkills, 
    refreshSkills, refreshMarketplaceSkills, 
    installMarketplaceSkill, 
    installExternalSkillUrl,
    goBack,
    skillsPanelMode, setSkillsPanelMode,
    focusedAgentId, setFocusedAgentId,
    agents, saveAgent, refreshAgents, refreshSkillsForAgent
  } = useAppStore();
  
  const [activeTab, setActiveTab] = useState<"installed" | "marketplace">("installed");
  const [searchQuery, setSearchQuery] = useState("");
  const [installingId, setInstallingId] = useState<string | null>(null);
  const [installLocalPath, setInstallLocalPath] = useState("");
  const [installUrl, setInstallUrl] = useState("");
  const [installName, setInstallName] = useState("");
  const [installCategory, setInstallCategory] = useState("");
  const [installForce, setInstallForce] = useState(false);
  const [installNotice, setInstallNotice] = useState("");
  const [localEnabledOverride, setLocalEnabledOverride] = useState<Record<string, string[]>>({});
  const [localRuntimeOverride, setLocalRuntimeOverride] = useState<Record<string, boolean>>({});
  const [pendingLocalSkillIds, setPendingLocalSkillIds] = useState<Set<string>>(() => new Set());
  const [runtimePending, setRuntimePending] = useState(false);

  const focusedAgent = agents.find(a => a.id === focusedAgentId) || agents.find(a => a.isDefault) || agents[0];
  const focusedInstallAgentId = focusedAgent?.id || focusedAgentId || undefined;
  const focusedAgentEnabledSkills = focusedAgent
    ? localEnabledOverride[focusedAgent.id] ?? focusedAgent.enabledSkills ?? []
    : [];
  const focusedAgentSkillsRuntime = focusedAgent
    ? localRuntimeOverride[focusedAgent.id] ?? focusedAgent.skillsEnabled
    : false;

  useEffect(() => {
    if (skillsPanelMode === "local") {
      if (focusedAgentId) {
        void refreshSkillsForAgent(focusedAgentId);
      }
      return;
    }
    void refreshSkills();
    if (marketplaceSkills.length === 0) {
      void refreshMarketplaceSkills();
    }
  }, [focusedAgentId, marketplaceSkills.length, refreshMarketplaceSkills, refreshSkills, refreshSkillsForAgent, skillsPanelMode]);

  useEffect(() => {
    if ((!focusedAgentId || !agents.some((agent) => agent.id === focusedAgentId)) && agents.length > 0) {
      setFocusedAgentId((agents.find((agent) => agent.isDefault) ?? agents[0]).id);
    }
  }, [agents, focusedAgentId, setFocusedAgentId]);

  const handleInstall = async (skillId: string) => {
    setInstallingId(skillId);
    try {
      await installMarketplaceSkill(skillId);
      await refreshSkills();
      setActiveTab("installed");
    } catch (e) {
      console.error(e);
      alert("Failed to install skill.");
    } finally {
      setInstallingId(null);
    }
  };

  const handleInstallExternalPath = async () => {
    const sourcePath = installLocalPath.trim();
    if (!sourcePath) return;
    setInstallingId("__local__");
    setInstallNotice("");
    try {
      await api.installExternalSkillFile(
        sourcePath,
        installName.trim() || undefined,
        installCategory.trim() || undefined,
        focusedInstallAgentId,
        installForce
      );
      setInstallNotice("已从本地文件/目录安装");
      setInstallLocalPath("");
      setInstallName("");
      await refreshAgents();
      if (focusedInstallAgentId) {
        await refreshSkillsForAgent(focusedInstallAgentId);
      } else {
        await refreshSkills();
      }
    } catch (error) {
      setInstallNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setInstallingId(null);
    }
  };

  const handleInstallExternalUrl = async () => {
    const url = installUrl.trim();
    if (!url) return;
    setInstallingId("__url__");
    setInstallNotice("");
    try {
      await installExternalSkillUrl(
        url,
        installName.trim() || undefined,
        installCategory.trim() || undefined,
        focusedInstallAgentId,
        installForce
      );
      setInstallNotice("已从 URL 安装");
      setInstallUrl("");
      setInstallName("");
      await refreshAgents();
      if (focusedInstallAgentId) {
        await refreshSkillsForAgent(focusedInstallAgentId);
      } else {
        await refreshSkills();
      }
    } catch (error) {
      setInstallNotice(error instanceof Error ? error.message : String(error));
    } finally {
      setInstallingId(null);
    }
  };

  const toggleLocalSkill = async (skillId: string, enabled: boolean) => {
    if (!focusedAgent) return;
    const currentEnabled = focusedAgentEnabledSkills;
    const newEnabled = enabled
      ? Array.from(new Set([...currentEnabled, skillId]))
      : currentEnabled.filter(id => id !== skillId);

    setLocalEnabledOverride((current) => ({ ...current, [focusedAgent.id]: newEnabled }));
    if (enabled) setLocalRuntimeOverride((current) => ({ ...current, [focusedAgent.id]: true }));
    setPendingLocalSkillIds((current) => new Set(current).add(skillId));
    try {
      const saved = await saveAgent({
        ...focusedAgent,
        enabledSkills: newEnabled,
        skillsEnabled: enabled ? true : focusedAgentSkillsRuntime
      });

      setFocusedAgentId(saved.id);
      setLocalEnabledOverride((current) => ({ ...current, [saved.id]: saved.enabledSkills ?? [] }));
      setLocalRuntimeOverride((current) => ({ ...current, [saved.id]: saved.skillsEnabled }));
      await refreshSkillsForAgent(saved.id);
    } catch (error) {
      setLocalEnabledOverride((current) => ({ ...current, [focusedAgent.id]: focusedAgent.enabledSkills ?? [] }));
      setLocalRuntimeOverride((current) => ({ ...current, [focusedAgent.id]: focusedAgent.skillsEnabled }));
      console.error(error);
      alert(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingLocalSkillIds((current) => {
        const next = new Set(current);
        next.delete(skillId);
        return next;
      });
    }
  };

  const toggleLocalRuntime = async () => {
    if (!focusedAgent) return;
    const nextRuntime = !focusedAgentSkillsRuntime;
    setRuntimePending(true);
    setLocalRuntimeOverride((current) => ({ ...current, [focusedAgent.id]: nextRuntime }));
    try {
      const saved = await saveAgent({
        ...focusedAgent,
        skillsEnabled: nextRuntime
      });
      setFocusedAgentId(saved.id);
      setLocalRuntimeOverride((current) => ({ ...current, [saved.id]: saved.skillsEnabled }));
      await refreshSkillsForAgent(saved.id);
    } catch (error) {
      setLocalRuntimeOverride((current) => ({ ...current, [focusedAgent.id]: focusedAgent.skillsEnabled }));
      console.error(error);
      alert(error instanceof Error ? error.message : String(error));
    } finally {
      setRuntimePending(false);
    }
  };

  const filteredInstalled = skills.filter(s => 
    s.name.toLowerCase().includes(searchQuery.toLowerCase()) || 
    s.description.toLowerCase().includes(searchQuery.toLowerCase())
  );
  
  const filteredMarketplace = marketplaceSkills.filter(s => 
    s.name.toLowerCase().includes(searchQuery.toLowerCase()) || 
    s.description.toLowerCase().includes(searchQuery.toLowerCase())
  );
  const localEnabledCount = focusedAgentEnabledSkills.filter(skillId => skills.some(skill => skill.id === skillId)).length;

  return (
    <section className="primary-panel embedded-panel settings-form mcp-console" style={{ display: "flex", flexDirection: "column", height: "100%", padding: 0 }}>
      
      {/* Header */}
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button">
          <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
        </button>
        <div className="panel-title-text">
          <Wand2 size={16} className="panel-title-icon" />
          <span>Skills</span>
          <strong>技能中心 (Python Plugins)</strong>
        </div>
      </div>

      {/* Global / Local Mode Switch */}
      <div style={{ padding: "16px 24px", display: "flex", gap: "16px", alignItems: "center", borderBottom: "1px solid var(--divider)", background: "var(--surface-1)" }}>
        <div className="segmented-control">
          <button 
            className={`segmented-btn ${skillsPanelMode === "global" ? "active" : ""}`} 
            onClick={() => setSkillsPanelMode("global")}
          >
            <Settings2 size={15} /> 全局管理 (Global)
          </button>
          <button 
            className={`segmented-btn ${skillsPanelMode === "local" ? "active" : ""}`} 
            onClick={() => setSkillsPanelMode("local")}
          >
            <UserSquare2 size={15} /> 局部配置 (Local)
          </button>
        </div>
        
        {/* Search input is shared across both modes */}
        <input 
          type="text" 
          className="text-input" 
          placeholder={skillsPanelMode === "global" ? "搜索全局技能..." : "搜索本地技能..."}
          value={searchQuery}
          onChange={(e) => setSearchQuery(e.target.value)}
          style={{ flex: 1, maxWidth: "300px", marginLeft: "auto" }}
        />
      </div>

      {/* Content Area */}
      <div style={{ flex: 1, overflowY: "auto", padding: "24px", background: "var(--background)" }}>
        
        {skillsPanelMode === "global" ? (
          <>
            <div className="skill-config-panel" style={{ margin: "0 0 16px 0" }}>
              <div className="card-header" style={{ marginBottom: 12 }}>
                <Download size={15} /> 安装 Skill
              </div>
              <div className="settings-form">
                <div className="form-row">
                  <label>本地路径</label>
                  <input
                    value={installLocalPath}
                    onChange={(event) => setInstallLocalPath(event.target.value)}
                    onKeyDown={(event) => { if (event.key === "Enter") void handleInstallExternalPath(); }}
                    placeholder="D:\\path\\to\\skill 或 D:\\path\\to\\SKILL.md"
                  />
                </div>
                <div className="form-row">
                  <label>公开 URL</label>
                  <input
                    value={installUrl}
                    onChange={(event) => setInstallUrl(event.target.value)}
                    onKeyDown={(event) => { if (event.key === "Enter") void handleInstallExternalUrl(); }}
                    placeholder="GitHub blob/tree、raw SKILL.md 或 skills.sh 具体技能页"
                  />
                </div>
                <div className="agent-form-row">
                  <div className="agent-field">
                    <label>名称</label>
                    <input value={installName} onChange={(event) => setInstallName(event.target.value)} placeholder="留空使用 frontmatter" />
                  </div>
                  <div className="agent-field">
                    <label>分类</label>
                    <input value={installCategory} onChange={(event) => setInstallCategory(event.target.value)} placeholder="例如 media / research / coding" />
                  </div>
                </div>
                <label className="checkbox-row">
                  <input type="checkbox" checked={installForce} onChange={(event) => setInstallForce(event.target.checked)} />
                  <span>允许覆盖审计阻断或同名 skill</span>
                </label>
                <div className="inline-actions">
                  <button className="btn-primary beautiful-btn-primary" onClick={handleInstallExternalPath} disabled={installingId !== null || !installLocalPath.trim()} type="button">
                    <Download size={14} /> 本地导入
                  </button>
                  <button className="btn-primary beautiful-btn-primary" onClick={handleInstallExternalUrl} disabled={installingId !== null || !installUrl.trim()} type="button">
                    <ExternalLink size={14} /> URL 安装
                  </button>
                </div>
                <p className="form-hint">离线导入会复制到应用数据目录；URL 安装支持 GitHub blob/tree 页面、raw SKILL.md、skills.sh 具体技能页，不能直接填 skills.sh 首页。</p>
                {installNotice ? <p className="form-hint">{installNotice}</p> : null}
              </div>
            </div>

            <div style={{ display: "flex", gap: "4px", marginBottom: "20px", borderBottom: "1px solid var(--divider)", paddingBottom: "12px" }}>
              <button
                onClick={() => setActiveTab("installed")}
                style={{
                  flex: 1,
                  display: "flex", alignItems: "center", justifyContent: "center", gap: 6,
                  padding: "8px 16px",
                  border: "none",
                  borderRadius: "var(--radius-md)",
                  fontSize: 14, fontWeight: 500,
                  cursor: "pointer",
                  transition: "all 0.2s ease",
                  background: activeTab === "installed" ? "var(--primary)" : "transparent",
                  color: activeTab === "installed" ? "#fff" : "var(--text-2)",
                }}
              >
                <Layers size={15} /> 已安装 ({skills.length})
              </button>
              <button
                onClick={() => setActiveTab("marketplace")}
                style={{
                  flex: 1,
                  display: "flex", alignItems: "center", justifyContent: "center", gap: 6,
                  padding: "8px 16px",
                  border: "none",
                  borderRadius: "var(--radius-md)",
                  fontSize: 14, fontWeight: 500,
                  cursor: "pointer",
                  transition: "all 0.2s ease",
                  background: activeTab === "marketplace" ? "var(--primary)" : "transparent",
                  color: activeTab === "marketplace" ? "#fff" : "var(--text-2)",
                }}
              >
                <PackageSearch size={15} /> 插件市场
              </button>
            </div>

            {activeTab === "installed" && (
              <div className="card-list">
                {filteredInstalled.length === 0 ? (
                  <div className="card beautiful-card" style={{ padding: "48px 24px", textAlign: "center", borderStyle: "dashed" }}>
                    <Layers size={48} style={{ opacity: 0.2, margin: "0 auto 16px" }} />
                    <h3 style={{ margin: "0 0 8px 0", color: "var(--text-1)" }}>未找到已安装的技能</h3>
                    <p style={{ margin: "0 0 24px 0", color: "var(--text-3)", fontSize: "0.9rem" }}>去插件市场逛逛，给大模型安装一些强大的本地能力吧！</p>
                    <button className="btn-primary beautiful-btn-primary" onClick={() => setActiveTab("marketplace")}>
                      <PackageSearch size={16} style={{ marginRight: 6 }} /> 浏览市场
                    </button>
                  </div>
                ) : (
                  filteredInstalled.map((skill) => (
                    <div key={skill.id} className="card beautiful-card" style={{ marginBottom: "16px" }}>
                      <div className="card-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                        <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                          <Wand2 size={15} className="text-primary" /> {skill.name}
                        </div>
                        {skill.version && <span style={{ fontSize: "0.75rem", background: "var(--surface-3)", padding: "2px 8px", borderRadius: "12px", color: "var(--text-2)" }}>v{skill.version}</span>}
                      </div>
                      <div className="form-group" style={{ padding: "16px 20px" }}>
                        <p style={{ margin: "0 0 16px 0", color: "var(--text-2)", fontSize: "0.9rem", lineHeight: "1.5" }}>
                          {skill.description || "无描述"}
                        </p>
                        {skill.author && (
                          <div style={{ display: "inline-flex", alignItems: "center", gap: "4px", fontSize: "0.8rem", color: "var(--text-3)", marginRight: "16px" }}>
                            <ExternalLink size={12} /> {skill.author}
                          </div>
                        )}
                      </div>
                    </div>
                  ))
                )}
              </div>
            )}

            {activeTab === "marketplace" && (
              <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(320px, 1fr))", gap: "16px" }}>
                {filteredMarketplace.length === 0 ? (
                  <div style={{ gridColumn: "1 / -1", textAlign: "center", padding: "48px 24px", color: "var(--text-3)" }}>
                    没有找到匹配的技能
                  </div>
                ) : (
                  filteredMarketplace.map((skill) => {
                    const isInstalled = skills.some(s => s.id === skill.id);
                    const isInstalling = installingId === skill.id;
                    
                    return (
                      <div key={skill.id} className="card beautiful-card" style={{ display: "flex", flexDirection: "column", height: "100%" }}>
                        <div className="card-header" style={{ borderBottom: "none", paddingBottom: 0 }}>
                          <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", marginBottom: "8px" }}>
                            <h3 style={{ margin: 0, fontSize: "1.1rem", display: "flex", alignItems: "center", gap: "8px" }}>
                              <Sparkles size={16} style={{ color: "var(--primary)" }}/> {skill.name}
                            </h3>
                            {skill.version && <span style={{ fontSize: "0.75rem", background: "var(--surface-2)", padding: "2px 8px", borderRadius: "12px", color: "var(--text-2)" }}>v{skill.version}</span>}
                          </div>
                        </div>
                        
                        <div className="form-group" style={{ flex: 1, padding: "16px", borderBottom: "none" }}>
                          <p style={{ margin: 0, color: "var(--text-2)", fontSize: "0.85rem", lineHeight: "1.5", display: "-webkit-box", WebkitLineClamp: 3, WebkitBoxOrient: "vertical", overflow: "hidden" }}>
                            {skill.description}
                          </p>
                        </div>
                        
                        <div style={{ padding: "16px", borderTop: "1px solid var(--divider)", display: "flex", justifyContent: "space-between", alignItems: "center", background: "var(--surface-1)", marginTop: "auto" }}>
                          <div style={{ fontSize: "0.8rem", color: "var(--text-3)" }}>
                            {skill.author || "社区提供"}
                          </div>
                          
                          {isInstalled ? (
                            <span style={{ display: "flex", alignItems: "center", gap: "6px", fontSize: "0.85rem", color: "var(--success)" }}>
                              <CheckCircle2 size={16} /> 已安装
                            </span>
                          ) : (
                            <button 
                              className="btn-primary beautiful-btn-primary" 
                              onClick={() => handleInstall(skill.id)}
                              disabled={isInstalling}
                              style={{ padding: "6px 16px", fontSize: "0.85rem", display: "flex", gap: "6px", alignItems: "center" }}
                            >
                              {isInstalling ? <><Loader2 size={14} className="spin" /> 安装中</> : <><Download size={14} /> 安装</>}
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            )}
          </>
        ) : (
          /* ========================================= */
          /* LOCAL SKILLS CONFIGURATION                */
          /* ========================================= */
          <>
            {agents.length === 0 ? (
              <div style={{ textAlign: "center", padding: "40px", color: "var(--text-3)" }}>
                请先创建智能体，再为其配置局部技能。
              </div>
            ) : (
              <div style={{ display: "flex", gap: "24px", alignItems: "flex-start" }}>
                {/* Agent Selector Sidebar */}
                <div className="beautiful-sidebar card beautiful-card" style={{ width: "240px", flexShrink: 0 }}>
                  <div className="card-header" style={{ fontSize: "0.9rem" }}>选择智能体</div>
                  <div style={{ padding: "8px 0" }}>
                    {agents.map(agent => (
                      <div 
                        key={agent.id}
                        onClick={() => setFocusedAgentId(agent.id)}
                        className={`adapter-row beautiful-row ${focusedAgentId === agent.id ? "active" : ""}`}
                        style={{ cursor: "pointer", padding: "10px 16px", display: "grid", gridTemplateColumns: "auto 1fr" }}
                      >
                        <span className="row-icon indigo"><UserSquare2 size={16} /></span>
                        <div className="adapter-info">
                          <strong>{agent.name}</strong>
                        </div>
                      </div>
                    ))}
                  </div>
                </div>

                {/* Local Config Area */}
                {focusedAgent ? (
                  <div className="card beautiful-card" style={{ flex: 1 }}>
                    <div className="card-header" style={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                      <span>为 <strong>{focusedAgent.name}</strong> 启用 Python 技能</span>
                      <small style={{ fontWeight: "normal", color: "var(--text-3)" }}>{localEnabledCount} / {skills.length} 已启用</small>
                    </div>
                    <div style={{ padding: "14px 20px", borderBottom: "1px solid var(--divider)", display: "flex", alignItems: "center", justifyContent: "space-between", gap: "12px", flexWrap: "wrap", background: "var(--surface-1)" }}>
                      <div style={{ display: "flex", flexDirection: "column", gap: "4px" }}>
                        <strong style={{ fontSize: "0.9rem", color: "var(--text-1)" }}>Skills Runtime</strong>
                        <small style={{ color: "var(--text-3)" }}>
                          {focusedAgentSkillsRuntime ? "已开启，勾选的技能会参与当前智能体的技能加载。" : "当前已关闭；仅勾选列表还不算真正启用。"}
                        </small>
                      </div>
                      <button className={focusedAgentSkillsRuntime ? "btn-primary beautiful-btn-primary" : "btn-secondary"} disabled={runtimePending} onClick={() => void toggleLocalRuntime()} type="button">
                        {focusedAgentSkillsRuntime ? "已开启" : "开启 Runtime"}
                      </button>
                    </div>
                    
                    {skills.length === 0 ? (
                      <div style={{ padding: "32px", textAlign: "center", color: "var(--text-3)" }}>
                        全局暂无可用技能，请先切换到全局配置去市场下载。
                      </div>
                    ) : filteredInstalled.length === 0 ? (
                      <div style={{ padding: "32px", textAlign: "center", color: "var(--text-3)" }}>
                        没有匹配的技能搜索结果
                      </div>
                    ) : (
                      <div style={{ padding: "16px", display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))", gap: "12px", background: "var(--surface-1)" }}>
                        {filteredInstalled.map((skill) => {
                          const isLocalEnabled = focusedAgentEnabledSkills.includes(skill.id);
                          return (
                            <div
                              key={skill.id} 
                              className={`agent-toggle-item beautiful-row ${isLocalEnabled ? "active" : ""}`} 
                              onClick={() => {
                                if (!pendingLocalSkillIds.has(skill.id)) {
                                  void toggleLocalSkill(skill.id, !isLocalEnabled);
                                }
                              }}
                              style={{ 
                                cursor: "pointer", 
                                display: "flex", 
                                alignItems: "center", 
                                justifyContent: "space-between",
                                padding: "12px 16px",
                                background: isLocalEnabled ? "var(--primary-light)" : "var(--card)"
                              }}
                            >
                              <div style={{ display: "flex", alignItems: "center", gap: "10px", flex: 1, minWidth: 0 }}>
                                <Sparkles size={16} style={{ color: isLocalEnabled ? "var(--primary)" : "var(--text-3)", flexShrink: 0 }} />
                                <div style={{ overflow: "hidden" }}>
                                  <div style={{ fontWeight: 500, fontSize: "0.9rem", color: isLocalEnabled ? "var(--primary)" : "var(--text-1)", textOverflow: "ellipsis", whiteSpace: "nowrap", overflow: "hidden" }}>{skill.name}</div>
                                  <div style={{ fontSize: "0.75rem", color: "var(--text-3)", textOverflow: "ellipsis", whiteSpace: "nowrap", overflow: "hidden" }}>
                                    {skill.description}
                                    {!focusedAgentSkillsRuntime && isLocalEnabled ? " · Runtime 未开启" : ""}
                                  </div>
                                </div>
                              </div>
                              <input 
                                type="checkbox" 
                                className="beautiful-checkbox"
                                checked={isLocalEnabled} 
                                disabled={pendingLocalSkillIds.has(skill.id)}
                                onClick={(event) => event.stopPropagation()}
                                onChange={e => void toggleLocalSkill(skill.id, e.target.checked)}
                                style={{ marginLeft: "12px", flexShrink: 0 }} 
                              />
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                ) : null}
              </div>
            )}
          </>
        )}
      </div>
    </section>
  );
}
