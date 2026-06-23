export const PET_ACTIVE_CONTEXT_EVENT = "synthchat-pet-active-context";
export const PET_ACTIVE_CONTEXT_STORAGE_KEY = "synthchat.pet.activeContext";

export interface PetActiveContext {
  conversationId: string;
  conversationTitle: string | null;
  personaId: string | null;
  personaName: string | null;
  agentId: string | null;
  updatedAt: string;
  source?: string;
}

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value : null;
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
