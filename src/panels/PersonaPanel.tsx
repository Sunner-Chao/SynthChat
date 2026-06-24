import { ChangeEvent, useEffect, useState } from "react";
import { Camera, Check, FileAudio, FolderOpen, Image, ImagePlus, Mic, Pencil, Plus, Settings, Sparkles, Trash2, Wand2 } from "lucide-react";
import { api } from "../lib/api";
import { useAppStore } from "../lib/store";
import type { ChatConfig, ModelCatalogEntry, Persona } from "../lib/types";
import { Avatar } from "../components/common";
export function PersonaPanel() {
  const {
    personas,
    emojiGroups,
    llmProviders,
    imageProviders,
    agents,
    config,
    savePersona,
    saveConfig,
    deletePersona,
    uploadPersonaAvatar,
    clearPersonaAvatar,
    proactiveStatuses,
    refreshProactiveStatuses,
    triggerProactiveOnce
  } = useAppStore();
  const [selectedId, setSelectedId] = useState(personas[0]?.id ?? "default");
  const selectedPersona = personas.find((persona) => persona.id === selectedId) ?? personas[0] ?? createDraftPersona();
  const [draft, setDraft] = useState<Persona>(selectedPersona);
  const [tab, setTab] = useState<"detail" | "persona" | "behavior" | "image" | "tools">("detail");
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [catalogModels, setCatalogModels] = useState<ModelCatalogEntry[]>([]);

  useEffect(() => {
    const provider = llmProviders.find((p) => p.id === draft.llmProvider);
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
  }, [draft.llmProvider, llmProviders]);

  useEffect(() => {
    const next = personas.find((persona) => persona.id === selectedId) ?? personas[0];
    if (selectedId.startsWith("persona-") && !personas.some((persona) => persona.id === selectedId)) return;
    if (next) {
      setSelectedId(next.id);
      setDraft(next);
    }
  }, [personas, selectedId]);

  useEffect(() => {
    void refreshProactiveStatuses();
  }, [refreshProactiveStatuses, personas.length]);

  const provider = llmProviders.find((item) => item.id === draft.llmProvider) ?? llmProviders[0];
  const proactiveStatus = proactiveStatuses.find((status) => status.personaId === draft.id);

  const updateDraft = <K extends keyof Persona>(key: K, value: Persona[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };

  const save = async () => {
    setSaving(true);
    try {
      const saved = await savePersona(draft);
      setSelectedId(saved.id);
      setDraft(saved);
      setSaved(true);
      setTimeout(() => setSaved(false), 2000);
    } finally {
      setSaving(false);
    }
  };

  const createNew = () => {
    const next = createDraftPersona();
    setSelectedId(next.id);
    setDraft(next);
    setTab("detail");
  };

  const remove = async () => {
    if (draft.id === "default") return;
    await deletePersona(draft.id);
    const fallback = personas.find((persona) => persona.id !== draft.id) ?? createDraftPersona();
    setSelectedId(fallback.id);
  };

  const onAvatar = async (event: ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.currentTarget.value = "";
    if (!file) return;
    let targetId = draft.id;
    if (draft.id.startsWith("persona-")) {
      const savedPersona = await savePersona(draft);
      setSelectedId(savedPersona.id);
      setDraft(savedPersona);
      targetId = savedPersona.id;
    }
    const saved = await uploadPersonaAvatar(targetId, file);
    setDraft(saved);
  };

  const avatarSrc = draft.avatarPath ? api.assetUrl(draft.avatarPath) : "";
  const chatConfig = config?.chat ?? null;
  const saveChatConfig = async (patch: Partial<ChatConfig>) => {
    if (!config) return;
    await saveConfig({ ...config, chat: { ...config.chat, ...patch } });
  };

  return (
    <section className="panel-grid persona-workbench">
      <aside className="side-panel persona-sidebar">
        <div className="side-title">
          <h3>通讯录</h3>
          <button onClick={createNew} title="新建角色" type="button">
            <Plus size={16} />
          </button>
        </div>
        <div className="persona-list">
          {personas.map((persona) => {
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
                className={persona.id === draft.id ? "persona-list-item active" : "persona-list-item"}
                key={persona.id}
                onClick={() => {
                  setSelectedId(persona.id);
                  setDraft(persona);
                }}
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

      <article className="primary-panel persona-editor">
        <div className="panel-title persona-editor-title">
          <div className="panel-title-text"><span>Persona</span><strong>{draft.id.startsWith("persona-") ? "新建角色" : "编辑角色"}</strong></div>
          <button onClick={save} type="button" disabled={saving}>
            {saved ? <><Check size={16} /> 已保存</> : saving ? "保存中..." : "保存"}
          </button>
        </div>

        <div className="persona-hero">
          <input accept="image/*" id="persona-avatar-file" onChange={onAvatar} type="file" />
          <label className="persona-avatar-uploader" htmlFor="persona-avatar-file">
            <Avatar name={draft.name} src={avatarSrc} size="large" />
            <span><Image size={14} /></span>
          </label>
          <div>
            <input
              aria-label="角色名称"
              value={draft.name}
              onChange={(event) => updateDraft("name", event.target.value)}
              placeholder="输入角色名称"
            />
            <p>{draft.id}</p>
            {draft.avatarPath ? (
              <button onClick={() => void clearPersonaAvatar(draft.id).then(setDraft)} type="button">移除头像</button>
            ) : null}
          </div>
        </div>

        <div className="inline-tabs">
          <button className={tab === "detail" ? "active" : ""} onClick={() => setTab("detail")} type="button">角色详情</button>
          <button className={tab === "persona" ? "active" : ""} onClick={() => setTab("persona")} type="button">角色人设</button>
          <button className={tab === "behavior" ? "active" : ""} onClick={() => setTab("behavior")} type="button">互动设置</button>
          <button className={tab === "image" ? "active" : ""} onClick={() => setTab("image")} type="button">生图选项</button>
          <button className={tab === "tools" ? "active" : ""} onClick={() => setTab("tools")} type="button">工具策略</button>
        </div>

        {tab === "detail" ? (
          <div className="settings-form persona-form">
            <label>
              对话服务商
              <select
                value={draft.llmProvider || ""}
                onChange={(event) => {
                  const nextProvider = llmProviders.find((item) => item.id === event.target.value);
                  setDraft((current) => ({
                    ...current,
                    llmProvider: event.target.value,
                    llmModel: nextProvider?.model ?? current.llmModel
                  }));
                }}
              >
                <option value="">请选择服务商</option>
                {llmProviders.map((item) => (
                  <option key={item.id} value={item.id}>{item.name}</option>
                ))}
              </select>
            </label>
            <label>
              模型 ID
              <div className="model-select-row">
                {catalogModels.length > 0 ? (
                  <select
                    value={catalogModels.some((model) => model.id === draft.llmModel) ? draft.llmModel : ""}
                    onChange={(event) => {
                      const value = event.target.value;
                      if (value) updateDraft("llmModel", value);
                    }}
                  >
                    <option value="">从目录选择模型</option>
                    {catalogModels.map((model) => (
                      <option key={model.id} value={model.id}>{model.name || model.id}{model.family ? ` (${model.family})` : ""}</option>
                    ))}
                  </select>
                ) : null}
                <input
                  value={draft.llmModel || provider?.model || ""}
                  onChange={(event) => updateDraft("llmModel", event.target.value)}
                  placeholder={catalogModels.length > 0 ? "或手动输入" : "模型 ID"}
                />
              </div>
            </label>
            <label>
              绑定智能体
              <select
                value={draft.agentId ?? ""}
                onChange={(event) => updateDraft("agentId", event.target.value)}
              >
                <option value="">默认智能体</option>
                {agents.map((agent) => (
                  <option key={agent.id} value={agent.id}>{agent.name}{agent.isDefault ? " (默认)" : ""}</option>
                ))}
              </select>
            </label>
            <label>
              系统提示
              <textarea value={draft.systemPrompt} onChange={(event) => updateDraft("systemPrompt", event.target.value)} />
            </label>
            <div className="two-column">
              <label>
                温度 {draft.temperature.toFixed(2)}
                <input min={0} max={2} step={0.05} type="range" value={draft.temperature} onChange={(event) => updateDraft("temperature", Number(event.target.value))} />
              </label>
              <label>
                最大输出
                <input min={128} max={65536} type="number" value={draft.maxTokens} onChange={(event) => updateDraft("maxTokens", Number(event.target.value))} />
              </label>
            </div>
          </div>
        ) : null}

        {tab === "persona" ? (
          <div className="settings-form persona-form">
            <label>
              角色详情
              <textarea value={draft.characterPrompt} onChange={(event) => updateDraft("characterPrompt", event.target.value)} placeholder="描述角色的背景、性格、经历..." />
            </label>
            <label>
              输出示例
              <textarea value={draft.outputExamples} onChange={(event) => updateDraft("outputExamples", event.target.value)} placeholder="输入角色的经典台词作为风格参考..." />
            </label>
            <label>
              全局系统指令
              <textarea value={draft.systemInstructions} onChange={(event) => updateDraft("systemInstructions", event.target.value)} />
            </label>
          </div>
        ) : null}

        {tab === "behavior" ? (
          <div className="settings-form persona-form">
            <div className="form-section-title">表情包</div>
            <label className="checkbox-row">
              <input
                checked={draft.emojiEnabled ?? false}
                onChange={(event) => setDraft((current) => ({ ...current, emojiEnabled: event.target.checked }))}
                type="checkbox"
              />
              启用表情包自动发送
            </label>
            <div className="two-column">
              <label>
                表情包分组
                <select value={draft.emojiGroup ?? ""} onChange={(event) => updateDraft("emojiGroup", event.target.value)}>
                  <option value="">不绑定</option>
                  {emojiGroups.map((group) => (
                    <option key={group.id} value={group.id}>{group.name}</option>
                  ))}
                </select>
              </label>
              <label>
                发送概率 {draft.emojiSendProbability ?? 25}%
                <input
                  min={0}
                  max={100}
                  step={1}
                  type="range"
                  value={draft.emojiSendProbability ?? 25}
                  onChange={(event) => updateDraft("emojiSendProbability", Number(event.target.value))}
                />
              </label>
            </div>
            <div className="form-section-title">长期记忆</div>
            <label className="checkbox-row">
              <input
                checked={draft.memory?.enabled ?? true}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  memory: { ...(current.memory ?? defaultMemoryConfig()), enabled: event.target.checked }
                }))}
                type="checkbox"
              />
              启用长期记忆
            </label>
            <label>
              长期记忆注入上限
              <input
                min={1}
                type="number"
                value={draft.memory?.maxMemories ?? 50}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  memory: { ...(current.memory ?? defaultMemoryConfig()), maxMemories: Number(event.target.value) }
                }))}
              />
            </label>
            <label className="checkbox-row">
              <input
                checked={draft.memory?.includeInPrompt ?? true}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  memory: { ...(current.memory ?? defaultMemoryConfig()), includeInPrompt: event.target.checked }
                }))}
                type="checkbox"
              />
              将记忆注入提示词
            </label>
            <div className="form-hint">这里是角色级长期记忆：控制是否注入长期记忆、以及最多注入多少条。</div>
            {chatConfig ? (
              <ShortMemorySettings config={chatConfig} onSave={saveChatConfig} />
            ) : null}
            <div className="form-section-title">主动消息</div>
            <label className="checkbox-row">
              <input
                checked={draft.proactive?.enabled ?? false}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  proactive: { ...(current.proactive ?? defaultProactiveConfig()), enabled: event.target.checked }
                }))}
                type="checkbox"
              />
              启用主动消息
            </label>
            <div className="two-column">
              <label>
                回复后最短（小时）
                <input min={0} step={0.1} type="number" value={draft.proactive?.minIdleHours ?? 1} onChange={(event) => setDraft((current) => ({ ...current, proactive: { ...(current.proactive ?? defaultProactiveConfig()), minIdleHours: Number(event.target.value) } }))} />
              </label>
              <label>
                回复后最长（小时）
                <input min={0} step={0.1} type="number" value={draft.proactive?.maxIdleHours ?? 3} onChange={(event) => setDraft((current) => ({ ...current, proactive: { ...(current.proactive ?? defaultProactiveConfig()), maxIdleHours: Number(event.target.value) } }))} />
              </label>
            </div>
            <div className="two-column">
              <label>
                连续上限
                <input min={1} max={100} type="number" value={draft.proactive?.maxConsecutive ?? 3} onChange={(event) => setDraft((current) => ({ ...current, proactive: { ...(current.proactive ?? defaultProactiveConfig()), maxConsecutive: Number(event.target.value) } }))} />
              </label>
              <label>
                静默时段
                <span className="time-range">
                  <input type="time" value={draft.proactive?.quietHours.start ?? "22:00"} onChange={(event) => setDraft((current) => ({ ...current, proactive: { ...(current.proactive ?? defaultProactiveConfig()), quietHours: { ...((current.proactive ?? defaultProactiveConfig()).quietHours), start: event.target.value } } }))} />
                  <input type="time" value={draft.proactive?.quietHours.end ?? "08:00"} onChange={(event) => setDraft((current) => ({ ...current, proactive: { ...(current.proactive ?? defaultProactiveConfig()), quietHours: { ...((current.proactive ?? defaultProactiveConfig()).quietHours), end: event.target.value } } }))} />
                </span>
              </label>
            </div>
            <label className="checkbox-row">
              <input
                checked={draft.proactive?.quietHours.enabled ?? true}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  proactive: { ...(current.proactive ?? defaultProactiveConfig()), quietHours: { ...((current.proactive ?? defaultProactiveConfig()).quietHours), enabled: event.target.checked } }
                }))}
                type="checkbox"
              />
              静默时段内不主动发送
            </label>
            <label>
              主动消息提示词
              <textarea value={draft.proactive?.prompt ?? ""} onChange={(event) => setDraft((current) => ({ ...current, proactive: { ...(current.proactive ?? defaultProactiveConfig()), prompt: event.target.value } }))} />
            </label>
            <div className="memory-item" style={{ alignItems: "center" }}>
              <div className="memory-content">
                <strong>{proactiveStatus?.canFire ? "主动消息已就绪" : proactiveStatus?.blockedReason || "主动消息状态未同步"}</strong>
                <span className="memory-meta">
                  回复后 {Math.ceil((proactiveStatus?.secondsSinceLastReply ?? 0) / 60)} 分钟 · 间隔 {Math.ceil((proactiveStatus?.waitSeconds ?? 0) / 60)} 分钟 · 连续 {proactiveStatus?.consecutiveCount ?? 0}/{proactiveStatus?.maxConsecutive ?? 1}
                </span>
              </div>
              <button
                onClick={async () => {
                  await savePersona(draft);
                  await triggerProactiveOnce(draft.id);
                }}
                type="button"
              >
                立即触发
              </button>
            </div>
            <div className="form-section-title" style={{ display: "flex", alignItems: "center", gap: 8, marginTop: 8 }}>
              <Mic size={15} style={{ color: "var(--primary)" }} />
              微信语音回复
            </div>

            {/* 语音回复总开关 */}
            <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
              <label className="checkbox-row" style={{ marginBottom: 0 }}>
                <input
                  checked={draft.voiceReply?.enabled ?? false}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), enabled: event.target.checked }
                  }))}
                  type="checkbox"
                />
                <span style={{ fontWeight: 500 }}>启用语音回复</span>
              </label>
            </div>

            {/* TTS 引擎配置 */}
            <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                <Settings size={14} />
                TTS 引擎配置
              </div>
              <div className="two-column" style={{ marginBottom: 12 }}>
                <label>
                  TTS 引擎
                  <select
                    value={draft.voiceReply?.engine ?? "chattts"}
                    onChange={(event) => setDraft((current) => ({
                      ...current,
                      voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), engine: event.target.value }
                    }))}
                  >
                    <option value="chattts">ChatTTS</option>
                  </select>
                </label>
                <label>
                  采样率
                  <input
                    min={8000}
                    max={48000}
                    step={1000}
                    type="number"
                    value={draft.voiceReply?.sampleRate ?? 16000}
                    onChange={(event) => setDraft((current) => ({
                      ...current,
                      voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), sampleRate: Number(event.target.value) }
                    }))}
                  />
                </label>
              </div>
              <label style={{ marginBottom: 12 }}>
                模型目录
                <input
                  value={draft.voiceReply?.modelDir ?? ""}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), modelDir: event.target.value }
                  }))}
                  placeholder="留空使用环境变量 SYNTHCHAT_TTS_MODEL_DIR"
                />
              </label>
              <label style={{ marginBottom: 0 }}>
                Python 路径
                <input
                  value={draft.voiceReply?.pythonPath ?? ""}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), pythonPath: event.target.value }
                  }))}
                  placeholder="留空使用 SYNTHCHAT_TTS_PYTHON 或 python"
                />
              </label>
            </div>

            {/* 音色配置 */}
            <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                <FileAudio size={14} />
                音色配置
              </div>
              <div className="two-column" style={{ marginBottom: 12 }}>
                <label>
                  音色种子
                  <input
                    min={0}
                    type="number"
                    value={draft.voiceReply?.speakerSeed ?? 0}
                    onChange={(event) => setDraft((current) => ({
                      ...current,
                      voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), speakerSeed: Number(event.target.value) }
                    }))}
                  />
                </label>
                <label>
                  语速 {draft.voiceReply?.speed ?? 5}
                  <input
                    min={1}
                    max={9}
                    step={1}
                    type="range"
                    value={draft.voiceReply?.speed ?? 5}
                    onChange={(event) => setDraft((current) => ({
                      ...current,
                      voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), speed: Number(event.target.value) }
                    }))}
                  />
                </label>
              </div>

              {/* Speaker Embedding */}
              <div style={{ padding: "12px", background: "var(--surface-2)", borderRadius: "var(--radius-md)", border: "1px solid var(--divider)" }}>
                <div className="detail-row" style={{ paddingTop: 0, borderTop: 0 }}>
                  <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                    <Sparkles size={13} style={{ color: "var(--primary)" }} />
                    固定音色
                  </span>
                  <strong style={{ color: draft.voiceReply?.speakerEmbedding ? "var(--success)" : "var(--text-3)" }}>
                    {draft.voiceReply?.speakerEmbedding ? "已固定" : "按种子随机"}
                  </strong>
                </div>
                <label style={{ marginBottom: 0, marginTop: 8 }}>
                  Embedding 文件路径
                  <div style={{ display: "flex", gap: 8 }}>
                    <input
                      value={draft.voiceReply?.speakerEmbedding ?? ""}
                      onChange={(event) => setDraft((current) => ({
                        ...current,
                        voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), speakerEmbedding: event.target.value }
                      }))}
                      placeholder="点击右侧按钮浏览选择 .pt 文件"
                      style={{ fontFamily: "var(--font-mono)", fontSize: 13, flex: 1 }}
                    />
                    <button
                      type="button"
                      onClick={async () => {
                        const path = await api.pickFile("选择 Speaker Embedding 文件", "Embedding 文件", ["pt"]);
                        if (path) {
                          setDraft((current) => ({
                            ...current,
                            voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), speakerEmbedding: path }
                          }));
                        }
                      }}
                      style={{ display: "inline-flex", alignItems: "center", gap: 4, padding: "0 12px", height: 38, border: "1px solid var(--divider)", borderRadius: "var(--radius-sm)", background: "var(--card)", color: "var(--text-2)", cursor: "pointer", fontSize: 13, whiteSpace: "nowrap", flexShrink: 0 }}
                      title="浏览选择文件"
                    >
                      <FolderOpen size={14} />
                      浏览
                    </button>
                    {draft.voiceReply?.speakerEmbedding ? (
                      <button
                        type="button"
                        onClick={() => setDraft((current) => ({
                          ...current,
                          voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), speakerEmbedding: "" }
                        }))}
                        style={{ display: "inline-flex", alignItems: "center", gap: 4, padding: "0 12px", height: 38, border: "1px solid var(--danger)", borderRadius: "var(--radius-sm)", background: "transparent", color: "var(--danger)", cursor: "pointer", fontSize: 13, whiteSpace: "nowrap", flexShrink: 0 }}
                        title="清除路径"
                      >
                        <Trash2 size={14} />
                      </button>
                    ) : null}
                  </div>
                </label>
                <p className="form-hint" style={{ marginTop: 6, marginBottom: 0, fontSize: 11 }}>
                  生成 embedding：运行 ChatTTS 脚本后在模型目录下产出 .pt 文件，浏览选择即可固定音色
                </p>
              </div>
            </div>

            {/* 语音风格参数 */}
            <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
              <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                <Wand2 size={14} />
                语音风格
              </div>
              <div className="two-column" style={{ marginBottom: 12 }}>
                <label>
                  口语化 {draft.voiceReply?.oral ?? 2}
                  <input min={0} max={9} step={1} type="range" value={draft.voiceReply?.oral ?? 2} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), oral: Number(event.target.value) } }))} />
                </label>
                <label>
                  笑声 {draft.voiceReply?.laugh ?? 0}
                  <input min={0} max={9} step={1} type="range" value={draft.voiceReply?.laugh ?? 0} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), laugh: Number(event.target.value) } }))} />
                </label>
              </div>
              <label style={{ marginBottom: 12 }}>
                停顿 {draft.voiceReply?.breakLevel ?? 4}
                <input min={0} max={9} step={1} type="range" value={draft.voiceReply?.breakLevel ?? 4} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), breakLevel: Number(event.target.value) } }))} />
              </label>
              <div className="two-column" style={{ marginBottom: 0 }}>
                <label>
                  temperature
                  <input min={0.01} max={2} step={0.01} type="number" value={draft.voiceReply?.temperature ?? 0.3} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), temperature: Number(event.target.value) } }))} />
                </label>
                <label>
                  top_p
                  <input min={0.01} max={1} step={0.01} type="number" value={draft.voiceReply?.topP ?? 0.7} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), topP: Number(event.target.value) } }))} />
                </label>
                <label>
                  top_k
                  <input min={1} max={100} type="number" value={draft.voiceReply?.topK ?? 20} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), topK: Number(event.target.value) } }))} />
                </label>
              </div>
            </div>

            {/* 文本润色 */}
            <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
              <label className="checkbox-row" style={{ marginBottom: 12 }}>
                <input
                  checked={draft.voiceReply?.refineTextEnabled ?? true}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), refineTextEnabled: event.target.checked }
                  }))}
                  type="checkbox"
                />
                <span style={{ fontWeight: 500, display: "flex", alignItems: "center", gap: 6 }}>
                  <Sparkles size={13} />
                  启用文本润色
                </span>
              </label>
              <div className="two-column" style={{ marginBottom: 12 }}>
                <label>
                  润色 temperature
                  <input min={0.01} max={2} step={0.01} type="number" value={draft.voiceReply?.refineTemperature ?? 0.7} onChange={(event) => setDraft((current) => ({ ...current, voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), refineTemperature: Number(event.target.value) } }))} />
                </label>
              </div>
              <label style={{ marginBottom: 0 }}>
                润色 Prompt
                <input
                  value={draft.voiceReply?.refinePrompt ?? ""}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    voiceReply: { ...(current.voiceReply ?? defaultVoiceReplyConfig()), refinePrompt: event.target.value }
                  }))}
                  placeholder="留空使用 oral/laugh/break 组合"
                />
              </label>
            </div>
          </div>
        ) : null}

        {tab === "image" ? (
          <div className="settings-form persona-form">
            <label className="checkbox-row">
              <input
                checked={draft.imageGeneration?.enabled ?? false}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), enabled: event.target.checked }
                }))}
                type="checkbox"
              />
              启用 AI 生图
            </label>
            <div className="two-column">
              <label>
                生图服务商
                <select value={draft.imageGeneration?.provider ?? ""} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), provider: event.target.value } }))}>
                  <option value="">使用默认启用服务商</option>
                  {imageProviders.map((item) => (
                    <option key={item.id} value={item.id}>{item.name}{item.model ? ` · ${item.model}` : ""}</option>
                  ))}
                </select>
              </label>
              <label>
                生图模型
                <input value={draft.imageGeneration?.model ?? ""} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), model: event.target.value } }))} />
              </label>
            </div>
            <label>
              风格前缀
              <input value={draft.imageGeneration?.stylePrefix ?? ""} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), stylePrefix: event.target.value } }))} />
            </label>
            <label>
              画面风格
              <textarea value={draft.imageGeneration?.artStyle ?? ""} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), artStyle: event.target.value } }))} />
            </label>
            <label className="checkbox-row">
              <input
                checked={draft.imageGeneration?.negativeEnabled ?? true}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), negativeEnabled: event.target.checked }
                }))}
                type="checkbox"
              />
              启用负面提示词
            </label>
            <label>
              负面提示词
              <textarea value={draft.imageGeneration?.negativePrompt ?? ""} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), negativePrompt: event.target.value } }))} />
            </label>
            <label>
              参考图模式
              <select value={draft.imageGeneration?.refMode ?? "avatar"} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), refMode: event.target.value as "avatar" | "custom" | "none" } }))}>
                <option value="avatar">使用角色头像</option>
                <option value="custom">使用自定义形象图</option>
                <option value="none">不使用参考图</option>
              </select>
            </label>
          </div>
        ) : null}

        {tab === "tools" ? (
          <div className="settings-form persona-form">
            <label className="checkbox-row">
              <input
                checked={draft.toolPolicy.enabled}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  toolPolicy: { ...current.toolPolicy, enabled: event.target.checked }
                }))}
                type="checkbox"
              />
              允许该角色调用 MCP 工具
            </label>
            <div className="two-column">
              <label>
                timeout_seconds
                <input
                  min={1}
                  type="number"
                  value={draft.toolPolicy.timeoutSeconds}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    toolPolicy: { ...current.toolPolicy, timeoutSeconds: Number(event.target.value) }
                  }))}
                />
              </label>
              <label>
                max_iterations
                <input
                  min={1}
                  max={64}
                  type="number"
                  value={draft.toolPolicy.maxIterations}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    toolPolicy: { ...current.toolPolicy, maxIterations: Number(event.target.value) }
                  }))}
                />
              </label>
              <label>
                max_failure_replans
                <input
                  min={0}
                  max={32}
                  type="number"
                  value={draft.toolPolicy.maxFailureReplans ?? 2}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    toolPolicy: { ...current.toolPolicy, maxFailureReplans: Number(event.target.value) }
                  }))}
                />
              </label>
              <label>
                retry_count
                <input
                  min={0}
                  max={5}
                  type="number"
                  value={draft.toolPolicy.retryCount ?? 1}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    toolPolicy: { ...current.toolPolicy, retryCount: Number(event.target.value) }
                  }))}
                />
              </label>
              <label>
                retry_backoff_ms
                <input
                  min={0}
                  max={10000}
                  step={100}
                  type="number"
                  value={draft.toolPolicy.retryBackoffMs ?? 300}
                  onChange={(event) => setDraft((current) => ({
                    ...current,
                    toolPolicy: { ...current.toolPolicy, retryBackoffMs: Number(event.target.value) }
                  }))}
                />
              </label>
            </div>
            <div className="metric-strip compact">
              <div><strong>{draft.toolPolicy.enabled ? "开启" : "关闭"}</strong><span>工具调用</span></div>
              <div><strong>{draft.toolPolicy.timeoutSeconds}s</strong><span>角色级超时</span></div>
              <div><strong>{draft.toolPolicy.maxIterations}</strong><span>循环上限</span></div>
              <div><strong>{draft.toolPolicy.maxFailureReplans ?? 2}</strong><span>失败重规划</span></div>
              <div><strong>{draft.toolPolicy.retryCount ?? 1}</strong><span>工具重试</span></div>
            </div>
          </div>
        ) : null}

        <div className="persona-actions">
          <button onClick={save} type="button">
            <Pencil size={15} />
            保存角色
          </button>
          <button className="ghost-button" onClick={createNew} type="button">新建副本</button>
          {draft.id !== "default" ? (
            <button className="danger-text" onClick={() => void remove()} type="button">
              <Trash2 size={15} />
              删除角色
            </button>
          ) : null}
        </div>
      </article>
    </section>
  );
}

