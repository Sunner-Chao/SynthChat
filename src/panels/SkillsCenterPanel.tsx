import React, { useState, useEffect } from "react";
import { Wand2, Download, Trash2, CheckCircle2, PackageSearch, Layers, ExternalLink, Settings2, Loader2, Sparkles } from "lucide-react";
import { useAppStore } from "../lib/store";
import { EnhancedSkillSummary, MarketplaceSkill } from "../lib/types";

export function SkillsCenterPanel() {
  const { 
    skills, marketplaceSkills, 
    refreshSkills, refreshMarketplaceSkills, 
    installMarketplaceSkill, 
    goBack 
  } = useAppStore();
  
  const [activeTab, setActiveTab] = useState<"installed" | "marketplace">("installed");
  const [searchQuery, setSearchQuery] = useState("");
  const [installingId, setInstallingId] = useState<string | null>(null);

  useEffect(() => {
    void refreshSkills();
    if (marketplaceSkills.length === 0) {
      void refreshMarketplaceSkills();
    }
  }, []);

  const handleInstall = async (skillId: string) => {
    setInstallingId(skillId);
    try {
      await installMarketplaceSkill(skillId);
      setActiveTab("installed");
    } catch (e) {
      console.error(e);
    } finally {
      setInstallingId(null);
    }
  };

  const filteredMarketplace = marketplaceSkills.filter(s => 
    s.name.toLowerCase().includes(searchQuery.toLowerCase()) || 
    s.description.toLowerCase().includes(searchQuery.toLowerCase()) ||
    (s.tags || []).some(t => t.toLowerCase().includes(searchQuery.toLowerCase()))
  );

  return (
    <section className="primary-panel embedded-panel" style={{ background: "var(--background)", display: "flex", flexDirection: "column", height: "100vh" }}>
      <div className="panel-title action-title" style={{ padding: "20px 24px", borderBottom: "1px solid var(--divider)", background: "var(--surface-1)" }}>
        <button className="icon-only-btn" onClick={goBack} title="Back" type="button" style={{ marginRight: 12 }}>
          <ChevronRightIcon />
        </button>
        <div className="panel-title-text" style={{ fontSize: "1.25rem", fontWeight: 600, display: "flex", alignItems: "center", gap: "8px" }}>
          <Wand2 size={22} className="text-primary" />
          <span>Skills Center</span>
        </div>
      </div>

      <div style={{ display: "flex", borderBottom: "1px solid var(--divider)", padding: "0 24px", background: "var(--surface-1)" }}>
        <button 
          onClick={() => setActiveTab("installed")}
          style={{ 
            padding: "16px 20px", 
            background: "none", border: "none", 
            borderBottom: activeTab === "installed" ? "2px solid var(--primary)" : "2px solid transparent",
            color: activeTab === "installed" ? "var(--primary)" : "var(--text-2)",
            fontWeight: activeTab === "installed" ? 600 : 400,
            cursor: "pointer", display: "flex", alignItems: "center", gap: "8px",
            transition: "all 0.2s"
          }}
        >
          <Layers size={18} /> My Skills
        </button>
        <button 
          onClick={() => setActiveTab("marketplace")}
          style={{ 
            padding: "16px 20px", 
            background: "none", border: "none", 
            borderBottom: activeTab === "marketplace" ? "2px solid var(--primary)" : "2px solid transparent",
            color: activeTab === "marketplace" ? "var(--primary)" : "var(--text-2)",
            fontWeight: activeTab === "marketplace" ? 600 : 400,
            cursor: "pointer", display: "flex", alignItems: "center", gap: "8px",
            transition: "all 0.2s"
          }}
        >
          <PackageSearch size={18} /> Marketplace
        </button>
      </div>

      <div style={{ padding: "24px", overflowY: "auto", flex: 1 }}>
        {activeTab === "installed" && (
          <div style={{ display: "flex", flexDirection: "column", gap: "16px" }}>
            {skills.length === 0 ? (
              <div style={{ textAlign: "center", padding: "64px 20px", color: "var(--text-3)" }}>
                <Sparkles size={48} style={{ opacity: 0.2, margin: "0 auto 16px" }} />
                <h3>No Skills Installed</h3>
                <p style={{ marginBottom: "24px" }}>Enhance your agent with specialized abilities from the Marketplace.</p>
                <button className="primary-btn" onClick={() => setActiveTab("marketplace")}>Browse Marketplace</button>
              </div>
            ) : (
              skills.map(skill => (
                <div key={skill.id} style={{ 
                  background: "var(--surface-1)", borderRadius: "12px", 
                  border: "1px solid var(--divider)", padding: "20px",
                  display: "flex", alignItems: "flex-start", gap: "16px",
                  boxShadow: "0 2px 8px rgba(0,0,0,0.02)"
                }}>
                  <div style={{
                    width: 48, height: 48, borderRadius: "12px", 
                    background: "rgba(34, 211, 238, 0.1)",
                    display: "flex", alignItems: "center", justifyContent: "center",
                    color: "var(--primary)"
                  }}>
                    <Wand2 size={24} />
                  </div>
                  <div style={{ flex: 1 }}>
                    <div style={{ display: "flex", alignItems: "center", gap: "12px", marginBottom: "6px" }}>
                      <h3 style={{ margin: 0, fontSize: "1.1rem", color: "var(--text-1)" }}>{skill.name}</h3>
                      <span style={{ fontSize: "0.75rem", color: "var(--text-3)", background: "var(--surface-2)", padding: "2px 8px", borderRadius: "999px" }}>
                        v{skill.version || "1.0.0"}
                      </span>
                    </div>
                    <p style={{ margin: "0 0 12px 0", color: "var(--text-2)", fontSize: "0.9rem", lineHeight: 1.5 }}>
                      {skill.description}
                    </p>
                    <div style={{ fontSize: "0.8rem", color: "var(--text-3)" }}>By {skill.author || "Unknown"}</div>
                  </div>
                  <div>
                    <button className="icon-btn" title="Settings"><Settings2 size={18} /></button>
                  </div>
                </div>
              ))
            )}
          </div>
        )}

        {activeTab === "marketplace" && (
          <div style={{ display: "flex", flexDirection: "column", gap: "20px" }}>
            <div style={{ position: "relative" }}>
              <input 
                type="text" 
                className="text-input" 
                placeholder="Search skills by name or tag..." 
                value={searchQuery}
                onChange={e => setSearchQuery(e.target.value)}
                style={{ width: "100%", paddingLeft: "40px" }}
              />
              <PackageSearch size={18} style={{ position: "absolute", left: "14px", top: "50%", transform: "translateY(-50%)", color: "var(--text-3)" }} />
            </div>

            <div style={{ display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(300px, 1fr))", gap: "16px" }}>
              {filteredMarketplace.map(skill => {
                const isInstalled = skills.some(s => s.id === skill.id);
                const isInstalling = installingId === skill.id;

                return (
                  <div key={skill.id} style={{ 
                    background: "var(--surface-1)", borderRadius: "12px", 
                    border: "1px solid var(--divider)", padding: "20px",
                    display: "flex", flexDirection: "column", gap: "12px",
                    boxShadow: "0 2px 8px rgba(0,0,0,0.02)",
                    transition: "transform 0.2s, box-shadow 0.2s",
                    cursor: "default"
                  }}>
                    <div style={{ display: "flex", alignItems: "flex-start", gap: "12px" }}>
                      <div style={{
                        width: 40, height: 40, borderRadius: "10px", 
                        background: "var(--surface-2)",
                        display: "flex", alignItems: "center", justifyContent: "center",
                        color: "var(--text-2)"
                      }}>
                        <Layers size={20} />
                      </div>
                      <div style={{ flex: 1 }}>
                        <h4 style={{ margin: "0 0 4px 0", fontSize: "1.05rem", color: "var(--text-1)" }}>{skill.name}</h4>
                        <div style={{ fontSize: "0.75rem", color: "var(--text-3)" }}>By {skill.author}</div>
                      </div>
                    </div>
                    
                    <p style={{ margin: 0, color: "var(--text-2)", fontSize: "0.85rem", lineHeight: 1.5, flex: 1 }}>
                      {skill.description}
                    </p>
                    
                    {skill.tags && skill.tags.length > 0 && (
                      <div style={{ display: "flex", flexWrap: "wrap", gap: "6px" }}>
                        {skill.tags.map(tag => (
                          <span key={tag} style={{ background: "var(--surface-2)", color: "var(--text-2)", fontSize: "0.7rem", padding: "2px 8px", borderRadius: "4px" }}>
                            {tag}
                          </span>
                        ))}
                      </div>
                    )}
                    
                    <div style={{ marginTop: "auto", paddingTop: "12px", borderTop: "1px solid var(--divider)", display: "flex", alignItems: "center", justifyContent: "space-between" }}>
                      <span style={{ fontSize: "0.75rem", color: "var(--text-3)" }}>v{skill.version}</span>
                      {isInstalled ? (
                        <span style={{ display: "flex", alignItems: "center", gap: "4px", color: "var(--success)", fontSize: "0.85rem", fontWeight: 500 }}>
                          <CheckCircle2 size={16} /> Installed
                        </span>
                      ) : (
                        <button 
                          className="primary-btn" 
                          style={{ padding: "6px 14px", fontSize: "0.85rem", display: "flex", alignItems: "center", gap: "6px" }}
                          onClick={() => handleInstall(skill.id)}
                          disabled={isInstalling}
                        >
                          {isInstalling ? <Loader2 size={14} className="spin" /> : <Download size={14} />} 
                          {isInstalling ? "Installing..." : "Install"}
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
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
