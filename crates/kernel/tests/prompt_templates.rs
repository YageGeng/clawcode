use std::fs;

use kernel::PromptTemplateCatalog;

#[test]
fn discovers_and_expands_pi_prompt_template_arguments() {
    let root = tempfile::tempdir().expect("temp directory");
    fs::write(
        root.path().join("review.md"),
        "---\ndescription: Review selected files\n---\nReview $1 with $ARGUMENTS and ${@:2:2}.",
    )
    .expect("write prompt template");

    let catalog = PromptTemplateCatalog::discover(&[root.path().to_path_buf()])
        .expect("discover prompt templates");
    let expanded = catalog
        .expand("/review 'src/main.rs' fast safe")
        .expect("expand prompt template");

    assert_eq!(
        expanded,
        "Review src/main.rs with src/main.rs fast safe and fast safe."
    );
}

#[test]
fn ignores_nested_and_non_markdown_prompt_files() {
    let root = tempfile::tempdir().expect("temp directory");
    fs::create_dir(root.path().join("nested"))
        .expect("create nested directory");
    fs::write(root.path().join("nested/hidden.md"), "hidden")
        .expect("write nested template");
    fs::write(root.path().join("ignored.txt"), "ignored")
        .expect("write ignored file");

    let catalog = PromptTemplateCatalog::discover(&[root.path().to_path_buf()])
        .expect("discover prompt templates");

    assert_eq!(catalog.expand("/hidden value"), None);
}
