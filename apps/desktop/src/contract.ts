import type { components, paths } from "./generated/http-schema";

export type HttpServerInfo = components["schemas"]["ServerInfo"];
export type HttpSessionCatalogPage =
  components["schemas"]["SessionCatalogPage"];
export type HttpSessionSnapshot = components["schemas"]["SessionSnapshot"];
export type HttpRunSnapshot = components["schemas"]["RunSnapshot"];
export type HttpRunStartCommand =
  components["schemas"]["RunStartCommand"];
export type HttpRunCancelCommand =
  components["schemas"]["RunCancelCommand"];
export type HttpApprovalDecisionCommand =
  components["schemas"]["ApprovalDecisionCommand"];
export type HttpCatalogOperation =
  paths["/session-catalog"]["get"];
