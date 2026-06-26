import type { AgentDefinition, LlmProvider, Persona } from "./types";

export type PersonaAgentBinding = {
  agent: AgentDefinition | null;
  provider: LlmProvider | null;
  providerId: string;
  providerName: string;
  model: string;
  infoText: string;
  searchText: string;
};

function trimmed(value?: string | null) {
  return value?.trim() ?? "";
}

export function resolvePersonaBoundAgent(
  persona: Persona | null | undefined,
  agents: AgentDefinition[],
  fallbackAgentId?: string | null
): AgentDefinition | null {
  const candidates = [trimmed(persona?.agentId), trimmed(fallbackAgentId)].filter(Boolean);
  for (const agentId of candidates) {
    const match = agents.find((agent) => agent.id === agentId);
    if (match) return match;
  }
  return agents.find((agent) => agent.isDefault) ?? agents[0] ?? null;
}

export function resolvePersonaAgentBinding(
  persona: Persona | null | undefined,
  agents: AgentDefinition[],
  llmProviders: LlmProvider[],
  fallbackAgentId?: string | null
): PersonaAgentBinding {
  const agent = resolvePersonaBoundAgent(persona, agents, fallbackAgentId);
  const agentProviderId = trimmed(agent?.llmProvider);
  const personaProviderId = trimmed(persona?.llmProvider);
  const providerId = personaProviderId || agentProviderId;
  const provider = providerId
    ? llmProviders.find((item) => item.id === providerId) ?? null
    : null;
  const providerName = provider?.name?.trim() ?? "";
  const personaModel = trimmed(persona?.llmModel);
  const agentModel = trimmed(agent?.llmModel);
  const model = personaModel || agentModel || trimmed(provider?.model) || "";
  let infoText = "";
  if (providerName || model) {
    infoText = [providerName, model].filter(Boolean).join(" · ");
  } else if (llmProviders.length > 0) {
    infoText = "请选择服务商";
  } else {
    infoText = "未配置服务商";
  }
  const searchText = [
    persona?.name,
    persona?.id,
    agent?.name,
    agent?.id,
    providerName,
    providerId,
    model,
    personaProviderId,
    trimmed(persona?.llmModel)
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return {
    agent,
    provider,
    providerId,
    providerName,
    model,
    infoText,
    searchText
  };
}
