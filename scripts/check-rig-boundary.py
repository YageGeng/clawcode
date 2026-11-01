#!/usr/bin/env python3
"""Fail when a workspace crate outside the allowlist depends on or names rig directly.

Only crates/protocol, crates/provider and crates/extension may use rig crates;
every other crate goes through protocol::llm or the extension re-exports.
[dev-dependencies] stay unrestricted for tests. Members and dependencies come
from `cargo metadata`, so indentation, table syntax, target tables and
`package = "rig-core"` renames cannot hide a dependency.
"""

import json
import pathlib
import re
import subprocess
import sys

ALLOWED = {"crates/protocol", "crates/provider", "crates/extension"}
# Crate paths as Rust source names them: `use rig_core as rc;` counts too.
RIG_SOURCE = re.compile(r"\brig_(core|reqwest|http)\b")


def manifest_violations(package, root):
    """Returns one line per non-dev rig dependency of a package outside the allowlist."""
    directory = (
        pathlib.Path(package["manifest_path"]).parent.relative_to(root).as_posix()
    )
    if directory in ALLOWED:
        return []
    return [
        f"{directory}/Cargo.toml: {dependency['kind'] or 'normal'} dependency on "
        f"{dependency['name']}"
        + (f" (renamed {dependency['rename']})" if dependency.get("rename") else "")
        for dependency in package["dependencies"]
        if dependency["name"].startswith("rig-") and dependency["kind"] != "dev"
    ]


def source_violations(path, text):
    """Returns one line per source line that names a rig crate."""
    return [
        f"{path}:{number}: {line.strip()}"
        for number, line in enumerate(text.splitlines(), 1)
        if RIG_SOURCE.search(line)
    ]


def source_files(directory):
    """Yields a package's library sources and its build script."""
    yield from sorted((directory / "src").rglob("*.rs"))
    if (directory / "build.rs").is_file():
        yield directory / "build.rs"


def self_test():
    """Asserts every detection rule before the workspace result is trusted."""
    root = pathlib.Path("/w")

    def package(directory, *dependencies):
        return {
            "manifest_path": f"/w/{directory}/Cargo.toml",
            "dependencies": [
                {"name": name, "kind": kind, "rename": rename}
                for name, kind, rename in dependencies
            ],
        }

    for dependency in [
        ("rig-core", None, None),
        ("rig-core", "build", None),
        ("rig-core", None, "rig"),
        ("rig-reqwest", None, None),
    ]:
        assert manifest_violations(package("crates/kernel", dependency), root)
    assert not manifest_violations(package("crates/kernel", ("rig-core", "dev", None)), root)
    assert not manifest_violations(package("crates/provider", ("rig-core", None, None)), root)
    for line in ["use rig_core as rc;", "extern crate rig_core;", "rig_reqwest::shared()"]:
        assert source_violations("f.rs", line), line
    assert not source_violations("f.rs", "use protocol::llm::message::Message;")


def main():
    """Checks every workspace member and prints each violation with its location."""
    self_test()
    try:
        output = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    except subprocess.CalledProcessError as error:
        # Fail closed, but show cargo's own diagnosis instead of a traceback.
        print(f"cargo metadata failed:\n{error.stderr.strip()}", file=sys.stderr)
        return 1
    metadata = json.loads(output)
    root = pathlib.Path(metadata["workspace_root"])
    members = set(metadata["workspace_members"])
    violations = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        violations += manifest_violations(package, root)
        directory = pathlib.Path(package["manifest_path"]).parent
        if directory.relative_to(root).as_posix() in ALLOWED:
            continue
        for path in source_files(directory):
            violations += source_violations(path.relative_to(root), path.read_text())
    if violations:
        print("\n".join(violations))
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
