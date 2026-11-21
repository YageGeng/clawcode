//! File icon themes: resolving names to Catppuccin icons and recoloring flavors.

use desktop::file::icons::{FileIconCatalog, Flavor};

#[test]
fn files_resolve_by_name_then_longest_extension() {
    let catalog = FileIconCatalog::builtin();
    assert_eq!(catalog.file("Cargo.toml"), "cargo");
    assert_eq!(catalog.file("lib.rs"), "rust");
    assert_eq!(catalog.file("README.md"), catalog.file("readme.md"));
    // A compound extension wins over its last part.
    assert_eq!(catalog.file("index.d.ts"), "typescript-def");
    assert_eq!(catalog.file("index.ts"), "typescript");
    assert_eq!(catalog.file("no-such.zzz"), "_file");
    assert_eq!(catalog.file("Makefile.unknown-ext"), "_file");
}

#[test]
fn folders_resolve_open_and_closed() {
    let catalog = FileIconCatalog::builtin();
    assert_eq!(catalog.folder("src", false), "folder_src");
    assert_eq!(catalog.folder("SRC", true), "folder_src_open");
    assert_eq!(catalog.folder("whatever", false), "_folder");
    assert_eq!(catalog.folder("whatever", true), "_folder_open");
}

#[test]
fn every_resolved_icon_is_bundled() {
    let catalog = FileIconCatalog::builtin();
    for name in ["Cargo.toml", "lib.rs", "index.d.ts", "x.zzz", "readme.md"] {
        assert!(catalog.svg(catalog.file(name)).is_some(), "{name}");
    }
    for (folder, expanded) in
        [("src", false), ("src", true), ("zz", false), ("zz", true)]
    {
        assert!(
            catalog.svg(catalog.folder(folder, expanded)).is_some(),
            "{folder}"
        );
    }
}

#[test]
fn flavors_recolor_the_mocha_palette() {
    let svg = r##"<path stroke="#fab387" fill="#1E1E2E" color="#123456"/>"##;
    assert_eq!(
        Flavor::Latte.recolor(svg),
        r##"<path stroke="#fe640b" fill="#eff1f5" color="#123456"/>"##
    );
    assert_eq!(
        Flavor::Frappe.recolor(svg),
        r##"<path stroke="#ef9f76" fill="#303446" color="#123456"/>"##
    );
    assert_eq!(
        Flavor::Macchiato.recolor(svg),
        r##"<path stroke="#f5a97f" fill="#24273a" color="#123456"/>"##
    );
    assert_eq!(Flavor::Mocha.recolor(svg), svg);
}
