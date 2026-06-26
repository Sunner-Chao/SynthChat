import React, { useState } from "react";
import { PlugZap, Plus, Trash2, Edit3, CheckCircle2, XCircle, Server, Command, Globe } from "lucide-react";
import { useAppStore } from "../lib/store";
import { McpServer } from "../lib/types";

export function McpExtensionPanel() {
  const { mcpServers, saveMcpServers, goBack } = useAppStore();
  const [editingId, setEditingId] = useState<string | null>(null);
  const [draftServer, setDraftServer] = useState<Partial<McpServer>>({});

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

  const handleSave = async () => {
    if (!draftServer.id || !draftServer.name || !draftServer.command) return;
    const existing = mcpServers.find(s => s.id === draftServer.id);
    let newServers;
    if (existing) {
      newServers = mcpServers.map(s => s.id === draftServer.id ? { ...s, ...draftServer } as McpServer : s);
    } else {
      newServers = [...mcpServers, draftServer as McpServer];
    }
    await saveMcpServers(newServers);
    setEditingId(null);
  };

  const handleDelete = async (id: string) => {
    await saveMcpServers(mcpServers.filter(s => s.id !== id));
  };

  const toggleEnable = async (server: McpServer) => {
    await saveMcpServers(mcpServers.map(s => s.id === server.id ? { ...s, enabled: !s.enabled } : s));
  };

  return (
    <section className="primary-panel embedded-panel" style={{ background: "var(--background)", display: "flex", flexDirection: "column", height: "100vh" }}>
      <div className="panel-title action-title" style={{ padding: "20px 24px", borderBottom: "1px solid var(--divider)", background: "var(--surface-1)" }}>
        <button className="icon-only-btn" onClick={goBack} title="Back" type="button" style={{ marginRight: 12 }}>
          <ChevronRightIcon />
        </button>
        <div className="panel-title-text" style={{ fontSize: "1.25rem", fontWeight: 600, display: "flex", alignItems: "center", gap: "8px" }}>
          <PlugZap size={22} className="text-primary" />
          <span>MCP Extension Center</span>
        </div>
        <div style={{ flex: 1 }} />
        <button className="primary-btn" onClick={handleAdd} style={{ display: "flex", alignItems: "center", gap: 6 }}>
          <Plus size={16} /> Add Server
        </button>
      </div>

      <div style={{ padding: "24px", overflowY: "auto", flex: 1, display: "flex", flexDirection: "column", gap: "16px" }}>
        {mcpServers.length === 0 && !editingId && (
          <div style={{ textAlign: "center", padding: "64px 20px", color: "var(--text-3)" }}>
            <Server size={48} style={{ opacity: 0.2, margin: "0 auto 16px" }} />
            <h3>No MCP Servers Configured</h3>
            <p>Connect external Model Context Protocol servers to enhance your AI's capabilities.</p>
          </div>
        )}

        {mcpServers.map(server => (
          <div key={server.id} style={{ 
            background: "var(--surface-1)", 
            borderRadius: "12px", 
            border: `1px solid ${server.enabled ? "rgba(34, 211, 238, 0.3)" : "var(--divider)"}`,
            padding: "20px",
            boxShadow: "0 4px 12px rgba(0,0,0,0.05)",
            transition: "all 0.2s"
          }}>
            {editingId === server.id ? (
              <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
                <input className="text-input" placeholder="Server Name" value={draftServer.name || ""} onChange={e => setDraftServer({...draftServer, name: e.target.value})} />
                <input className="text-input" placeholder="Command (e.g. node, python, npx)" value={draftServer.command || ""} onChange={e => setDraftServer({...draftServer, command: e.target.value})} />
                <input className="text-input" placeholder="Args (comma separated)" value={(draftServer.args || []).join(", ")} onChange={e => setDraftServer({...draftServer, args: e.target.value.split(",").map(s => s.trim()).filter(Boolean)})} />
                <select className="select-input" value={draftServer.transport || "stdio"} onChange={e => setDraftServer({...draftServer, transport: e.target.value as any})}>
                  <option value="stdio">stdio</option>
                  <option value="sse">sse</option>
                  <option value="streamable_http">streamable_http</option>
                </select>
                <div style={{ display: "flex", gap: "12px", justifyContent: "flex-end", marginTop: "8px" }}>
                  <button className="secondary-btn" onClick={() => setEditingId(null)}>Cancel</button>
                  <button className="primary-btn" onClick={handleSave}>Save</button>
                </div>
              </div>
            ) : (
              <div style={{ display: "flex", alignItems: "flex-start", gap: "16px" }}>
                <div style={{
                  width: 48, height: 48, borderRadius: "12px", 
                  background: server.enabled ? "rgba(34, 211, 238, 0.1)" : "var(--surface-2)",
                  display: "flex", alignItems: "center", justifyContent: "center",
                  color: server.enabled ? "var(--primary)" : "var(--text-3)"
                }}>
                  <Server size={24} />
                </div>
                <div style={{ flex: 1 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: "12px", marginBottom: "8px" }}>
                    <h3 style={{ margin: 0, fontSize: "1.1rem", color: server.enabled ? "var(--text-1)" : "var(--text-3)" }}>{server.name}</h3>
                    <span style={{ 
                      padding: "2px 8px", borderRadius: "999px", fontSize: "0.75rem", fontWeight: 600,
                      background: server.enabled ? "rgba(34, 211, 238, 0.1)" : "var(--surface-2)",
                      color: server.enabled ? "var(--primary)" : "var(--text-3)"
                    }}>
                      {server.enabled ? "Active" : "Disabled"}
                    </span>
                  </div>
                  <div style={{ display: "flex", alignItems: "center", gap: "16px", color: "var(--text-2)", fontSize: "0.85rem", marginBottom: "16px" }}>
                    <span style={{ display: "flex", alignItems: "center", gap: 4 }}><Command size={14} /> {server.command} {(server.args||[]).join(" ")}</span>
                    <span style={{ display: "flex", alignItems: "center", gap: 4 }}><Globe size={14} /> {server.transport}</span>
                  </div>
                </div>
                <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
                  <button className="icon-btn" onClick={() => toggleEnable(server)} title={server.enabled ? "Disable" : "Enable"}>
                    {server.enabled ? <XCircle size={18} /> : <CheckCircle2 size={18} />}
                  </button>
                  <button className="icon-btn" onClick={() => { setDraftServer(server); setEditingId(server.id); }}>
                    <Edit3 size={18} />
                  </button>
                  <button className="icon-btn" style={{ color: "var(--error)" }} onClick={() => handleDelete(server.id)}>
                    <Trash2 size={18} />
                  </button>
                </div>
              </div>
            )}
          </div>
        ))}

        {editingId && !mcpServers.find(s => s.id === editingId) && (
          <div style={{ background: "var(--surface-1)", borderRadius: "12px", border: "1px solid rgba(34, 211, 238, 0.3)", padding: "20px", boxShadow: "0 4px 12px rgba(0,0,0,0.05)" }}>
            <div style={{ display: "flex", flexDirection: "column", gap: "12px" }}>
              <input className="text-input" placeholder="Server Name" value={draftServer.name || ""} onChange={e => setDraftServer({...draftServer, name: e.target.value})} />
              <input className="text-input" placeholder="Command (e.g. node, python, npx)" value={draftServer.command || ""} onChange={e => setDraftServer({...draftServer, command: e.target.value})} />
              <input className="text-input" placeholder="Args (comma separated)" value={(draftServer.args || []).join(", ")} onChange={e => setDraftServer({...draftServer, args: e.target.value.split(",").map(s => s.trim()).filter(Boolean)})} />
              <select className="select-input" value={draftServer.transport || "stdio"} onChange={e => setDraftServer({...draftServer, transport: e.target.value as any})}>
                <option value="stdio">stdio</option>
                <option value="sse">sse</option>
                <option value="streamable_http">streamable_http</option>
              </select>
              <div style={{ display: "flex", gap: "12px", justifyContent: "flex-end", marginTop: "8px" }}>
                <button className="secondary-btn" onClick={() => setEditingId(null)}>Cancel</button>
                <button className="primary-btn" onClick={handleSave}>Save</button>
              </div>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}

function ChevronRightIcon() {
  return <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ transform: "rotate(180deg)" }}><path d="m9 18 6-6-6-6"/></svg>;
}