function ShortMemorySettings({
  config,
  onSave
}: {
  config: ChatConfig;
  onSave: (patch: Partial<ChatConfig>) => Promise<void>;
}) {
  const [mode, setMode] = useState<"messages" | "tokens">(config.shortContextMode ?? "tokens");
  const [messages, setMessages] = useState(config.maxContextRounds);
  const [tokenK, setTokenK] = useState(Math.max(1, Math.round((config.shortContextTokenBudget ?? 8000) / 1000)));
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setMode(config.shortContextMode ?? "messages");
    setMessages(config.maxContextRounds);
    setTokenK(Math.max(1, Math.round((config.shortContextTokenBudget ?? 8000) / 1000)));
  }, [config.maxContextRounds, config.shortContextMode, config.shortContextTokenBudget]);

  const save = async () => {
    setSaving(true);
    try {
      await onSave({
        shortContextMode: mode,
        maxContextRounds: messages,
        shortContextTokenBudget: tokenK * 1000
      });
      setSaved(true);
      window.setTimeout(() => setSaved(false), 1600);
    } finally {
      setSaving(false);
    }
  };

  return (
    <>
      <div className="form-section-title">短时记忆</div>
      <label>
        短时记忆策略
        <select value={mode} onChange={(event) => setMode(event.target.value === "tokens" ? "tokens" : "messages")}>
          <option value="tokens">按 token 预算</option>
          <option value="messages">按消息数</option>
        </select>
      </label>
      {mode === "messages" ? (
        <label>
          消息窗口
          <input
            min={1}
            max={500}
            type="number"
            value={messages}
            onChange={(event) => setMessages(Math.min(500, Math.max(1, Number(event.target.value) || 1)))}
          />
        </label>
      ) : (
        <label>
          Token 预算（K）
          <input
            min={1}
            max={200}
            type="number"
            value={tokenK}
            onChange={(event) => setTokenK(Math.min(200, Math.max(1, Number(event.target.value) || 1)))}
          />
        </label>
      )}
      <button className="ghost-button" disabled={saving} onClick={() => void save()} type="button">
        {saved ? "短时记忆已保存" : saving ? "保存中..." : "保存短时记忆设置"}
      </button>
      <div className="form-hint">按 token 预算时使用 K 单位预算；按消息数时使用消息窗口。达到瓶颈后旧片段会压缩为短时摘要继续参与当前会话。</div>
    </>
  );
}

