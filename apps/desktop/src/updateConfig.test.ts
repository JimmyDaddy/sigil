import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

const tauriRoot = resolve(process.cwd(), "src-tauri");

function readJson(path: string): Record<string, unknown> {
  return JSON.parse(readFileSync(resolve(tauriRoot, path), "utf8")) as Record<string, unknown>;
}

describe("signed Desktop updater configuration", () => {
  it("uses the beta HTTPS manifest and the committed Minisign public key", () => {
    const config = readJson("tauri.conf.json");
    const plugins = config.plugins as Record<string, unknown>;
    const updater = plugins.updater as Record<string, unknown>;

    expect(updater.endpoints).toEqual([
      "https://sigil.corerobin.com/updates/beta/latest.json",
    ]);
    expect(new URL((updater.endpoints as string[])[0]).protocol).toBe("https:");
    expect(updater).not.toHaveProperty("dangerousInsecureTransportProtocol");
    expect(updater).not.toHaveProperty("dangerousRemoteDomainIpcAccess");

    const decodedPublicKey = Buffer.from(updater.pubkey as string, "base64").toString("utf8");
    expect(decodedPublicKey).toBe(
      "untrusted comment: minisign public key: 6C04D7096E4FD608\n"
      + "RWQI1k9uCdcEbMTkCZjIBTDNHH7eKm0ly0Xl/Ec1bVh4F7D2N8aU7sb9\n",
    );
  });

  it("generates updater artifacts only for the explicit updater packaging command", () => {
    const baseConfig = readJson("tauri.conf.json");
    const updaterConfig = readJson("tauri.updater.conf.json");

    expect(baseConfig.bundle).not.toHaveProperty("createUpdaterArtifacts");
    expect(updaterConfig.bundle).toMatchObject({ createUpdaterArtifacts: true });
  });

  it("keeps updater and restart authority behind the narrow native wrapper", () => {
    const capability = readJson("capabilities/main.json");
    const permissions = capability.permissions as string[];
    const desktopPermission = readFileSync(resolve(tauriRoot, "permissions/desktop.toml"), "utf8");
    const updatePermission = desktopPermission.match(
      /\[\[permission\]\]\nidentifier = "allow-desktop-update"[\s\S]*?commands\.allow = \[([^\]]+)\]/,
    );

    expect(permissions).toContain("allow-desktop-update");
    expect(permissions.some((permission) => permission.startsWith("updater:"))).toBe(false);
    expect(permissions.some((permission) => permission.startsWith("process:"))).toBe(false);
    expect(updatePermission?.[1].match(/"[^"]+"/g)).toEqual([
      "\"desktop_update_state\"",
      "\"desktop_check_for_update\"",
      "\"desktop_download_and_install_update\"",
      "\"desktop_restart_after_update\"",
    ]);
  });
});
