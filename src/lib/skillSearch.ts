import type { EnhancedSkillSummary, SkillSummary } from "./types";

type SkillSearchable = Pick<SkillSummary, "id" | "name" | "description"> & Partial<Pick<EnhancedSkillSummary, "author" | "version" | "path" | "source">>;

export function filterSkillsByQuery<T extends SkillSearchable>(skills: T[], query: string): T[] {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return skills;
  return skills.filter((skill) =>
    [
      skill.name,
      skill.id,
      skill.description,
      skill.author,
      skill.version,
      skill.path,
      skill.source
    ]
      .filter(Boolean)
      .join(" ")
      .toLowerCase()
      .includes(normalized)
  );
}
