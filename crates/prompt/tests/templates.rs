//! Integration tests for pi-compatible Prompt Template behavior.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use prompt::{
    PromptTemplateCatalog, PromptTemplateRoot, PromptTemplateRootRequirement,
};
use protocol::{
    PromptDiagnosticSeverity, PromptSourceScope, PromptTemplateInfo,
};

/// Writes one test Prompt Template and creates its parent directory.
fn write_template(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("template parent"))
        .expect("create template directory");
    fs::write(path, content).expect("write template");
}

/// Discovers one required directory as a user-scoped Prompt Template root.
fn required_catalog(path: &Path) -> PromptTemplateCatalog {
    PromptTemplateCatalog::discover(&[PromptTemplateRoot {
        path: path.to_path_buf(),
        scope: PromptSourceScope::User,
        requirement: PromptTemplateRootRequirement::Required,
    }])
    .expect("discover required templates")
}

/// Returns metadata for one named template from the public catalog projection.
fn template_info(
    templates: &[PromptTemplateInfo],
    name: &str,
) -> PromptTemplateInfo {
    templates
        .iter()
        .find(|template| template.name == name)
        .cloned()
        .expect("template metadata")
}

/// Template expansion implements every positional, aggregate, default, and slice form.
#[test]
fn expansion_supports_pi_placeholder_forms() {
    let root = tempfile::tempdir().expect("template root");
    write_template(
        &root.path().join("review.md"),
        "$1|$2|$@|$ARGUMENTS|${1:-fallback}|${@:-all-default}|${@:2}|${@:2:1}",
    );
    let catalog = required_catalog(root.path());
    let cases = [
        (
            "/review one two",
            "one|two|one two|one two|one|one two|two|two",
        ),
        (
            "/review 'one two' three",
            "one two|three|one two three|one two three|one two|one two three|three|three",
        ),
        ("/review", "||||fallback|all-default||"),
        (
            "/review one two three",
            "one|two|one two three|one two three|one|one two three|two three|two",
        ),
        ("/review '$1'", "$1||$1|$1|$1|$1||"),
    ];

    for (input, expected) in cases {
        assert_eq!(catalog.expand(input), expected, "input: {input}");
    }
}

/// Command arguments follow pi quote and Unicode-whitespace parsing edge cases.
#[test]
fn expansion_parses_pi_command_argument_edges() {
    let root = tempfile::tempdir().expect("template root");
    write_template(&root.path().join("args.md"), "$1|$2|$3|$ARGUMENTS");
    let catalog = required_catalog(root.path());
    let cases = [
        ("/args one\ntwo\u{2003}three", "one|two|three|one two three"),
        ("/args \"\" one", "one|||one"),
        ("/args 'one two", "one two|||one two"),
        ("/args '$1' two", "$1|two||$1 two"),
    ];

    for (input, expected) in cases {
        assert_eq!(catalog.expand(input), expected, "input: {input}");
    }
}

/// Discovery is non-recursive, deterministic, symlink-aware, and first-winner.
#[test]
fn discovery_resolves_sources_and_reports_collisions() {
    let workspace = tempfile::tempdir().expect("workspace");
    let global = workspace.path().join("global");
    let project = workspace.path().join("project");
    write_template(
        &global.join("review.md"),
        "---\ndescription: Review global changes\nargument-hint: \"<path>\"\n---\nReview $1",
    );
    write_template(
        &global.join("crlf.md"),
        "---\r\ndescription: Normalize content\r\n---\r\n  Use $1  \r\n",
    );
    write_template(
        &global.join("invalid.md"),
        "---\ndescription: [\n---\ninvalid",
    );
    write_template(&global.join("ignored.txt"), "ignored");
    symlink(global.join("review.md"), global.join("alias.md"))
        .expect("create file symlink");
    symlink(global.join("missing.md"), global.join("broken.md"))
        .expect("create broken symlink");

    write_template(
        &project.join("review.md"),
        "---\ndescription: Review project changes\n---\nProject $1",
    );
    write_template(
        &project.join("long.md"),
        "123456789012345678901234567890123456789012345678901234567890X\nBody",
    );
    write_template(&project.join("nested/hidden.md"), "Hidden");

    let catalog = PromptTemplateCatalog::discover(&[
        PromptTemplateRoot {
            path: global.clone(),
            scope: PromptSourceScope::User,
            requirement: PromptTemplateRootRequirement::Discovered,
        },
        PromptTemplateRoot {
            path: project.clone(),
            scope: PromptSourceScope::Project,
            requirement: PromptTemplateRootRequirement::Discovered,
        },
    ])
    .expect("discover templates");
    let templates = catalog.templates();
    let names: Vec<_> = templates
        .iter()
        .map(|template| template.name.as_str())
        .collect();

    assert_eq!(names, vec!["alias", "crlf", "review", "long"]);
    assert_eq!(
        template_info(&templates, "review").description,
        "Review global changes"
    );
    assert_eq!(
        template_info(&templates, "review").argument_hint.as_deref(),
        Some("<path>")
    );
    assert_eq!(
        template_info(&templates, "long").description,
        "123456789012345678901234567890123456789012345678901234567890..."
    );
    assert_eq!(catalog.expand("/crlf file.rs"), "Use file.rs");
    assert_eq!(catalog.expand("/hidden"), "/hidden");
    assert_eq!(catalog.expand("/review src/lib.rs"), "Review src/lib.rs");

    let collision = catalog
        .diagnostics()
        .iter()
        .find(|diagnostic| {
            diagnostic.severity == PromptDiagnosticSeverity::Collision
        })
        .expect("collision diagnostic");
    let details = collision.collision.as_ref().expect("collision details");
    assert_eq!(details.name, "review");
    assert_eq!(details.winner.scope, PromptSourceScope::User);
    assert_eq!(details.loser.scope, PromptSourceScope::Project);
    assert!(catalog.diagnostics().iter().any(|diagnostic| {
        diagnostic.severity == PromptDiagnosticSeverity::Warning
            && diagnostic
                .source
                .path
                .as_ref()
                .is_some_and(|path| path.ends_with("invalid.md"))
    }));
}

/// A required configured root fails instead of silently degrading discovery.
#[test]
fn discovery_rejects_missing_required_roots() {
    let workspace = tempfile::tempdir().expect("workspace");
    let missing = workspace.path().join("missing");

    let result = PromptTemplateCatalog::discover(&[PromptTemplateRoot {
        path: missing.clone(),
        scope: PromptSourceScope::Config,
        requirement: PromptTemplateRootRequirement::Required,
    }]);

    let error = result.expect_err("required root must fail").to_string();
    assert!(error.contains(missing.to_string_lossy().as_ref()));
}

/// Missing automatic directories are normal absence and do not create warnings.
#[test]
fn discovery_ignores_missing_discovered_roots() {
    let workspace = tempfile::tempdir().expect("workspace");
    let catalog = PromptTemplateCatalog::discover(&[PromptTemplateRoot {
        path: workspace.path().join("not-created"),
        scope: PromptSourceScope::User,
        requirement: PromptTemplateRootRequirement::Discovered,
    }])
    .expect("ignore absent discovered root");

    assert!(catalog.templates().is_empty());
    assert!(catalog.diagnostics().is_empty());
}
