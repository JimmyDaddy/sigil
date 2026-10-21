import type { ProviderConnection } from "../../types";

type ConnectionIdentity = Pick<ProviderConnection, "label" | "providerLabel">;

export function connectionMatchesProvider(connection: ConnectionIdentity): boolean {
  return connection.label.trim() === connection.providerLabel.trim();
}

export function providerConnectionLabel(connection: ConnectionIdentity): string {
  return connectionMatchesProvider(connection)
    ? connection.label.trim()
    : `${connection.providerLabel.trim()} · ${connection.label.trim()}`;
}

export function providerConnectionDetail(
  connection: ConnectionIdentity & Pick<ProviderConnection, "protocolLabel">,
): string {
  const heading = connection.label.trim();
  return [...new Set([connection.providerLabel.trim(), connection.protocolLabel.trim()])]
    .filter((label) => label !== "" && label !== heading)
    .join(" · ");
}
