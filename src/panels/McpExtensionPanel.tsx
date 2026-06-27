import React, { useState, useEffect } from "react";
import { PlugZap, Plus, Trash2, Edit3, Server, Command, Globe, ChevronRight, Settings2, UserSquare2 } from "lucide-react";
import { useAppStore } from "../lib/store";
import { McpServer } from "../lib/types";

export function McpExtensionPanel() {
  const { 
    mcpServers, saveMcpServers, goBack, 
    mcpPanelMode, setMcpPanelMode,
    focusedAgentId, setFocusedAgentId,
    agents, saveAgent
  } = useAppStore();
  
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draftServer, setDraftServer] = useState<Partial<McpServer>>({});
  const [pendingGlobalIds, setPendingGlobalIds] = useState<Set<string>>(() => new Set());
  const [localEnabledOverride, setLocalEnabledOverride] = useState<Record<string, string[]>>({});
  const [pendingLocalIds, setPendingLocalIds] = useState<Set<string>>(() => new Set());

  const focusedAgent = agents.find(a => a.id === focusedAgentId) || agents.find(a => a.isDefault) || agents[0];
  const focusedAgentEnabledMcpRaw = focusedAgent
    ? localEnabledOverride[focusedAgent.id] ?? focusedAgent.enabledMcpServers ?? []
    : [];
  // Filter out stale refs (server IDs that no longer exist in global mcpServers)
  const focusedAgentEnabledMcp = focusedAgentEnabledMcpRaw.filter(id => mcpServers.some(s => s.id === id));

  useEffect(() => {
    if ((!focusedAgentId || !agents.some((agent) => agent.id === focusedAgentId)) && agents.length > 0) {
      setFocusedAgentId((agents.find((agent) => agent.isDefault) ?? agents[0]).id);
    }
  }, [agents, focusedAgentId, setFocusedAgentId]);

  const handleAdd = () => {
    const newId = `mcp-${Date.now()}`;
    setDraftServer({
      id: newId,
      name: "New Server",
      transport: "stdio",
      command: "node",
      args: [],
      protocol: "mcpJsonRpc",
      enabled: true,
      timeoutSeconds: 60
    });
    setEditingId(newId);
  };

  const handleAddPlaywright = () => {
    const newId = `playwright-${Date.now()}`;
    setDraftServer({
      id: newId,
      name: "Playwright MCP",
      transport: "stdio",
      command: "npx",
      args: ["-y", "@playwright/mcp@latest"],
      protocol: "mcpJsonRpc",
      enabled: true,
      timeoutSeconds: 120,
      persistentSession: true,
      keepAlive: true,
      keepAliveIntervalSeconds: 300,
      keepAliveTimeoutSeconds: 30
    });
    setEditingId(newId);
  };

  const handleSaveGlobal = async () => {
    if (!draftServer.id) return;
    let newServers = [...mcpServers];
    const idx = newServers.findIndex(s => s.id === draftServer.id);
    if (idx >= 0) {
      newServers[idx] = draftServer as McpServer;
    } else {
      newServers.push(draftServer as McpServer);
    }
    await saveMcpServers(newServers);
    setEditingId(null);
  };

  const handleDeleteGlobal = async (id: string) => {
    if (!window.confirm("确定要删除这个 MCP Server 吗？")) return;
    const newServers = mcpServers.filter(s => s.id !== id);
    await saveMcpServers(newServers);
  };

  const toggleEnableGlobal = async (id: string, enabled: boolean) => {
    const newServers = mcpServers.map(s => s.id === id ? { ...s, enabled } : s);
    setPendingGlobalIds((current) => new Set(current).add(id));
    try {
      await saveMcpServers(newServers);
    } catch (error) {
      console.error(error);
      alert(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingGlobalIds((current) => {
        const next = new Set(current);
        next.delete(id);
        return next;
      });
    }
  };

  const toggleLocalMcp = async (serverId: string, enabled: boolean) => {
    if (!focusedAgent) return;
    const currentEnabled = focusedAgentEnabledMcp;
    const newEnabled = enabled 
      ? Array.from(new Set([...currentEnabled, serverId]))
      : currentEnabled.filter(id => id !== serverId);

    setLocalEnabledOverride((current) => ({ ...current, [focusedAgent.id]: newEnabled }));
    setPendingLocalIds((current) => new Set(current).add(serverId));
    try {
      const saved = await saveAgent({
        ...focusedAgent,
        enabledMcpServers: newEnabled
      });
      setLocalEnabledOverride((current) => ({ ...current, [saved.id]: saved.enabledMcpServers ?? [] }));
    } catch (error) {
      setLocalEnabledOverride((current) => ({ ...current, [focusedAgent.id]: focusedAgent.enabledMcpServers ?? [] }));
      console.error(error);
      alert(error instanceof Error ? error.message : String(error));
    } finally {
      setPendingLocalIds((current) => {
        const next = new Set(current);
        next.delete(serverId);
        return next;
      });
    }
  };

  return (
    <section className="primary-panel embedded-panel settings-form mcp-console" style={{ display: "flex", flexDirection: "column", height: "100%", padding: 0 }}>
      <div className="panel-title action-title">
        <button className="icon-only-btn" onClick={goBack} title="返回" type="button">
          <ChevronRight size={19} style={{ transform: "rotate(180deg)" }} />
        </button>
        <div className="panel-title-text">
          <PlugZap size={16} className="panel-title-icon" />
          <span>MCP</span>
          <strong>协议扩展配置 (Model Context Protocol)</strong>
        </div>
      </div>

      <div style={{ padding: "16px 24px", display: "flex", gap: "16px", alignItems: "center", borderBottom: "1px solid var(--divider)", background: "var(--surface-1)" }}>
        <div className="segmented-control">
          <button 
            className={`segmented-btn ${mcpPanelMode === "global" ? "active" : ""}`} 
            onClick={() => setMcpPanelMode("global")}
          >
            <Settings2 size={15} /> 全局配置 (Global)
          </button>
          <button 
            className={`segmented-btn ${mcpPanelMode === "local" ? "active" : ""}`} 
            onClick={() => setMcpPanelMode("local")}
          >
            <UserSquare2 size={15} /> 局部配置 (Local)
          </button>
        </div>
      </div>

      <div style={{ flex: 1, overflowY: "auto", padding: "24px", background: "var(--background)" }}>
        
        {mcpPanelMode === "global" ? (
          /* ========================================= */
          /* GLOBAL MCP CONFIGURATION                  */
          /* ========================================= */
          <>
            {mcpServers.length === 0 && !editingId ? (
              <div className="card beautiful-card" style={{ padding: "48px 24px", textAlign: "center", borderStyle: "dashed" }}>
                <Server size={48} style={{ opacity: 0.2, margin: "0 auto 16px" }} />
                <h3 style={{ margin: "0 0 8px 0", color: "var(--text-1)" }}>没有配置任何 MCP Server</h3>
                <p style={{ margin: "0 0 24px 0", color: "var(--text-3)", fontSize: "0.9rem" }}>MCP 允许大模型连接到你的本地系统、外部 API、或企业级工具。</p>
                <button className="btn-primary beautiful-btn-primary" onClick={handleAdd}>
                  <Plus size={16} style={{ marginRight: 6 }} /> 添加第一个服务
                </button>
              </div>
            ) : (
              <div className="card-list">
                <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 16 }}>
                  <div>
                    <h3 style={{ margin: 0, fontSize: "1rem", color: "var(--text-1)" }}>全局已安装的服务 (Global Servers)</h3>
                    <small style={{ color: "var(--text-3)" }}>这些服务对所有智能体可见，但需在局部配置中为每个智能体单独勾选启用。</small>
                  </div>
                  <div className="inline-actions">
                    <button className="btn-secondary" onClick={handleAddPlaywright}><Command size={15} style={{ marginRight: 4 }}/> Playwright</button>
                    <button className="btn-secondary" onClick={handleAdd}><Plus size={15} style={{ marginRight: 4 }}/> 添加服务</button>
                  </div>
                </div>
                
                {mcpServers.map(server => (
                  <div key={server.id} className="card beautiful-card" style={{ marginBottom: "16px" }}>
                    <div className="adapter-row" style={{ padding: "16px", cursor: "default" }}>
                      <span className={`row-icon ${server.enabled ? "indigo" : ""}`} style={{ color: server.enabled ? "var(--primary)" : "var(--text-3)" }}>
                        {server.transport === "stdio" ? <Command size={18} /> : <Globe size={18} />}
                      </span>
                      <div className="adapter-info">
                        <strong style={{ color: server.enabled ? "inherit" : "var(--text-3)" }}>{server.name}</strong>
                        <small style={{ color: server.enabled ? "inherit" : "var(--text-3)" }}>{server.transport === "stdio" ? `${server.command} ${server.args.join(" ")}` : server.url}</small>
                      </div>
                      <div style={{ display: "flex", gap: "12px", alignItems: "center" }}>
                        <label style={{ display: "flex", alignItems: "center", gap: "6px", cursor: "pointer", fontSize: "0.85rem", color: "var(--text-2)" }}>
                          主开关
                          <input type="checkbox" className="beautiful-checkbox" checked={server.enabled} disabled={pendingGlobalIds.has(server.id)} onChange={e => toggleEnableGlobal(server.id, e.target.checked)} />
                        </label>
                        <button className="icon-btn" onClick={() => { setDraftServer(server); setEditingId(server.id); }} title="编辑">
                          <Edit3 size={15} />
                        </button>
                        <button className="icon-btn" onClick={() => handleDeleteGlobal(server.id)} style={{ color: "var(--error)" }} title="删除">
                          <Trash2 size={15} />
                        </button>
                      </div>
                    </div>

                    {editingId === server.id && (
                      <div style={{ borderTop: "1px solid var(--divider)", padding: "16px", background: "var(--surface-1)" }}>
                        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "16px", marginBottom: "16px" }}>
                          <div className="form-group">
                            <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>服务名称</label>
                            <input className="text-input" value={draftServer.name || ""} onChange={e => setDraftServer({ ...draftServer, name: e.target.value })} style={{ width: "100%" }} />
                          </div>
                          <div className="form-group">
                            <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>通信方式 (Transport)</label>
                            <select className="select-input" value={draftServer.transport || "stdio"} onChange={e => setDraftServer({ ...draftServer, transport: e.target.value as any })} style={{ width: "100%" }}>
                              <option value="stdio">Stdio (本地进程)</option>
                              <option value="sse">SSE (远程 HTTP)</option>
                            </select>
                          </div>
                        </div>
                        {draftServer.transport === "stdio" ? (
                          <div style={{ display: "grid", gridTemplateColumns: "1fr 2fr", gap: "16px", marginBottom: "16px" }}>
                            <div className="form-group">
                              <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>命令 (Command)</label>
                              <input className="text-input" value={draftServer.command || ""} onChange={e => setDraftServer({ ...draftServer, command: e.target.value })} style={{ width: "100%" }} placeholder="e.g. npx" />
                            </div>
                            <div className="form-group">
                              <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>参数 (Arguments, 空格分隔)</label>
                              <input className="text-input" value={(draftServer.args || []).join(" ")} onChange={e => setDraftServer({ ...draftServer, args: e.target.value.split(" ").filter(Boolean) })} style={{ width: "100%" }} placeholder="-y @modelcontextprotocol/server-github" />
                            </div>
                          </div>
                        ) : (
                          <div className="form-group" style={{ marginBottom: "16px" }}>
                            <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>SSE URL</label>
                            <input className="text-input" value={draftServer.url || ""} onChange={e => setDraftServer({ ...draftServer, url: e.target.value })} style={{ width: "100%" }} placeholder="http://localhost:3000/sse" />
                          </div>
                        )}
                        <div style={{ display: "flex", justifyContent: "flex-end", gap: "8px" }}>
                          <button className="btn-secondary" onClick={() => setEditingId(null)}>取消</button>
                          <button className="btn-primary beautiful-btn-primary" onClick={handleSaveGlobal}>保存更改</button>
                        </div>
                      </div>
                    )}
                  </div>
                ))}
                
                {/* New Server Draft */}
                {editingId && !mcpServers.find(s => s.id === editingId) && (
                  <div className="card beautiful-card" style={{ marginBottom: "16px" }}>
                    <div className="card-header">添加新的 MCP 服务</div>
                    <div style={{ padding: "16px", background: "var(--surface-1)" }}>
                      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "16px", marginBottom: "16px" }}>
                        <div className="form-group">
                          <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>服务名称</label>
                          <input className="text-input" value={draftServer.name || ""} onChange={e => setDraftServer({ ...draftServer, name: e.target.value })} style={{ width: "100%" }} />
                        </div>
                        <div className="form-group">
                          <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>通信方式 (Transport)</label>
                          <select className="select-input" value={draftServer.transport || "stdio"} onChange={e => setDraftServer({ ...draftServer, transport: e.target.value as any })} style={{ width: "100%" }}>
                            <option value="stdio">Stdio (本地进程)</option>
                            <option value="sse">SSE (远程 HTTP)</option>
                          </select>
                        </div>
                      </div>
                      {draftServer.transport === "stdio" ? (
                        <div style={{ display: "grid", gridTemplateColumns: "1fr 2fr", gap: "16px", marginBottom: "16px" }}>
                          <div className="form-group">
                            <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>命令 (Command)</label>
                            <input className="text-input" value={draftServer.command || ""} onChange={e => setDraftServer({ ...draftServer, command: e.target.value })} style={{ width: "100%" }} placeholder="e.g. npx" />
                          </div>
                          <div className="form-group">
                            <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>参数 (Arguments, 空格分隔)</label>
                            <input className="text-input" value={(draftServer.args || []).join(" ")} onChange={e => setDraftServer({ ...draftServer, args: e.target.value.split(" ").filter(Boolean) })} style={{ width: "100%" }} placeholder="-y @modelcontextprotocol/server-github" />
                          </div>
                        </div>
                      ) : (
                        <div className="form-group" style={{ marginBottom: "16px" }}>
                          <label style={{ display: "block", marginBottom: 6, fontSize: "0.85rem", color: "var(--text-2)" }}>SSE URL</label>
                          <input className="text-input" value={draftServer.url || ""} onChange={e => setDraftServer({ ...draftServer, url: e.target.value })} style={{ width: "100%" }} placeholder="http://localhost:3000/sse" />
                        </div>
                      )}
                      <div style={{ display: "flex", justifyContent: "flex-end", gap: "8px" }}>
                        <button className="btn-secondary" onClick={() => setEditingId(null)}>取消</button>
                        <button className="btn-primary beautiful-btn-primary" onClick={handleSaveGlobal}>保存并添加</button>
                      </div>
                    </div>
                  </div>
                )}
                
              </div>
            )}
          </>
        ) : (
          /* ========================================= */
          /* LOCAL MCP CONFIGURATION                   */
          /* ========================================= */
          <>
            {agents.length === 0 ? (
              <div style={{ textAlign: "center", padding: "40px", color: "var(--text-3)" }}>
                请先创建智能体，再为其配置局部工具。
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
                      <span>为 <strong>{focusedAgent.name}</strong> 启用 MCP 服务</span>
                      <small style={{ fontWeight: "normal", color: "var(--text-3)" }}>{focusedAgentEnabledMcp.length || 0} / {mcpServers.length} 已启用</small>
                    </div>
                    {mcpServers.length === 0 ? (
                      <div style={{ padding: "32px", textAlign: "center", color: "var(--text-3)" }}>
                        全局暂无 MCP 服务，请先切换到全局配置进行添加。
                      </div>
                    ) : (
                      <div style={{ padding: "0" }}>
                        {mcpServers.map((server, index) => {
                          const isLocalEnabled = focusedAgentEnabledMcp.includes(server.id);
                          const isGlobalEnabled = server.enabled;
                          return (
                            <div
                              key={server.id} 
                              className={`adapter-row ${isLocalEnabled ? "active" : ""}`}
                              onClick={() => {
                                if (isGlobalEnabled && !pendingLocalIds.has(server.id)) {
                                  void toggleLocalMcp(server.id, !isLocalEnabled);
                                }
                              }}
                              style={{ 
                                cursor: isGlobalEnabled ? "pointer" : "not-allowed", 
                                padding: "16px 20px", 
                                display: "grid", 
                                gridTemplateColumns: "auto 1fr auto",
                                margin: 0,
                                borderBottom: index < mcpServers.length - 1 ? "1px solid var(--divider)" : "none",
                                opacity: isGlobalEnabled ? 1 : 0.6
                              }}
                            >
                              <span className={`row-icon ${isLocalEnabled ? "indigo" : ""}`} style={{ color: isLocalEnabled ? "var(--primary)" : "var(--text-3)" }}>
                                {server.transport === "stdio" ? <Command size={18} /> : <Globe size={18} />}
                              </span>
                              <div className="adapter-info">
                                <strong>{server.name} {!isGlobalEnabled && <span style={{ color: "var(--error)", fontSize: "0.8rem", fontWeight: "normal" }}>(全局未启用)</span>}</strong>
                                <small>{server.transport === "stdio" ? `${server.command} ${server.args.join(" ")}` : server.url}</small>
                              </div>
                              <input 
                                type="checkbox" 
                                className="beautiful-checkbox"
                                checked={isLocalEnabled} 
                                disabled={!isGlobalEnabled || pendingLocalIds.has(server.id)}
                                onClick={(event) => event.stopPropagation()}
                                onChange={e => void toggleLocalMcp(server.id, e.target.checked)}
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
