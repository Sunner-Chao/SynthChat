import { ChangeEvent, useEffect, useMemo, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { Camera, Check, FileAudio, FolderOpen, Image, ImagePlus, Mic, Pencil, Plus, Settings, Sparkles, Trash2, Wand2 } from "lucide-react";
import { api } from "../lib/api";
import { resolvePersonaAgentBinding } from "../lib/personaAgentBinding";
import { useAppStore } from "../lib/store";
import type { ModelCatalogEntry, Persona } from "../lib/types";
import { Avatar } from "../components/common";

type VoiceReplyConfig = NonNullable<Persona["voiceReply"]>;
type TextOption = { value: string; label: string };
type NumberOption = { value: number; label: string };

const TTS_ENGINE_OPTIONS: TextOption[] = [
  { value: "chattts", label: "ChatTTS" },
  { value: "edge", label: "Edge TTS" },
  { value: "local_command", label: "本地命令" }
];

const EDGE_LANGUAGE_OPTIONS: TextOption[] = [
  { value: "zh-CN", label: "中文（普通话）" },
  { value: "en-US", label: "English（美国）" },
  { value: "en-GB", label: "English（英国）" },
  { value: "ja-JP", label: "日语" },
  { value: "ko-KR", label: "韩语" },
  { value: "zh-HK", label: "中文（粤语）" },
  { value: "zh-TW", label: "中文（台湾）" }
];

const EDGE_VOICE_OPTIONS: Record<string, TextOption[]> = {
  "zh-CN": [
    { value: "zh-CN-XiaoxiaoNeural", label: "晓晓 · 女声 · 自然" },
    { value: "zh-CN-XiaoyiNeural", label: "晓伊 · 女声 · 明亮" },
    { value: "zh-CN-YunxiNeural", label: "云希 · 男声 · 温和" },
    { value: "zh-CN-YunjianNeural", label: "云健 · 男声 · 稳重" },
    { value: "zh-CN-YunyangNeural", label: "云扬 · 男声 · 播报" },
    { value: "zh-CN-XiaobeiNeural", label: "晓北 · 女声 · 东北" },
    { value: "zh-CN-XiaoniNeural", label: "晓妮 · 女声 · 陕西" },
    { value: "zh-CN-XiaorouNeural", label: "晓柔 · 女声 · 四川" }
  ],
  "en-US": [
    { value: "en-US-AriaNeural", label: "Aria · Female · Friendly" },
    { value: "en-US-JennyNeural", label: "Jenny · Female · Natural" },
    { value: "en-US-GuyNeural", label: "Guy · Male · Warm" },
    { value: "en-US-AnaNeural", label: "Ana · Female · Young" },
    { value: "en-US-AndrewNeural", label: "Andrew · Male · Conversational" },
    { value: "en-US-EmmaNeural", label: "Emma · Female · Conversational" }
  ],
  "en-GB": [
    { value: "en-GB-SoniaNeural", label: "Sonia · Female · Natural" },
    { value: "en-GB-RyanNeural", label: "Ryan · Male · Natural" },
    { value: "en-GB-LibbyNeural", label: "Libby · Female · Bright" }
  ],
  "ja-JP": [
    { value: "ja-JP-NanamiNeural", label: "Nanami · Female" },
    { value: "ja-JP-KeitaNeural", label: "Keita · Male" }
  ],
  "ko-KR": [
    { value: "ko-KR-SunHiNeural", label: "SunHi · Female" },
    { value: "ko-KR-InJoonNeural", label: "InJoon · Male" }
  ],
  "zh-HK": [
    { value: "zh-HK-HiuMaanNeural", label: "曉曼 · 女声" },
    { value: "zh-HK-WanLungNeural", label: "雲龍 · 男声" }
  ],
  "zh-TW": [
    { value: "zh-TW-HsiaoChenNeural", label: "曉臻 · 女声" },
    { value: "zh-TW-YunJheNeural", label: "雲哲 · 男声" }
  ]
};

const EDGE_SPEED_OPTIONS: NumberOption[] = [
  { value: 1, label: "极慢 · 0.50x" },
  { value: 3, label: "慢速 · 0.75x" },
  { value: 5, label: "标准 · 1.00x" },
  { value: 7, label: "偏快 · 1.25x" },
  { value: 9, label: "快速 · 1.50x" }
];

const EDGE_VOLUME_OPTIONS: TextOption[] = [
  { value: "-20%", label: "安静 · -20%" },
  { value: "-10%", label: "稍低 · -10%" },
  { value: "+0%", label: "标准 · +0%" },
  { value: "+10%", label: "稍高 · +10%" },
  { value: "+20%", label: "响亮 · +20%" }
];

const EDGE_PITCH_OPTIONS: TextOption[] = [
  { value: "-40Hz", label: "低沉 · -40Hz" },
  { value: "-20Hz", label: "稍低 · -20Hz" },
  { value: "+0Hz", label: "标准 · +0Hz" },
  { value: "+20Hz", label: "稍高 · +20Hz" },
  { value: "+40Hz", label: "明亮 · +40Hz" }
];

const PYTHON_PATH_OPTIONS: TextOption[] = [
  { value: "", label: "自动选择 python" },
  { value: "python", label: "python" },
  { value: "py", label: "Windows py launcher" },
  { value: ".venv\\Scripts\\python.exe", label: "项目 .venv" }
];

const CHATTTS_SAMPLE_RATE_OPTIONS: NumberOption[] = [
  { value: 16000, label: "16 kHz · 微信更轻" },
  { value: 24000, label: "24 kHz · 推荐" },
  { value: 32000, label: "32 kHz · 清晰" },
  { value: 48000, label: "48 kHz · 高质量" }
];

const CHATTTS_SPEED_OPTIONS: NumberOption[] = [
  { value: 3, label: "慢速" },
  { value: 4, label: "稍慢" },
  { value: 5, label: "标准" },
  { value: 6, label: "稍快" },
  { value: 7, label: "快速" }
];

const CHATTTS_LEVEL_OPTIONS: NumberOption[] = Array.from({ length: 10 }, (_, value) => ({
  value,
  label: `${value}`
}));

const CHATTTS_SPEAKER_SEED_OPTIONS: NumberOption[] = [
  { value: 20240, label: "默认种子 20240" },
  { value: 42, label: "种子 42" },
  { value: 4096, label: "种子 4096" },
  { value: 7777, label: "种子 7777" },
  { value: 114514, label: "种子 114514" }
];

const CHATTTS_STYLE_PRESETS: Array<{ value: string; label: string; patch: Partial<VoiceReplyConfig> }> = [
  { value: "natural", label: "自然 · 少口语", patch: { oral: 2, laugh: 0, breakLevel: 4, refinePrompt: "[oral_2][laugh_0][break_4]" } },
  { value: "chatty", label: "聊天 · 轻松", patch: { oral: 4, laugh: 1, breakLevel: 4, refinePrompt: "[oral_4][laugh_1][break_4]" } },
  { value: "lively", label: "活泼 · 有笑声", patch: { oral: 6, laugh: 2, breakLevel: 3, refinePrompt: "[oral_6][laugh_2][break_3]" } },
  { value: "calm", label: "温柔 · 慢停顿", patch: { oral: 1, laugh: 0, breakLevel: 6, speed: 4, refinePrompt: "[oral_1][laugh_0][break_6]" } },
  { value: "crisp", label: "利落 · 少停顿", patch: { oral: 3, laugh: 0, breakLevel: 2, speed: 6, refinePrompt: "[oral_3][laugh_0][break_2]" } }
];

const CHATTTS_SAMPLER_PRESETS: Array<{ value: string; label: string; patch: Partial<VoiceReplyConfig> }> = [
  { value: "stable", label: "稳定 · temperature 0.30", patch: { temperature: 0.3, topP: 0.7, topK: 20, refineTemperature: 0.7 } },
  { value: "expressive", label: "表现力 · temperature 0.45", patch: { temperature: 0.45, topP: 0.8, topK: 30, refineTemperature: 0.8 } },
  { value: "creative", label: "变化感 · temperature 0.60", patch: { temperature: 0.6, topP: 0.9, topK: 40, refineTemperature: 0.9 } }
];

export function PersonaPanel() {
  const {
    personas,
    emojiGroups,
    llmProviders,
    imageProviders,
    agents,
    savePersona,
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
  const [remoteVoiceReplyEnabled, setRemoteVoiceReplyEnabled] = useState<boolean | null>(null);
  const selectedIdRef = useRef(selectedId);
  const draftRef = useRef(draft);
  const voiceReplySaveQueueRef = useRef(Promise.resolve());
  const pendingVoiceReplySaveCountsRef = useRef(new Map<string, number>());
  const latestVoiceReplySaveRef = useRef(new Map<string, VoiceReplyConfig>());

  useEffect(() => {
    selectedIdRef.current = selectedId;
    setRemoteVoiceReplyEnabled(null);
  }, [selectedId]);

  useEffect(() => {
    draftRef.current = draft;
  }, [draft]);

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
    let unlisten: (() => void) | null = null;
    void listen<{
      type?: string;
      personaId?: string;
      source?: string;
      persona?: Persona;
    }>("synthchat-persona-event", (event) => {
      const payload = event.payload;
      if (payload.type !== "persona_updated") return;
      const updated = payload.persona;
      if (!updated) return;
      const preservePendingVoiceReply =
        payload.source !== "wechat" && (pendingVoiceReplySaveCountsRef.current.get(updated.id) ?? 0) > 0;
      useAppStore.setState((state) => ({
        personas: state.personas
          .map((item) => (
            item.id === updated.id
              ? {
                  ...updated,
                  voiceReply: preservePendingVoiceReply && item.voiceReply ? item.voiceReply : updated.voiceReply
                }
              : item
          ))
          .concat(state.personas.some((item) => item.id === updated.id)
            ? []
            : [{
                ...updated,
                voiceReply: preservePendingVoiceReply && draftRef.current.id === updated.id
                  ? draftRef.current.voiceReply
                  : updated.voiceReply
              }])
          .sort((a, b) => a.name.localeCompare(b.name))
      }));
      if (updated.id !== selectedIdRef.current) return;
      const updatedVoiceReplyEnabled = updated.voiceReply?.enabled;
      if (typeof updatedVoiceReplyEnabled === "boolean") {
        setRemoteVoiceReplyEnabled(updatedVoiceReplyEnabled);
      }
      setDraft((current) => {
        if (current.id !== updated.id) return current;
        const next = {
          ...updated,
          voiceReply: preservePendingVoiceReply && current.voiceReply ? current.voiceReply : updated.voiceReply
        };
        draftRef.current = next;
        return next;
      });
    }).then((handler) => {
      unlisten = handler;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    void refreshProactiveStatuses();
  }, [refreshProactiveStatuses, personas.length]);

  const provider = llmProviders.find((item) => item.id === draft.llmProvider && item.enabled) ?? null;
  const proactiveStatus = proactiveStatuses.find((status) => status.personaId === draft.id);
  const selectedImageProvider = useMemo(() => {
    const providerId = draft.imageGeneration?.provider ?? "";
    if (providerId) {
      return imageProviders.find((item) => item.id === providerId);
    }
    return imageProviders.find((item) => item.enabled && item.model.trim()) ?? imageProviders[0];
  }, [draft.imageGeneration?.provider, imageProviders]);
  const effectiveLlmModelId = (
    draft.llmModel ||
    provider?.model ||
    catalogModels[0]?.id ||
    ""
  ).trim();
  const effectiveImageModelId = (selectedImageProvider?.model ?? "").trim();
  const voiceReply = { ...defaultVoiceReplyConfig(), ...(draft.voiceReply ?? {}) };
  const voiceReplyEnabled = remoteVoiceReplyEnabled ?? voiceReply.enabled;
  const activeVoiceEngine = voiceReply.engine || "chattts";
  const activeEdgeLanguage = voiceReply.language || "zh-CN";
  const activeEdgeVoices = EDGE_VOICE_OPTIONS[activeEdgeLanguage] ?? EDGE_VOICE_OPTIONS["zh-CN"];
  const chatttsSpeakerMode = voiceReply.speakerEmbedding
    ? "fixed"
    : voiceReply.speakerSeed > 0
      ? "seed"
      : "random";

  useEffect(() => {
    const nextModel = (provider?.model || catalogModels[0]?.id || "").trim();
    if (!nextModel) return;
    setDraft((current) => {
      const currentProviderId = current.llmProvider || "";
      const activeProviderId = provider?.id || "";
      if (currentProviderId !== activeProviderId && current.llmProvider) return current;
      const currentModel = current.llmModel.trim();
      if (currentModel) return current;
      return { ...current, llmModel: nextModel };
    });
  }, [catalogModels, provider?.id, provider?.model]);

  const updateDraft = <K extends keyof Persona>(key: K, value: Persona[K]) => {
    setDraft((current) => ({ ...current, [key]: value }));
  };

  const mergeVoiceReply = (persona: Persona, patch: Partial<VoiceReplyConfig>): Persona => ({
    ...persona,
    voiceReply: { ...defaultVoiceReplyConfig(), ...(persona.voiceReply ?? {}), ...patch }
  });

  useEffect(() => {
    if (tab !== "behavior" || draft.id.startsWith("persona-")) return;
    let cancelled = false;
    const refreshVoiceReplyEnabled = async () => {
      const personaId = selectedIdRef.current;
      if (!personaId || personaId.startsWith("persona-")) return;
      try {
        const latest = await api.getPersona(personaId);
        const enabled = latest.voiceReply?.enabled;
        if (!cancelled && personaId === selectedIdRef.current && typeof enabled === "boolean") {
          setRemoteVoiceReplyEnabled(enabled);
        }
      } catch (error) {
        console.warn("persona voice reply refresh failed:", error);
      }
    };
    void refreshVoiceReplyEnabled();
    const timer = window.setInterval(() => {
      void refreshVoiceReplyEnabled();
    }, 1200);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [draft.id, tab]);

  const syncPersonaInStore = (next: Persona) => {
    useAppStore.setState((state) => ({
      personas: state.personas
        .map((item) => (item.id === next.id ? next : item))
        .concat(state.personas.some((item) => item.id === next.id) ? [] : [next])
        .sort((a, b) => a.name.localeCompare(b.name))
    }));
  };

  const queueVoiceReplySave = (
    personaId: string,
    voiceReply: VoiceReplyConfig,
    options: { preserveEnabled?: boolean } = {}
  ) => {
    const voiceReplySnapshot = { ...defaultVoiceReplyConfig(), ...voiceReply };
    latestVoiceReplySaveRef.current.set(personaId, voiceReplySnapshot);
    pendingVoiceReplySaveCountsRef.current.set(
      personaId,
      (pendingVoiceReplySaveCountsRef.current.get(personaId) ?? 0) + 1
    );
    voiceReplySaveQueueRef.current = voiceReplySaveQueueRef.current
      .then(async () => {
        if (latestVoiceReplySaveRef.current.get(personaId) !== voiceReplySnapshot) return;
        const localPersona =
          useAppStore.getState().personas.find((persona) => persona.id === personaId)
          ?? draftRef.current;
        const latestPersona = personaId.startsWith("persona-")
          ? localPersona
          : await api.getPersona(personaId).catch(() => localPersona);
        if (latestVoiceReplySaveRef.current.get(personaId) !== voiceReplySnapshot) return;
        const latestVoiceReply = { ...defaultVoiceReplyConfig(), ...(latestPersona.voiceReply ?? {}) };
        const voiceReplyToSave = options.preserveEnabled && typeof latestVoiceReply.enabled === "boolean"
          ? { ...voiceReplySnapshot, enabled: latestVoiceReply.enabled }
          : voiceReplySnapshot;
        const next = { ...latestPersona, voiceReply: voiceReplyToSave };
        if (latestVoiceReplySaveRef.current.get(personaId) !== voiceReplySnapshot) return;
        const saved = await api.savePersona(next);
        if (latestVoiceReplySaveRef.current.get(personaId) !== voiceReplySnapshot) return;
        syncPersonaInStore(saved);
        if (saved.id === selectedIdRef.current) {
          setDraft((current) => {
            if (current.id !== saved.id) return current;
            const merged = { ...current, voiceReply: saved.voiceReply };
            draftRef.current = merged;
            return merged;
          });
        }
      })
      .catch((error) => {
        console.error("voice reply save failed:", error);
      })
      .finally(() => {
        const count = pendingVoiceReplySaveCountsRef.current.get(personaId) ?? 0;
        if (count <= 1) {
          pendingVoiceReplySaveCountsRef.current.delete(personaId);
          if (latestVoiceReplySaveRef.current.get(personaId) === voiceReplySnapshot) {
            latestVoiceReplySaveRef.current.delete(personaId);
          }
        } else {
          pendingVoiceReplySaveCountsRef.current.set(personaId, count - 1);
        }
      });
  };

  const updateVoiceReply = (patch: Partial<VoiceReplyConfig>) => {
    if ("enabled" in patch) setRemoteVoiceReplyEnabled(null);
    const next = mergeVoiceReply(draftRef.current, patch);
    draftRef.current = next;
    setDraft(next);
    syncPersonaInStore(next);
    void emit("synthchat-persona-event", {
      type: "persona_updated",
      source: "desktop-local",
      personaId: next.id,
      persona: next
    });
    queueVoiceReplySave(next.id, next.voiceReply ?? defaultVoiceReplyConfig(), {
      preserveEnabled: !("enabled" in patch)
    });
  };

  const updateWechatVoiceReplyEnabled = (enabled: boolean) => {
    updateVoiceReply({ enabled });
  };

  const currentDraftSnapshot = () => draftRef.current;

  const save = async () => {
    setSaving(true);
    try {
      await voiceReplySaveQueueRef.current;
      const draftSnapshot = currentDraftSnapshot();
      const imageGeneration = {
        ...(draftSnapshot.imageGeneration ?? defaultImageGenerationConfig()),
        model: ""
      };
      const saved = await savePersona({
        ...draftSnapshot,
        llmProvider: draftSnapshot.llmProvider,
        llmModel: effectiveLlmModelId || draftSnapshot.llmModel,
        imageGeneration
      });
      setSelectedId(saved.id);
      draftRef.current = saved;
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
    const draftSnapshot = currentDraftSnapshot();
    let targetId = draftSnapshot.id;
    if (draftSnapshot.id.startsWith("persona-")) {
      const savedPersona = await savePersona(draftSnapshot);
      setSelectedId(savedPersona.id);
      draftRef.current = savedPersona;
      setDraft(savedPersona);
      targetId = savedPersona.id;
    }
    const saved = await uploadPersonaAvatar(targetId, file);
    draftRef.current = saved;
    setDraft(saved);
  };

  const avatarSrc = draft.avatarPath ? api.assetUrl(draft.avatarPath) : "";
  const personaBindings = useMemo(
    () => new Map(personas.map((persona) => [persona.id, resolvePersonaAgentBinding(persona, agents, llmProviders)])),
    [agents, llmProviders, personas]
  );

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
            const binding = personaBindings.get(persona.id);
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
                  <small>{binding?.infoText ?? "未配置服务商"}</small>
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
                    value={catalogModels.some((model) => model.id === effectiveLlmModelId) ? effectiveLlmModelId : ""}
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
                  value={effectiveLlmModelId}
                  readOnly
                  placeholder="自动使用服务商模型"
                />
              </div>
              <p className="form-hint" style={{ marginTop: 6, marginBottom: 0 }}>
                模型为空时会填入当前服务商默认模型或目录首个模型；保存后以通讯录中的模型 ID 为准。
              </p>
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
                  const draftSnapshot = currentDraftSnapshot();
                  const savedPersona = await savePersona(draftSnapshot);
                  draftRef.current = savedPersona;
                  setDraft(savedPersona);
                  await triggerProactiveOnce(savedPersona.id);
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

            <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
              <label className="checkbox-row" style={{ marginBottom: 12 }}>
                <input
                  checked={voiceReplyEnabled}
                  onChange={(event) => {
                    void updateWechatVoiceReplyEnabled(event.target.checked);
                  }}
                  type="checkbox"
                />
                <span style={{ fontWeight: 500 }}>微信端发送语音回复</span>
              </label>
              <div className="two-column" style={{ marginBottom: 0 }}>
                <label>
                  TTS 引擎
                  <select
                    value={activeVoiceEngine}
                    onChange={(event) => {
                      const engine = event.target.value;
                      updateVoiceReply({
                        engine,
                        ...(engine === "edge" ? { language: activeEdgeLanguage, voice: activeEdgeVoices[0]?.value ?? "zh-CN-XiaoxiaoNeural" } : {})
                      });
                    }}
                  >
                    {TTS_ENGINE_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>{option.label}</option>
                    ))}
                  </select>
                </label>
                <label>
                  桌面/Pet 播放格式
                  <select value="wav" disabled>
                    <option value="wav">WAV 本地播放</option>
                  </select>
                </label>
              </div>
              <p className="form-hint" style={{ marginTop: 8, marginBottom: 0, fontSize: 11 }}>
                Pet 端的语音回复总开关仍在 Pet 窗口；这里仅控制微信端是否发送语音。
              </p>
            </div>

            {activeVoiceEngine === "edge" ? (
              <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                  <Settings size={14} />
                  EdgeTTS 参数
                </div>
                <div className="two-column" style={{ marginBottom: 12 }}>
                  <label>
                    语言
                    <select
                      value={activeEdgeLanguage}
                      onChange={(event) => {
                        const language = event.target.value;
                        const firstVoice = EDGE_VOICE_OPTIONS[language]?.[0]?.value ?? "zh-CN-XiaoxiaoNeural";
                        updateVoiceReply({ language, voice: firstVoice });
                      }}
                    >
                      {EDGE_LANGUAGE_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>{option.label}</option>
                      ))}
                    </select>
                  </label>
                  <label>
                    音色
                    <select
                      value={voiceReply.voice}
                      onChange={(event) => updateVoiceReply({ voice: event.target.value })}
                    >
                      {!activeEdgeVoices.some((option) => option.value === voiceReply.voice) && voiceReply.voice ? (
                        <option value={voiceReply.voice}>当前音色 · {voiceReply.voice}</option>
                      ) : null}
                      {activeEdgeVoices.map((option) => (
                        <option key={option.value} value={option.value}>{option.label}</option>
                      ))}
                    </select>
                  </label>
                  <label>
                    语速
                    <select
                      value={voiceReply.speed}
                      onChange={(event) => updateVoiceReply({ speed: Number(event.target.value) })}
                    >
                      {EDGE_SPEED_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>{option.label}</option>
                      ))}
                    </select>
                  </label>
                  <label>
                    音量
                    <select
                      value={voiceReply.volume || "+0%"}
                      onChange={(event) => updateVoiceReply({ volume: event.target.value })}
                    >
                      {EDGE_VOLUME_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>{option.label}</option>
                      ))}
                    </select>
                  </label>
                  <label>
                    音调
                    <select
                      value={voiceReply.pitch || "+0Hz"}
                      onChange={(event) => updateVoiceReply({ pitch: event.target.value })}
                    >
                      {EDGE_PITCH_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>{option.label}</option>
                      ))}
                    </select>
                  </label>
                </div>
                <p className="form-hint" style={{ margin: 0, fontSize: 11 }}>
                  EdgeTTS 会使用当前语言和音色；英文回复不完整的问题已通过文本清洗和中文默认音色规避，标点与表情会先被清理再合成。
                </p>
              </div>
            ) : null}

            {activeVoiceEngine === "chattts" ? (
              <>
                <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                    <Settings size={14} />
                    ChatTTS 运行配置
                  </div>
                  <div className="two-column" style={{ marginBottom: 12 }}>
                    <div style={{ display: "grid", gap: 6 }}>
                      <span>模型目录</span>
                      <div className="model-select-row">
                        <input
                          id="chattts-model-dir"
                          readOnly
                          value={voiceReply.modelDir}
                          placeholder="点击右侧浏览选择 ChatTTS 目录"
                        />
                        <button
                          type="button"
                          onClick={async (event) => {
                            event.preventDefault();
                            event.stopPropagation();
                            const path = await api.pickFolder("选择 ChatTTS 模型目录");
                            if (path) updateVoiceReply({ modelDir: path });
                          }}
                          style={{ display: "inline-flex", alignItems: "center", gap: 6 }}
                          title="浏览选择 ChatTTS 模型目录"
                        >
                          <FolderOpen size={14} />
                          浏览
                        </button>
                      </div>
                    </div>
                    <label>
                      Python
                      <select
                        value={voiceReply.pythonPath}
                        onChange={(event) => updateVoiceReply({ pythonPath: event.target.value })}
                      >
                        {!PYTHON_PATH_OPTIONS.some((option) => option.value === voiceReply.pythonPath) && voiceReply.pythonPath ? (
                          <option value={voiceReply.pythonPath}>当前自定义 Python</option>
                        ) : null}
                        {PYTHON_PATH_OPTIONS.map((option) => (
                          <option key={option.value || "auto"} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      采样率
                      <select
                        value={voiceReply.sampleRate}
                        onChange={(event) => updateVoiceReply({ sampleRate: Number(event.target.value) })}
                      >
                        {CHATTTS_SAMPLE_RATE_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      语速
                      <select
                        value={voiceReply.speed}
                        onChange={(event) => updateVoiceReply({ speed: Number(event.target.value) })}
                      >
                        {CHATTTS_SPEED_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                  </div>
                  <p className="form-hint" style={{ marginTop: 8, marginBottom: 0, fontSize: 11 }}>
                    请选择 ChatTTS 的安装目录，路径会直接写入当前角色配置。
                  </p>
                </div>

                <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                    <FileAudio size={14} />
                    ChatTTS 音色
                  </div>
                  <div className="two-column" style={{ marginBottom: 12 }}>
                    <label>
                      固定音色
                      <select
                        value={chatttsSpeakerMode}
                        onChange={async (event) => {
                          const mode = event.target.value;
                          if (mode === "fixed") {
                            if (voiceReply.speakerEmbedding) {
                              updateVoiceReply({ speakerSeed: 0 });
                              return;
                            }
                            const path = await api.pickFile("选择 Speaker Embedding 文件", "Embedding 文件", ["pt", "pth", "safetensors"]);
                            if (path) updateVoiceReply({ speakerEmbedding: path, speakerSeed: 0 });
                            return;
                          }
                          if (mode === "random") {
                            updateVoiceReply({ speakerEmbedding: "", speakerSeed: 0 });
                            return;
                          }
                          updateVoiceReply({
                            speakerEmbedding: "",
                            speakerSeed: voiceReply.speakerSeed > 0 ? voiceReply.speakerSeed : 20240
                          });
                        }}
                      >
                        <option value="fixed">使用 embedding 固定音色</option>
                        <option value="random">随机音色</option>
                        <option value="seed">使用音色种子</option>
                      </select>
                    </label>
                    {chatttsSpeakerMode === "seed" ? (
                      <label>
                        音色种子
                        <select
                          value={voiceReply.speakerSeed > 0 ? voiceReply.speakerSeed : 20240}
                          onChange={(event) => updateVoiceReply({ speakerEmbedding: "", speakerSeed: Number(event.target.value) })}
                        >
                          {!CHATTTS_SPEAKER_SEED_OPTIONS.some((option) => option.value === voiceReply.speakerSeed) && voiceReply.speakerSeed > 0 ? (
                            <option value={voiceReply.speakerSeed}>当前种子 {voiceReply.speakerSeed}</option>
                          ) : null}
                          {CHATTTS_SPEAKER_SEED_OPTIONS.map((option) => (
                            <option key={option.value} value={option.value}>{option.label}</option>
                          ))}
                        </select>
                      </label>
                    ) : null}
                  </div>
                  <div style={{ padding: "12px", background: "var(--surface-2)", borderRadius: "var(--radius-md)", border: "1px solid var(--divider)" }}>
                    <div className="detail-row" style={{ paddingTop: 0, borderTop: 0 }}>
                      <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                        <Sparkles size={13} style={{ color: "var(--primary)" }} />
                        Speaker Embedding
                      </span>
                      <strong style={{ color: voiceReply.speakerEmbedding ? "var(--success)" : "var(--text-3)" }}>
                        {voiceReply.speakerEmbedding ? "已固定" : "未选择"}
                      </strong>
                    </div>
                    <div style={{ display: "flex", gap: 8, marginTop: 8, alignItems: "center" }}>
                        <input
                          readOnly
                          value={
                            voiceReply.speakerEmbedding ||
                            (chatttsSpeakerMode === "random" ? "随机音色" : "按音色种子")
                          }
                          style={{ fontFamily: "var(--font-mono)", fontSize: 13, flex: 1 }}
                        />
                      <button
                        type="button"
                        onClick={async () => {
                          const path = await api.pickFile("选择 Speaker Embedding 文件", "Embedding 文件", ["pt", "pth", "safetensors"]);
                          if (path) updateVoiceReply({ speakerEmbedding: path, speakerSeed: 0 });
                        }}
                        style={{ display: "inline-flex", alignItems: "center", gap: 4, padding: "0 12px", height: 38, border: "1px solid var(--divider)", borderRadius: "var(--radius-sm)", background: "var(--card)", color: "var(--text-2)", cursor: "pointer", fontSize: 13, whiteSpace: "nowrap", flexShrink: 0 }}
                        title="浏览选择文件"
                      >
                        <FolderOpen size={14} />
                        浏览
                      </button>
                      {voiceReply.speakerEmbedding ? (
                        <button
                          type="button"
                          onClick={() => updateVoiceReply({ speakerEmbedding: "", speakerSeed: 20240 })}
                          style={{ display: "inline-flex", alignItems: "center", gap: 4, padding: "0 12px", height: 38, border: "1px solid var(--danger)", borderRadius: "var(--radius-sm)", background: "transparent", color: "var(--danger)", cursor: "pointer", fontSize: 13, whiteSpace: "nowrap", flexShrink: 0 }}
                          title="清除路径"
                        >
                          <Trash2 size={14} />
                        </button>
                      ) : null}
                    </div>
                  </div>
                </div>

                <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
                  <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                    <Wand2 size={14} />
                    ChatTTS 风格
                  </div>
                  <div className="two-column" style={{ marginBottom: 12 }}>
                    <label>
                      风格预设
                      <select
                        value=""
                        onChange={(event) => {
                          const preset = CHATTTS_STYLE_PRESETS.find((item) => item.value === event.target.value);
                          if (preset) updateVoiceReply(preset.patch);
                        }}
                      >
                        <option value="">选择后套用</option>
                        {CHATTTS_STYLE_PRESETS.map((preset) => (
                          <option key={preset.value} value={preset.value}>{preset.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      采样预设
                      <select
                        value=""
                        onChange={(event) => {
                          const preset = CHATTTS_SAMPLER_PRESETS.find((item) => item.value === event.target.value);
                          if (preset) updateVoiceReply(preset.patch);
                        }}
                      >
                        <option value="">选择后套用</option>
                        {CHATTTS_SAMPLER_PRESETS.map((preset) => (
                          <option key={preset.value} value={preset.value}>{preset.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      口语化
                      <select value={voiceReply.oral} onChange={(event) => updateVoiceReply({ oral: Number(event.target.value) })}>
                        {CHATTTS_LEVEL_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      笑声
                      <select value={voiceReply.laugh} onChange={(event) => updateVoiceReply({ laugh: Number(event.target.value) })}>
                        {CHATTTS_LEVEL_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      停顿
                      <select value={voiceReply.breakLevel} onChange={(event) => updateVoiceReply({ breakLevel: Number(event.target.value) })}>
                        {CHATTTS_LEVEL_OPTIONS.map((option) => (
                          <option key={option.value} value={option.value}>{option.label}</option>
                        ))}
                      </select>
                    </label>
                    <label>
                      文本润色
                      <select
                        value={voiceReply.refineTextEnabled ? "on" : "off"}
                        onChange={(event) => updateVoiceReply({ refineTextEnabled: event.target.value === "on" })}
                      >
                        <option value="on">启用</option>
                        <option value="off">关闭</option>
                      </select>
                    </label>
                  </div>
                  <div className="two-column" style={{ marginBottom: 0 }}>
                    <label>
                      temperature
                      <select value={voiceReply.temperature} onChange={(event) => updateVoiceReply({ temperature: Number(event.target.value) })}>
                        <option value={0.3}>0.30 · 稳定</option>
                        <option value={0.45}>0.45 · 表现力</option>
                        <option value={0.6}>0.60 · 变化感</option>
                      </select>
                    </label>
                    <label>
                      top_p
                      <select value={voiceReply.topP} onChange={(event) => updateVoiceReply({ topP: Number(event.target.value) })}>
                        <option value={0.7}>0.70 · 稳定</option>
                        <option value={0.8}>0.80 · 均衡</option>
                        <option value={0.9}>0.90 · 开放</option>
                      </select>
                    </label>
                    <label>
                      top_k
                      <select value={voiceReply.topK} onChange={(event) => updateVoiceReply({ topK: Number(event.target.value) })}>
                        <option value={20}>20 · 稳定</option>
                        <option value={30}>30 · 均衡</option>
                        <option value={40}>40 · 开放</option>
                      </select>
                    </label>
                    <label>
                      润色 temperature
                      <select value={voiceReply.refineTemperature} onChange={(event) => updateVoiceReply({ refineTemperature: Number(event.target.value) })}>
                        <option value={0.7}>0.70 · 稳定</option>
                        <option value={0.8}>0.80 · 均衡</option>
                        <option value={0.9}>0.90 · 更自然</option>
                      </select>
                    </label>
                  </div>
                  <p className="form-hint" style={{ marginTop: 8, marginBottom: 0, fontSize: 11 }}>
                    当前 refine prompt：{voiceReply.refinePrompt || "[oral/laugh/break 自动组合]"}
                  </p>
                </div>
              </>
            ) : null}

            {activeVoiceEngine === "local_command" ? (
              <div className="card" style={{ padding: "14px 16px", marginBottom: 12 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 12, fontSize: 13, fontWeight: 600, color: "var(--text-2)" }}>
                  <Settings size={14} />
                  本地命令 TTS
                </div>
                <div className="two-column" style={{ marginBottom: 12 }}>
                  <label>
                    音色参数
                    <select
                      value={voiceReply.voice || "default"}
                      onChange={(event) => updateVoiceReply({ voice: event.target.value === "default" ? "" : event.target.value })}
                    >
                      {voiceReply.voice && !["female", "male", "default_voice"].includes(voiceReply.voice) ? (
                        <option value={voiceReply.voice}>当前自定义音色 · {voiceReply.voice}</option>
                      ) : null}
                      <option value="default">使用命令默认音色</option>
                      <option value="female">female</option>
                      <option value="male">male</option>
                      <option value="default_voice">default_voice</option>
                    </select>
                  </label>
                  <label>
                    语速
                    <select
                      value={voiceReply.speed}
                      onChange={(event) => updateVoiceReply({ speed: Number(event.target.value) })}
                    >
                      {CHATTTS_SPEED_OPTIONS.map((option) => (
                        <option key={option.value} value={option.value}>{option.label}</option>
                      ))}
                    </select>
                  </label>
                </div>
                <p className="form-hint" style={{ margin: 0, fontSize: 11 }}>
                  本地命令仍通过 SYNTHCHAT_LOCAL_TTS_COMMAND 或 HERMES_LOCAL_TTS_COMMAND 接入，模板可使用 {"{input_path}"}、{"{output_path}"}、{"{voice}"}、{"{speed}"}。
                </p>
              </div>
            ) : null}
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
                <select value={draft.imageGeneration?.provider ?? ""} onChange={(event) => setDraft((current) => ({ ...current, imageGeneration: { ...(current.imageGeneration ?? defaultImageGenerationConfig()), provider: event.target.value, model: "" } }))}>
                  <option value="">使用默认启用服务商</option>
                  {imageProviders.map((item) => (
                    <option key={item.id} value={item.id}>{item.name}{item.model ? ` · ${item.model}` : ""}</option>
                  ))}
                </select>
              </label>
              <label>
                生图模型
                <input value={effectiveImageModelId || "未配置"} readOnly />
                <p className="form-hint" style={{ marginTop: 6, marginBottom: 0 }}>
                  自动使用所选生图服务商的模型 ID；请在设置页维护服务商模型。
                </p>
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
            <div className="card" style={{ margin: "0 0 12px" }}>
              <div className="card-header">工具调用开关</div>
              <label className="checkbox-row" style={{ padding: "12px 16px" }}>
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
              <p className="form-hint" style={{ padding: "0 16px 10px" }}>开启后角色可使用已启用的 MCP 工具；最大迭代作为本角色会话的工具循环预算，并与绑定 Agent 的 fallback 工具迭代双向同步。</p>
            </div>

            <div className="card" style={{ margin: "0 0 12px", opacity: draft.toolPolicy.enabled ? 1 : 0.5, pointerEvents: draft.toolPolicy.enabled ? "auto" : "none" }}>
              <div className="card-header">循环与超时</div>
              <div className="form-group">
                <div className="form-row">
                  <label>单次超时（秒）</label>
                  <div className="stepper">
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, timeoutSeconds: Math.max(1, c.toolPolicy.timeoutSeconds - 5) } }))} type="button">−</button>
                    <span className="stepper-val">{draft.toolPolicy.timeoutSeconds}</span>
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, timeoutSeconds: c.toolPolicy.timeoutSeconds + 5 } }))} type="button">+</button>
                  </div>
                </div>
                <div className="form-row">
                  <label>最大迭代次数</label>
                  <div className="stepper">
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, maxIterations: Math.max(1, c.toolPolicy.maxIterations - 5) } }))} type="button">−</button>
                    <span className="stepper-val">{draft.toolPolicy.maxIterations}</span>
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, maxIterations: Math.min(90, c.toolPolicy.maxIterations + 5) } }))} type="button">+</button>
                  </div>
                </div>
                <div className="form-row">
                  <label>失败重规划</label>
                  <div className="stepper">
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, maxFailureReplans: Math.max(0, (c.toolPolicy.maxFailureReplans ?? 2) - 1) } }))} type="button">−</button>
                    <span className="stepper-val">{draft.toolPolicy.maxFailureReplans ?? 2}</span>
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, maxFailureReplans: Math.min(32, (c.toolPolicy.maxFailureReplans ?? 2) + 1) } }))} type="button">+</button>
                  </div>
                </div>
              </div>
            </div>

            <div className="card" style={{ margin: "0 0 12px", opacity: draft.toolPolicy.enabled ? 1 : 0.5, pointerEvents: draft.toolPolicy.enabled ? "auto" : "none" }}>
              <div className="card-header">重试策略</div>
              <div className="form-group">
                <div className="form-row">
                  <label>重试次数</label>
                  <div className="stepper">
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, retryCount: Math.max(0, (c.toolPolicy.retryCount ?? 1) - 1) } }))} type="button">−</button>
                    <span className="stepper-val">{draft.toolPolicy.retryCount ?? 1}</span>
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, retryCount: Math.min(5, (c.toolPolicy.retryCount ?? 1) + 1) } }))} type="button">+</button>
                  </div>
                </div>
                <div className="form-row">
                  <label>退避时间（ms）</label>
                  <div className="stepper">
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, retryBackoffMs: Math.max(0, (c.toolPolicy.retryBackoffMs ?? 300) - 100) } }))} type="button">−</button>
                    <span className="stepper-val">{draft.toolPolicy.retryBackoffMs ?? 300}</span>
                    <button onClick={() => setDraft((c) => ({ ...c, toolPolicy: { ...c.toolPolicy, retryBackoffMs: Math.min(10000, (c.toolPolicy.retryBackoffMs ?? 300) + 100) } }))} type="button">+</button>
                  </div>
                </div>
              </div>
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
      maxIterations: 90,
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
    language: "zh-CN",
    voice: "zh-CN-XiaoxiaoNeural",
    volume: "+0%",
    pitch: "+0Hz",
    pythonPath: "",
    modelDir: "",
    sampleRate: 16000,
    speed: 5,
    oral: 2,
    laugh: 0,
    breakLevel: 4,
    speakerSeed: 20240,
    speakerEmbedding: "",
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
