export type AcpExtensionMethods = Readonly<{
  tree: string;
  navigate: string;
  branch: string;
  fork: string;
  compact: string;
  pendingMessages: string;
  pendingMessageRemove: string;
  pendingMessagePromote: string;
  clearQueue: string;
  sessionRuntime: string;
  invokeSkill: string;
  skillList: string;
  mcpStatus: string;
  mcpReconnect: string;
  mcpPromptGet: string;
  mcpResourceRead: string;
  mcpComplete: string;
  mcpOAuthContinue: string;
  extensionCommand: string;
  execList: string;
  execTerminate: string;
  execClean: string;
}>;

export const AcpMethods = {
  forNamespace(namespace: string): AcpExtensionMethods {
    const prefix = `_${namespace}`;
    return {
      tree: `${prefix}/session/tree`,
      navigate: `${prefix}/session/navigate`,
      branch: `${prefix}/session/branch`,
      fork: `${prefix}/session/fork`,
      compact: `${prefix}/session/compact`,
      pendingMessages: `${prefix}/session/pending_messages`,
      pendingMessageRemove: `${prefix}/session/pending_message/remove`,
      pendingMessagePromote: `${prefix}/session/pending_message/promote`,
      clearQueue: `${prefix}/session/clear_queue`,
      sessionRuntime: `${prefix}/session/runtime`,
      invokeSkill: `${prefix}/skill/invoke`,
      skillList: `${prefix}/skill/list`,
      mcpStatus: `${prefix}/mcp/status`,
      mcpReconnect: `${prefix}/mcp/reconnect`,
      mcpPromptGet: `${prefix}/mcp/prompt/get`,
      mcpResourceRead: `${prefix}/mcp/resource/read`,
      mcpComplete: `${prefix}/mcp/complete`,
      mcpOAuthContinue: `${prefix}/mcp/oauth/continue`,
      extensionCommand: `${prefix}/extension/command`,
      execList: `${prefix}/session/exec/list`,
      execTerminate: `${prefix}/session/exec/terminate`,
      execClean: `${prefix}/session/exec/clean`
    };
  },
  required(methods: AcpExtensionMethods): readonly string[] {
    return Object.values(methods);
  }
} as const;

export type AcpExtensionNotifications = Readonly<{
  mcpUpdated: string;
}>;

export const AcpNotifications = {
  /** Builds product-scoped extension notification names from one namespace. */
  forNamespace(namespace: string): AcpExtensionNotifications {
    return { mcpUpdated: `_${namespace}/mcp/update` };
  }
} as const;
