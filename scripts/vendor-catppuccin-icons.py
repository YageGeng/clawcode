#!/usr/bin/env python3
"""Bundle the Catppuccin file icons the desktop's file tree uses.

Input is the `extension/` directory of the Catppuccin.catppuccin-vsc-icons
VS Code extension (Open VSX, MIT), unzipped from its .vsix. Only the Mocha
flavor is bundled: the desktop derives Latte, Frappé and Macchiato by
swapping palette colors, which reproduces the shipped icons exactly. Only icons the name and
extension tables reference are kept.

usage: vendor-catppuccin-icons.py EXTENSION_DIR OUTPUT_JSON
"""

import json
import pathlib
import sys


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__, file=sys.stderr)
        return 2
    extension = pathlib.Path(sys.argv[1])
    output = pathlib.Path(sys.argv[2])
    package = json.loads((extension / "package.json").read_text())
    theme = json.loads((extension / "dist/mocha/theme.json").read_text())
    tables = ["fileExtensions", "fileNames", "folderNames", "folderNamesExpanded"]
    defaults = {key: theme[key] for key in ["file", "folder", "folderExpanded"]}
    used = set(defaults.values())
    for table in tables:
        used.update(theme[table].values())
    icons = {}
    for name in sorted(used):
        path = extension / "dist/mocha/icons" / f"{name}.svg"
        if path.exists():
            icons[name] = path.read_text().strip()
    bundle = {
        "source": f"{package['publisher']}.{package['name']} {package['version']}",
        "license": package.get("license", ""),
        **defaults,
        **{table: dict(sorted(theme[table].items())) for table in tables},
        "icons": icons,
    }
    output.write_text(json.dumps(bundle, indent=1, ensure_ascii=False) + "\n")
    print(f"bundled {len(icons)} icons into {output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
