import { BookOpen, Brain, Database, MessageSquareText, RefreshCw, Trash2 } from "lucide-react";
import type { ChatConfig, MemoryEntry, Persona } from "../lib/types";

function defaultMemoryConfig(): NonNullable<Persona["memory"]> {
  return { enabled: true, triggerRounds: 10, maxMemories: 50, includeInPrompt: true };
}

export function PersonaMemoryManager({
  bindingModel,
  bindingProviderName,
  chatConfig,
  isDraftPersona = false,
  onDeleteMemory,
  onRefresh,
  onSaveChatConfig,
  onUpdateMemory,
  onViewAll,
  persistentMemories,
  personaMemory,
  sessionMemories
}: {
  bindingModel: string;
  bindingProviderName: string;
  chatConfig: ChatConfig | null;
  isDraftPersona?: boolean;
  onDeleteMemory: (id: string) => Promise<void>;
  onRefresh: () => void;
  onSaveChatConfig: (patch: Partial<ChatConfig>) => Promise<void>;
  onUpdateMemory: (memory: NonNullable<Persona["memory"]>) => Promise<void> | void;
  onViewAll?: () => void;
  persistentMemories: MemoryEntry[];
  personaMemory?: Persona["memory"];
  sessionMemories: MemoryEntry[];
}) {
  const memory = { ...defaultMemoryConfig(), ...(personaMemory ?? {}) };
  const promptLimit = Math.max(1, memory.maxMemories ?? 50);
  const providerLabel = [bindingProviderName, bindingModel].filter(Boolean).join(" / ") || "跟随当前角色服务商";

  return (
    <div className="persona-memory-manager">
      <div className="memory-manager-head">
        <div>
          <span>Memory</span>
          <strong>记忆管理</strong>
        </div>
        <div className="row-actions">
          <button className="ghost-button compact" disabled={isDraftPersona} onClick={onRefresh} type="button"><RefreshCw size={14} />刷新</button>
          {onViewAll ? <button className="ghost-button compact" onClick={onViewAll} type="button"><BookOpen size={14} />全局记忆</button> : null}
        </div>
      </div>
      <div className="memory-module-grid">
        <section className="memory-module-card persistent">
          <div className="memory-module-title">
            <Database size={17} />
            <div>
              <strong>长期持久记忆</strong>
              <small>链接 memory 系统，稳定注入角色提示词</small>
            </div>
            <span className={`status-badge ${memory.enabled ? "enabled" : "disabled"}`}>{memory.enabled ? "ON" : "OFF"}</span>
          </div>
          <div className="memory-module-controls">
            <label className="checkbox-row">
              <input checked={memory.enabled} onChange={(event) => void onUpdateMemory({ ...memory, enabled: event.target.checked })} type="checkbox" />
              启用长期持久记忆
            </label>
            <label className="checkbox-row">
              <input checked={memory.includeInPrompt ?? true} onChange={(event) => void onUpdateMemory({ ...memory, includeInPrompt: event.target.checked })} type="checkbox" />
              注入当前角色提示词
            </label>
            <label>
              注入上限
              <input min={1} max={500} type="number" value={promptLimit} onChange={(event) => void onUpdateMemory({ ...memory, maxMemories: Math.max(1, Number(event.target.value) || 1) })} />
            </label>
          </div>
          <MemoryPreviewList emptyText={isDraftPersona ? "保存角色后可查看长期持久记忆" : "暂无长期持久记忆"} memories={persistentMemories} onDelete={onDeleteMemory} />
        </section>
        <section className="memory-module-card session">
          <div className="memory-module-title">
            <MessageSquareText size={17} />
            <div>
              <strong>短期会话记忆</strong>
              <small>左侧删除会话时，调用当前服务商整理后存入</small>
            </div>
            <span className={`status-badge ${chatConfig?.backgroundMemoryReviewEnabled !== false ? "enabled" : "disabled"}`}>{chatConfig?.backgroundMemoryReviewEnabled !== false ? "ON" : "OFF"}</span>
          </div>
          {chatConfig ? (
            <SessionMemorySettings config={chatConfig} onSave={onSaveChatConfig} providerLabel={providerLabel} />
          ) : (
            <div className="form-hint compact">配置尚未加载，暂不可编辑短期会话记忆策略。</div>
          )}
          <MemoryPreviewList emptyText={isDraftPersona ? "保存角色后可查看会话整理记忆" : "暂无会话整理记忆"} memories={sessionMemories} onDelete={onDeleteMemory} />
        </section>
        <section className="memory-module-card context">
          <div className="memory-module-title">
            <Brain size={17} />
            <div>
              <strong>短时上下文记忆</strong>
              <small>控制当前会话历史窗口与摘要压缩策略</small>
            </div>
          </div>
          {chatConfig ? (
            <ShortContextSettings config={chatConfig} onSave={onSaveChatConfig} />
          ) : (
            <div className="form-hint compact">配置尚未加载，暂不可编辑短时上下文策略。</div>
          )}
        </section>
      </div>
    </div>
  );
}

