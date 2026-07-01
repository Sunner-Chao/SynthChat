import type { AgentDefinition, LlmProvider, Persona } from "./types";

export type PersonaAgentBinding = {
  agent: AgentDefinition | null;
  provider: LlmProvider | null;
  providerId: string;
  providerName: string;
  model: string;
  infoText: string;
  searchText: string;
  providerDisabled: boolean;
};

function trimmed(value?: string | null) {
  return value?.trim() ?? "";
}

function providerVisible(provider: LlmProvider | null | undefined) {
  return Boolean(provider?.enabled);
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
  const personaProviderId = trimmed(persona?.llmProvider);
  const personaProvider = personaProviderId
    ? llmProviders.find((item) => item.id === personaProviderId) ?? null
    : null;
  const provider = providerVisible(personaProvider) ? personaProvider : null;
  const configuredProvider = personaProviderId ? personaProvider : null;
  const providerId = trimmed(provider?.id);
  const providerName = provider?.name?.trim() ?? "";
  const personaModel = trimmed(persona?.llmModel);
  const model = provider?.id === personaProviderId ? personaModel : "";
  let infoText = "";
  const providerMissing = Boolean(personaProviderId && !configuredProvider);
  const providerDisabled = Boolean(providerMissing || (configuredProvider && !configuredProvider.enabled && !provider));
  if (providerDisabled) {
    infoText = "服务商已停用";
  } else if (providerName || model) {
    infoText = [providerName, model].filter(Boolean).join(" · ");
  } else if (!personaProviderId) {
    infoText = "请选择通讯录服务商";
  } else if (llmProviders.length > 0) {
    infoText = "请选择模型";
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
    providerDisabled ? "" : configuredProvider?.id,
    model,
    providerDisabled ? "" : personaProviderId,
    providerDisabled ? "" : personaModel
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
    searchText,
    providerDisabled
  };
}
