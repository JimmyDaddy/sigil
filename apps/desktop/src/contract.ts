import type { components, paths } from "./generated/http-schema";

export type HttpServerInfo = components["schemas"]["ServerInfo"];
export type HttpSessionCatalogPage =
  components["schemas"]["SessionCatalogPage"];
export type HttpSessionSnapshot = components["schemas"]["SessionSnapshot"];
export type HttpSessionContinuityView =
  components["schemas"]["SessionContinuityView"];
export type HttpSessionTranscriptPage =
  components["schemas"]["SessionTranscriptPage"];
export type HttpConversationDisplayPage =
  components["schemas"]["ConversationDisplayPage"];
export type HttpRunSnapshot = components["schemas"]["RunSnapshot"];
export type HttpRunStartCommand =
  components["schemas"]["RunStartCommand"];
export type HttpRunCancelCommand =
  components["schemas"]["RunCancelCommand"];
export type HttpApprovalDecisionCommand =
  components["schemas"]["ApprovalDecisionCommand"];
export type HttpCatalogOperation =
  paths["/session-catalog"]["get"];
