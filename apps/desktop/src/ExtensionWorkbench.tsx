import { useEffect, useMemo, useState } from "react";

import { useLocale } from "./i18n";
import type { AgentCatalogEntry, ExtensionCatalog, SkillCatalogEntry } from "./types";
import { Button, TextField } from "./ui/primitives";

type ExtensionKind = "skills" | "agents";

export function ExtensionWorkbench({
  catalog,
  runActive,
  initialKind = "skills",
  initialQuery = "",
  onUseSkill,
  onUseAgent,
}: {
  catalog: ExtensionCatalog;
  runActive: boolean;
  initialKind?: ExtensionKind;
  initialQuery?: string;
  onUseSkill: (skill: SkillCatalogEntry) => void;
  onUseAgent: (agent: AgentCatalogEntry) => void;
}) {
  const { t } = useLocale();
  const [kind, setKind] = useState<ExtensionKind>(initialKind);
  const [query, setQuery] = useState(initialQuery);
  const items = useMemo(
    () => filterExtensions(kind === "skills" ? catalog.skills : catalog.agents, query),
    [catalog.agents, catalog.skills, kind, query],
  );
  const [selectedId, setSelectedId] = useState<string>();
  useEffect(() => {
    if (!items.some((item) => item.id === selectedId)) setSelectedId(items[0]?.id);
  }, [items, selectedId]);
  const selected = items.find((item) => item.id === selectedId);

  return (
    <div className="extension-workbench">
      <div className="extension-tabs" role="tablist" aria-label={t("extensionKinds")}>
        <Button
          variant={kind === "skills" ? "secondary" : "quiet"}
          role="tab"
          aria-selected={kind === "skills"}
          onClick={() => setKind("skills")}
        >
          {t("skillsCount", { count: catalog.skills.length })}
        </Button>
        <Button
          variant={kind === "agents" ? "secondary" : "quiet"}
          role="tab"
          aria-selected={kind === "agents"}
          onClick={() => setKind("agents")}
        >
          {t("agentsCount", { count: catalog.agents.length })}
        </Button>
      </div>
      <TextField
        label={t("searchExtensions")}
        labelHidden
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        placeholder={kind === "skills" ? t("searchSkills") : t("searchAgents")}
      />
      <div className="extension-workbench-body">
        <div className="extension-list" role="listbox" aria-label={kind === "skills" ? t("skills") : t("agents")}>
          {items.map((item) => (
            <Button
              key={item.id}
              className="extension-row"
              variant="quiet"
              role="option"
              aria-selected={item.id === selectedId}
              onClick={() => setSelectedId(item.id)}
            >
              <span>
                <strong>{item.id}</strong>
                <small>{item.description || t("noDescription")}</small>
              </span>
              <span className={`extension-status${item.available ? " is-available" : ""}`}>
                {item.available ? t("available") : t("unavailable")}
              </span>
            </Button>
          ))}
          {items.length === 0 ? <p className="extension-empty">{t("noExtensionsFound")}</p> : null}
        </div>
        <ExtensionDetail
          kind={kind}
          item={selected}
          runActive={runActive}
          onUseSkill={onUseSkill}
          onUseAgent={onUseAgent}
        />
      </div>
    </div>
  );
}

function ExtensionDetail({
  kind,
  item,
  runActive,
  onUseSkill,
  onUseAgent,
}: {
  kind: ExtensionKind;
  item: SkillCatalogEntry | AgentCatalogEntry | undefined;
  runActive: boolean;
  onUseSkill: (skill: SkillCatalogEntry) => void;
  onUseAgent: (agent: AgentCatalogEntry) => void;
}) {
  const { t } = useLocale();
  if (item === undefined) return <div className="extension-detail extension-empty">{t("selectExtension")}</div>;
  const skill = kind === "skills" ? item as SkillCatalogEntry : undefined;
  const agent = kind === "agents" ? item as AgentCatalogEntry : undefined;
  return (
    <article className="extension-detail">
      <header>
        <span className="extension-token">{item.invocationToken}</span>
        <span className={`extension-status${item.available ? " is-available" : ""}`}>
          {item.available ? t("available") : t("unavailable")}
        </span>
      </header>
      <h3>{skill?.name || item.id}</h3>
      <p>{item.description || t("noDescription")}</p>
      <dl>
        <div><dt>{t("source")}</dt><dd>{item.source}</dd></div>
        <div><dt>{t("trust")}</dt><dd>{item.trust}</dd></div>
        {skill === undefined ? null : <div><dt>{t("execution")}</dt><dd>{skill.runMode}</dd></div>}
        {agent === undefined ? null : <div><dt>{t("agentKind")}</dt><dd>{agent.kind}</dd></div>}
      </dl>
      {item.available ? null : (
        <p className="extension-unavailable-reason">{item.unavailableReason ?? t("extensionUnavailable")}</p>
      )}
      {skill === undefined ? (
        <Button
          type="button"
          variant="primary"
          disabled={agent === undefined || !agent.available || agent.binding === undefined || runActive}
          onClick={() => agent !== undefined && onUseAgent(agent)}
        >
          {runActive ? t("waitForRun") : t("startAgent")}
        </Button>
      ) : (
        <Button
          type="button"
          variant="primary"
          disabled={!skill.available || skill.binding === undefined || runActive}
          onClick={() => onUseSkill(skill)}
        >
          {runActive ? t("waitForRun") : t("useInComposer")}
        </Button>
      )}
    </article>
  );
}

function filterExtensions(
  items: Array<SkillCatalogEntry | AgentCatalogEntry>,
  query: string,
): Array<SkillCatalogEntry | AgentCatalogEntry> {
  const normalized = query.trim().toLocaleLowerCase();
  if (normalized === "") return items;
  return items.filter((item) =>
    `${item.id}\n${item.description}\n${item.source}`.toLocaleLowerCase().includes(normalized),
  );
}
