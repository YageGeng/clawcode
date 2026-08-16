use std::path::Path;

use protocol::{
    SkillDiagnosticCode, SkillDiagnosticSeverity, SkillListResult,
    SkillSourceKind, SkillSourceScope,
};

/// Skill list payloads preserve source provenance and optional diagnostics at the wire boundary.
#[test]
fn skill_list_round_trips_complete_runtime_metadata() {
    let payload = serde_json::json!({
        "skills": [{
            "name": "review",
            "description": "Review the current change",
            "path": "/repo/.pi/skills/review/SKILL.md",
            "referenceDir": "/repo/.pi/skills/review",
            "source": {
                "kind": "pi",
                "scope": "project",
                "root": "/repo/.pi/skills",
                "originBaseDir": "/repo/.pi",
                "discoveryMode": "pi"
            },
            "disableModelInvocation": true
        }],
        "diagnostics": [{
            "severity": "warning",
            "code": "name_collision",
            "message": "name \"review\" collision",
            "path": "/home/user/.agents/skills/review/SKILL.md",
            "source": {
                "kind": "agents",
                "scope": "user",
                "root": "/home/user/.agents/skills",
                "originBaseDir": "/home/user/.agents",
                "discoveryMode": "agents"
            },
            "collision": {
                "name": "review",
                "winnerPath": "/repo/.pi/skills/review/SKILL.md",
                "loserPath": "/home/user/.agents/skills/review/SKILL.md"
            }
        }]
    });

    let result: SkillListResult = serde_json::from_value(payload.clone())
        .expect("deserialize Skill list");

    assert_eq!(result.skills.len(), 1);
    assert_eq!(result.skills[0].source.kind, SkillSourceKind::Pi);
    assert_eq!(result.skills[0].source.scope, SkillSourceScope::Project);
    assert_eq!(
        result.skills[0].reference_dir,
        Path::new("/repo/.pi/skills/review")
    );
    assert!(result.skills[0].disable_model_invocation);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(
        result.diagnostics[0].severity,
        SkillDiagnosticSeverity::Warning
    );
    assert_eq!(
        result.diagnostics[0].code,
        SkillDiagnosticCode::NameCollision
    );
    assert_eq!(
        serde_json::to_value(result).expect("serialize Skill list"),
        payload
    );
}

/// Optional diagnostic context and manual-only metadata use safe wire defaults when omitted.
#[test]
fn skill_wire_defaults_do_not_require_optional_context() {
    let payload = serde_json::json!({
        "skills": [{
            "name": "review",
            "description": "Review the current change",
            "path": "/repo/review/SKILL.md",
            "referenceDir": "/repo/review",
            "source": {
                "kind": "configured",
                "scope": "configured",
                "root": "/repo/review/SKILL.md",
                "originBaseDir": "/repo",
                "discoveryMode": "pi"
            }
        }],
        "diagnostics": [{
            "severity": "warning",
            "code": "path_not_found",
            "message": "skill path does not exist"
        }]
    });

    let result: SkillListResult =
        serde_json::from_value(payload).expect("deserialize defaulted fields");

    assert!(!result.skills[0].disable_model_invocation);
    assert!(result.diagnostics[0].path.is_none());
    assert!(result.diagnostics[0].source.is_none());
    assert!(result.diagnostics[0].collision.is_none());
}
