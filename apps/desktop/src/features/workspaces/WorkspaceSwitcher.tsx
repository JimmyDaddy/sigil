import { useRef, useState, type RefObject } from "react";

import type { RecentWorkspaceSummary, WorkspaceSummary } from "../../types";
import { useLocale } from "../../i18n";
import { StatusIndicator } from "../../ui/feedback";
import { Icon } from "../../ui/icons";
import { Button, IconButton, Popover } from "../../ui/primitives";

interface WorkspaceSwitcherProps {
  readonly workspaces: readonly WorkspaceSummary[];
  readonly recentWorkspaces: readonly RecentWorkspaceSummary[];
  readonly activeWorkspaceId?: string;
  readonly busy: boolean;
  readonly onSelect: (workspaceId: string) => void;
  readonly onOpenRecent: (recent: RecentWorkspaceSummary) => void;
  readonly onChoose: () => void;
  readonly onClose: (workspaceId: string) => void;
  readonly triggerRef?: RefObject<HTMLButtonElement | null>;
}

export function WorkspaceSwitcher({
  workspaces,
  recentWorkspaces,
  activeWorkspaceId,
  busy,
  onSelect,
  onOpenRecent,
  onChoose,
  onClose,
  triggerRef,
}: WorkspaceSwitcherProps) {
  const { t } = useLocale();
  const [open, setOpen] = useState(false);
  const localTriggerRef = useRef<HTMLButtonElement>(null);
  const resolvedTriggerRef = triggerRef ?? localTriggerRef;
  const active = workspaces.find((workspace) => workspace.id === activeWorkspaceId);
  const closedRecent = recentWorkspaces.filter((recent) => !recent.isOpen);
  const closeAndRestore = () => {
    setOpen(false);
    window.requestAnimationFrame(() => resolvedTriggerRef.current?.focus());
  };
  const choose = () => {
    closeAndRestore();
    onChoose();
  };
  return (
    <Popover
      className="workspace-switcher"
      align="start"
      label={
        <span className="workspace-switcher-label">
          {active === undefined ? (
            <span className="workspace-switcher-placeholder">{t("workspaces")}</span>
          ) : (
            <StatusIndicator
              label={active.displayName}
              tone={active.state === "ready" ? "success" : "danger"}
            />
          )}
          <span aria-hidden="true">⌄</span>
        </span>
      }
      accessibleLabel={active === undefined ? t("switchWorkspace") : t("switchWorkspaceNamed", { name: active.displayName })}
      open={open}
      onOpenChange={setOpen}
      triggerRef={resolvedTriggerRef}
    >
      <div className="workspace-switcher-content">
        <div className="workspace-switcher-heading">
          <strong>{t("workspaces")}</strong>
          <small>{t("openCount", { count: workspaces.length })}</small>
        </div>
        {workspaces.length === 0 ? (
          <p className="workspace-switcher-empty">{t("noWorkspaceOpen")}</p>
        ) : (
          <ul className="workspace-switcher-list">
            {workspaces.map((workspace) => (
              <li key={workspace.id}>
                <Button
                  className="workspace-switcher-row"
                  type="button"
                  variant="quiet"
                  aria-current={workspace.id === activeWorkspaceId ? "page" : undefined}
                  onClick={() => {
                    onSelect(workspace.id);
                    closeAndRestore();
                  }}
                >
                  <StatusIndicator
                    label={workspace.displayName}
                    tone={workspace.state === "ready" ? "success" : "danger"}
                  />
                </Button>
                <IconButton
                  aria-label={t("closeWorkspace", { name: workspace.displayName })}
                  icon={<Icon name="close" />}
                  onClick={() => {
                    onClose(workspace.id);
                    closeAndRestore();
                  }}
                />
              </li>
            ))}
          </ul>
        )}
        {closedRecent.length > 0 ? (
          <div className="workspace-recent-group">
            <strong>{t("recent")}</strong>
            <ul className="workspace-switcher-list">
              {closedRecent.map((recent) => (
                <li key={recent.id}>
                  <Button
                    className="workspace-switcher-row"
                    type="button"
                    variant="quiet"
                    onClick={() => {
                      onOpenRecent(recent);
                      closeAndRestore();
                    }}
                  >
                    {recent.displayName}
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        ) : null}
        <Button type="button" variant="primary" busy={busy} leadingIcon={<Icon name="add" />} onClick={choose}>
          {t("openWorkspace")}
        </Button>
      </div>
    </Popover>
  );
}
