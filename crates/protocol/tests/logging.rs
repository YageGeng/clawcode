//! Shared logging vocabulary: ACP request classification and usage text.

use protocol::AcpExtensionMethod as M;
use protocol::usage::{ModelUsage, Usage};

/// Read-only snapshots are the only extension methods classified as queries.
#[test]
fn query_methods_are_exactly_the_read_only_snapshots() {
    let queries = [
        M::Tree,
        M::PendingMessages,
        M::SessionRuntime,
        M::SkillList,
        M::McpStatus,
        M::McpPromptGet,
        M::McpResourceRead,
        M::McpComplete,
    ];
    let changes = [
        M::Navigate,
        M::Branch,
        M::Fork,
        M::Compact,
        M::PendingMessageRemove,
        M::PendingMessagePromote,
        M::ClearQueue,
        M::InvokeSkill,
        M::McpReconnect,
        M::McpOAuthContinue,
        M::ExtensionCommand,
        M::UserBash,
    ];
    assert!(queries.iter().all(|method| method.is_query()));
    assert!(changes.iter().all(|method| !method.is_query()));
}

/// Usage text names only the counters the provider reported, keeping reported zeros.
#[test]
fn usage_text_lists_only_reported_counts() {
    let mut usage = Usage::default();
    usage.input_tokens = Some(10);
    usage.output_tokens = Some(0);
    usage.reasoning_tokens = Some(3);
    assert_eq!(
        ModelUsage::from(usage).to_string(),
        "input 10, output 0, reasoning 3"
    );
}

/// Usage without any reported counter says so instead of printing zeros.
#[test]
fn usage_text_without_counts_says_so() {
    assert_eq!(ModelUsage::default().to_string(), "no usage reported");
}
