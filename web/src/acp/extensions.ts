export type AcpExtensionMethods = Readonly<{
  followUp: string;
  tree: string;
  navigate: string;
  branch: string;
  fork: string;
  compact: string;
  pendingMessages: string;
  pendingMessageRemove: string;
  clearQueue: string;
  sessionRename: string;
  invokeSkill: string;
  skillList: string;
  mcpStatus: string;
  extensionCommand: string;
}>;

export const AcpMethods = {
  forNamespace(namespace: string): AcpExtensionMethods {
    const prefix = `_${namespace}`;
    return {
      followUp: `${prefix}/session/follow_up`,
      tree: `${prefix}/session/tree`,
      navigate: `${prefix}/session/navigate`,
      branch: `${prefix}/session/branch`,
      fork: `${prefix}/session/fork`,
      compact: `${prefix}/session/compact`,
      pendingMessages: `${prefix}/session/pending_messages`,
      pendingMessageRemove: `${prefix}/session/pending_message/remove`,
      clearQueue: `${prefix}/session/clear_queue`,
      sessionRename: `${prefix}/session/rename`,
      invokeSkill: `${prefix}/skill/invoke`,
      skillList: `${prefix}/skill/list`,
      mcpStatus: `${prefix}/mcp/status`,
      extensionCommand: `${prefix}/extension/command`
    };
  },
  required(methods: AcpExtensionMethods): readonly string[] {
    return Object.values(methods);
  }
} as const;
