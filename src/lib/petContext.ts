export const PET_ACTIVE_CONTEXT_EVENT = "synthchat-pet-active-context";
export const PET_ACTIVE_CONTEXT_STORAGE_KEY = "synthchat.pet.activeContext";
export const PET_THINKING_STATE_EVENT = "synthchat-pet-thinking-state";
export const PET_THINKING_STATE_STORAGE_KEY = "synthchat.pet.thinkingState";
const PET_THINKING_STATE_CHANNEL = "synthchat.pet.thinkingState.channel";

export interface PetActiveContext {
  conversationId: string;
  conversationTitle: string | null;
  personaId: string | null;
  personaName: string | null;
  agentId: string | null;
  updatedAt: string;
  source?: string;
}

export interface PetThinkingState {
  conversationId: string | null;
  personaId: string | null;
  source: string;
  thinking: boolean;
  updatedAt: string;
}

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
}

function optionalBoolean(value: unknown): boolean | null {
  return typeof value === "boolean" ? value : null;
}

export function parsePetActiveContext(value: unknown): PetActiveContext | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  const conversationId = optionalString(record.conversationId);
  if (!conversationId) return null;
  return {
    conversationId,
    conversationTitle: optionalString(record.conversationTitle),
    personaId: optionalString(record.personaId),
    personaName: optionalString(record.personaName),
    agentId: optionalString(record.agentId),
    updatedAt: optionalString(record.updatedAt) ?? new Date().toISOString(),
    source: optionalString(record.source) ?? undefined
  };
}

export function readStoredPetActiveContext(): PetActiveContext | null {
  try {
    const raw = window.localStorage.getItem(PET_ACTIVE_CONTEXT_STORAGE_KEY);
    if (!raw) return null;
    return parsePetActiveContext(JSON.parse(raw));
  } catch {
    return null;
  }
}

export function writeStoredPetActiveContext(context: PetActiveContext | null) {
  try {
    if (!context) {
      window.localStorage.removeItem(PET_ACTIVE_CONTEXT_STORAGE_KEY);
      return;
    }
    window.localStorage.setItem(PET_ACTIVE_CONTEXT_STORAGE_KEY, JSON.stringify(context));
  } catch {
    // Storage can be unavailable in restricted webviews; the live event still carries context.
  }
}

export function parsePetThinkingState(value: unknown): PetThinkingState | null {
  if (!value || typeof value !== "object") return null;
  const record = value as Record<string, unknown>;
  return {
    conversationId: optionalString(record.conversationId),
    personaId: optionalString(record.personaId),
    source: optionalString(record.source) ?? "desktop",
    thinking: optionalBoolean(record.thinking) ?? false,
    updatedAt: optionalString(record.updatedAt) ?? new Date().toISOString()
  };
}

export function readStoredPetThinkingState(): PetThinkingState | null {
  try {
    const raw = window.localStorage.getItem(PET_THINKING_STATE_STORAGE_KEY);
    if (!raw) return null;
    return parsePetThinkingState(JSON.parse(raw));
  } catch {
    return null;
  }
}

export function writeStoredPetThinkingState(state: PetThinkingState | null) {
  try {
    if (!state) {
      window.localStorage.removeItem(PET_THINKING_STATE_STORAGE_KEY);
      return;
    }
    window.localStorage.setItem(PET_THINKING_STATE_STORAGE_KEY, JSON.stringify(state));
  } catch {
    // The live event path still carries the same state when storage is unavailable.
  }
}

export function publishPetThinkingState(state: PetThinkingState) {
  writeStoredPetThinkingState(state);
  try {
    window.dispatchEvent(new CustomEvent(PET_THINKING_STATE_EVENT, { detail: state }));
  } catch {
    // Best-effort same-window notification.
  }
  try {
    const channel = new BroadcastChannel(PET_THINKING_STATE_CHANNEL);
    channel.postMessage(state);
    channel.close();
  } catch {
    // BroadcastChannel is a cross-window fallback; Tauri events still run.
  }
}

export function subscribePetThinkingState(listener: (state: PetThinkingState) => void) {
  const onWindowEvent = (event: Event) => {
    const state = parsePetThinkingState((event as CustomEvent).detail);
    if (state) listener(state);
  };
  window.addEventListener(PET_THINKING_STATE_EVENT, onWindowEvent);

  let channel: BroadcastChannel | null = null;
  try {
    channel = new BroadcastChannel(PET_THINKING_STATE_CHANNEL);
    channel.onmessage = (event) => {
      const state = parsePetThinkingState(event.data);
      if (state) listener(state);
    };
  } catch {
    channel = null;
  }

  return () => {
    window.removeEventListener(PET_THINKING_STATE_EVENT, onWindowEvent);
    channel?.close();
  };
}
