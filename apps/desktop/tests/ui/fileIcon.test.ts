import { expect, test } from "nsh/test";

import { catppuccinIconBodies, resolveCatppuccinIcon } from "../../src/ui/catppuccinIcons";

test("resolves_exact_filenames_compound_extensions_and_language_icons", () => {
  expect(resolveCatppuccinIcon("Cargo.toml")).toBe("cargo");
  expect(resolveCatppuccinIcon("Cargo.lock")).toBe("cargo-lock");
  expect(resolveCatppuccinIcon("apps/desktop/tsconfig.json")).toBe("typescript-config");
  expect(resolveCatppuccinIcon("Dockerfile")).toBe("docker");
  expect(resolveCatppuccinIcon(".gitignore")).toBe("git");
  expect(resolveCatppuccinIcon("src/main.rs")).toBe("rust");
  expect(resolveCatppuccinIcon("src/App.tsx")).toBe("typescript-react");
  expect(resolveCatppuccinIcon("tests/App.test.tsx")).toBe("typescript-test");
  expect(resolveCatppuccinIcon(".nsh/types/base.d.ts")).toBe("typescript-def");
  expect(resolveCatppuccinIcon("locales/zh-CN.yml")).toBe("yaml");
  expect(resolveCatppuccinIcon("workspace.db")).toBe("database");
  expect(resolveCatppuccinIcon("workspace.db.zip")).toBe("zip");
  expect(resolveCatppuccinIcon("unknown.unregistered-ext")).toBe("_file");
});

test("resolves_specialized_and_fallback_folders_in_closed_and_open_states", () => {
  expect(resolveCatppuccinIcon("src", { kind: "directory" })).toBe("folder_src");
  expect(resolveCatppuccinIcon("src", { kind: "directory", open: true })).toBe("folder_src_open");
  expect(resolveCatppuccinIcon("tests", { kind: "directory" })).toBe("folder_tests");
  expect(resolveCatppuccinIcon("tests", { kind: "directory", open: true })).toBe("folder_tests_open");
  expect(resolveCatppuccinIcon("arbitrary_dir", { kind: "directory" })).toBe("_folder");
  expect(resolveCatppuccinIcon("arbitrary_dir", { kind: "directory", open: true })).toBe("_folder_open");
  expect(resolveCatppuccinIcon("main.rs", { kind: "symlink" })).toBe("symlink");
  expect(resolveCatppuccinIcon("src", { kind: "symlink" })).toBe("symlink");
  for (const key of ["symlink", "_file", "_folder", "_folder_open", "rust", "typescript-react", "folder_src", "folder_src_open"]) {
    expect(key in catppuccinIconBodies()).toBe(true);
  }
});