function ShortContextSettings({
  config,
  onSave
}: {
  config: ChatConfig;
  onSave: (patch: Partial<ChatConfig>) => Promise<void>;
}) {
  const mode = config.shortContextMode === "tokens" ? "tokens" : "messages";
  const messages = Math.max(1, config.maxContextRounds ?? 10);
  const tokenK = Math.max(1, Math.round((config.shortContextTokenBudget ?? 8000) / 1000));
  const savePatch = (patch: Partial<ChatConfig>) => {
    void onSave(patch);
  };
  return (
    <div className="memory-module-controls memory-context-controls">
      <label>
        短时记忆策略
        <select value={mode} onChange={(event) => savePatch({ shortContextMode: event.target.value === "tokens" ? "tokens" : "messages" })}>
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
            onChange={(event) => savePatch({ maxContextRounds: Math.min(500, Math.max(1, Number(event.target.value) || 1)) })}
          />
        </label>
      ) : (
        <label>
          Token 预算（K）
          <input
            min={1}
            max={500}
            type="number"
            value={tokenK}
            onChange={(event) => savePatch({ shortContextTokenBudget: Math.min(500, Math.max(1, Number(event.target.value) || 1)) * 1000 })}
          />
        </label>
      )}
      <div className="form-hint compact">达到窗口上限后，旧片段会压缩为短时摘要继续参与当前会话。</div>
    </div>
  );
}

function MemoryPreviewList({
  emptyText,
  memories,
  onDelete
}: {
  emptyText: string;
  memories: MemoryEntry[];
  onDelete: (id: string) => Promise<void>;
}) {
  const visible = memories.slice(0, 4);
  if (visible.length === 0) {
    return <div className="memory-preview-empty">{emptyText}</div>;
  }
  return (
    <div className="memory-preview-list">
      {visible.map((memory) => (
        <div className="memory-preview-item" key={memory.id}>
          <div>
            <strong>{memory.summary}</strong>
            <small>{memory.target ?? "memory"} · 重要度 {memory.importance} · {new Date(memory.updatedAt || memory.createdAt).toLocaleString()}</small>
          </div>
          <button className="icon-only-btn subtle" onClick={() => void onDelete(memory.id)} title="删除记忆" type="button">
            <Trash2 size={14} />
          </button>
        </div>
      ))}
    </div>
  );
}

function SessionMemorySettings({
  config,
  onSave,
  providerLabel
}: {
  config: ChatConfig;
  onSave: (patch: Partial<ChatConfig>) => Promise<void>;
  providerLabel: string;
}) {
  const enabled = config.backgroundMemoryReviewEnabled !== false;
  const minMessages = Math.max(2, config.backgroundMemoryReviewMinMessages ?? 4);
  const savePatch = (patch: Partial<ChatConfig>) => {
    void onSave(patch);
  };
  return (
    <div className="memory-module-controls">
      <label className="checkbox-row">
        <input checked={enabled} onChange={(event) => savePatch({ backgroundMemoryReviewEnabled: event.target.checked })} type="checkbox" />
        删除会话时整理会话记忆
      </label>
      <label>
        整理最少消息数
        <input
          min={2}
          max={40}
          type="number"
          value={minMessages}
          onChange={(event) => savePatch({ backgroundMemoryReviewMinMessages: Math.min(40, Math.max(2, Number(event.target.value) || 2)) })}
        />
      </label>
      <div className="session-memory-route">
        <Brain size={14} />
        <span>{providerLabel}</span>
      </div>
    </div>
  );
}