function createDraftPersona(): Persona {
  return {
    id: `persona-${crypto.randomUUID()}`,
    name: "新角色",
    avatarPath: null,
    systemPrompt: "你正在扮演这个角色，请保持设定一致并自然交流。",
    characterPrompt: "",
    outputExamples: "",
    systemInstructions: "请始终保持角色一致性，结合角色详情、世界书与长期记忆作答。",
    llmProvider: "",
    llmModel: "",
    temperature: 0.8,
    maxTokens: 2048,
    toolPolicy: {
      enabled: true,
      timeoutSeconds: 30,
      maxIterations: 8,
      maxFailureReplans: 2,
      retryCount: 1,
      retryBackoffMs: 300
    },
    emojiEnabled: false,
    emojiGroup: "",
    emojiSendProbability: 25,
    memory: defaultMemoryConfig(),
    proactive: defaultProactiveConfig(),
    voiceReply: defaultVoiceReplyConfig(),
    imageGeneration: defaultImageGenerationConfig(),
    agentId: ""
  };
}

function defaultMemoryConfig(): NonNullable<Persona["memory"]> {
  return { enabled: true, triggerRounds: 10, maxMemories: 50, includeInPrompt: true };
}

function defaultProactiveConfig(): NonNullable<Persona["proactive"]> {
  return {
    enabled: false,
    minIdleHours: 1,
    maxIdleHours: 3,
    maxConsecutive: 3,
    prompt: "用户已经一段时间没有回复了。请根据角色设定与近期对话，主动发起一条贴合角色的简短消息。",
    quietHours: { enabled: true, start: "22:00", end: "08:00" }
  };
}

function defaultVoiceReplyConfig(): NonNullable<Persona["voiceReply"]> {
  return {
    enabled: false,
    engine: "chattts",
    pythonPath: "",
    modelDir: "",
    sampleRate: 16000,
    speed: 5,
    oral: 2,
    laugh: 0,
    breakLevel: 4,
    speakerSeed: 20240,
    speakerEmbedding: "models/ChatTTS/speaker/speaker_20240.pt",
    temperature: 0.3,
    topP: 0.7,
    topK: 20,
    refineTextEnabled: true,
    refinePrompt: "[oral_2][laugh_0][break_4]",
    refineTemperature: 0.7
  };
}

function defaultImageGenerationConfig(): NonNullable<Persona["imageGeneration"]> {
  return {
    enabled: false,
    provider: "",
    model: "",
    stylePrefix: "",
    artStyle: "anime style, masterpiece, best quality",
    negativePrompt: "low quality, blurry, watermark, text, signature, lowres, bad anatomy, extra fingers, jpeg artifacts",
    negativeEnabled: true,
    refMode: "avatar"
  };
}
