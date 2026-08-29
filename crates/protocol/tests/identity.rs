use protocol::{AcpExtensionMethod, ProductIdentity};

/// Product-derived ACP methods share one extension namespace.
#[test]
fn acp_extension_methods_use_the_product_namespace() {
    let methods = [
        AcpExtensionMethod::FollowUp,
        AcpExtensionMethod::Tree,
        AcpExtensionMethod::Navigate,
        AcpExtensionMethod::Branch,
        AcpExtensionMethod::Fork,
        AcpExtensionMethod::Compact,
        AcpExtensionMethod::PendingMessages,
        AcpExtensionMethod::PendingMessageRemove,
        AcpExtensionMethod::ClearQueue,
        AcpExtensionMethod::SessionRename,
        AcpExtensionMethod::SessionRuntime,
        AcpExtensionMethod::TerminalList,
        AcpExtensionMethod::TerminalTerminate,
        AcpExtensionMethod::TerminalClean,
        AcpExtensionMethod::InvokeSkill,
        AcpExtensionMethod::SkillList,
        AcpExtensionMethod::McpStatus,
        AcpExtensionMethod::ExtensionCommand,
        AcpExtensionMethod::UserBash,
    ];

    for method in methods {
        assert!(
            method
                .to_string()
                .starts_with(&format!("_{}", ProductIdentity::ACP_NAMESPACE))
        );
    }
}

/// Product filesystem and environment names are exposed from one identity type.
#[test]
fn product_identity_exposes_runtime_names() {
    assert_eq!(ProductIdentity::NAME, "Clawcode");
    assert_eq!(ProductIdentity::SLUG, "clawcode");
    assert_eq!(ProductIdentity::CONFIG_ENV_PREFIX, "CLAW_");
    assert_eq!(ProductIdentity::CONFIG_PATH_ENV, "CLAW_CONFIG");
    assert_eq!(ProductIdentity::CONFIG_FILE_NAME, "claw.toml");
    assert_eq!(ProductIdentity::CONFIG_DIR_NAME, "clawcode");
    assert_eq!(ProductIdentity::DEFAULT_ACP_PATH, "/acp");
    assert_eq!(ProductIdentity::UI_BOOTSTRAP_PATH, "/api/ui/bootstrap");
}

/// WebUI methods remain discoverable from the shared product namespace.
#[test]
fn webui_extension_methods_are_centralized() {
    assert_eq!(
        AcpExtensionMethod::SessionRename.as_str(),
        format!("_{}/session/rename", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        AcpExtensionMethod::SessionRuntime.as_str(),
        format!("_{}/session/runtime", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        AcpExtensionMethod::McpStatus.as_str(),
        format!("_{}/mcp/status", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        AcpExtensionMethod::UserBash.as_str(),
        format!("_{}/session/bash", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        AcpExtensionMethod::TerminalList.as_str(),
        format!("_{}/terminal/list", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        AcpExtensionMethod::TerminalTerminate.as_str(),
        format!("_{}/terminal/terminate", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        AcpExtensionMethod::TerminalClean.as_str(),
        format!("_{}/terminal/clean", ProductIdentity::ACP_NAMESPACE)
    );
    assert_eq!(
        ProductIdentity::ACP_TERMINAL_UPDATE_NOTIFICATION,
        format!("_{}/terminal/update", ProductIdentity::ACP_NAMESPACE)
    );
}
