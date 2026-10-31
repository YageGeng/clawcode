# Third-Party Notices

This repository depends on `rig-core` and `rig-reqwest` from `0xPlaygrounds/rig`:

- Source: https://github.com/0xPlaygrounds/rig
- License: MIT
- Copyright: Copyright (c) 2024, Playgrounds Analytics Inc.

The upstream crates come from crates.io with the range `>=0.46.0-rc.1, <1`.
`Cargo.lock` records the currently resolved releases and checksums.
The former vendored implementation under `crates/provider/` has been removed;
that crate now contains project configuration and authentication assembly.
This attribution is retained for the project's prior derived implementation and
any remaining adaptation. Upstream license texts are distributed with the dependency.

<details>
<summary>Rig MIT license</summary>

```text
Copyright (c) 2024, Playgrounds Analytics Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```

</details>

## Agent WebUI direct dependencies

The browser application directly depends on the following packages. Versions
are pinned in `web/package.json` and their full license texts are distributed
by their respective package archives.

| Package | Version | License |
|---|---:|---|
| highlight.js | 11.11.2 | BSD-3-Clause |
| lucide-react | 1.31.0 | ISC |
| react | 19.2.8 | MIT |
| react-dom | 19.2.8 | MIT |
| react-markdown | 10.1.0 | MIT |
| rehype-highlight | 7.0.2 | MIT |
| remark-gfm | 4.0.1 | MIT |
| zustand | 5.0.15 | MIT |
| @eslint/js | 10.0.1 | MIT |
| @types/node | 26.2.0 | MIT |
| @types/react | 19.2.18 | MIT |
| @types/react-dom | 19.2.4 | MIT |
| @vitejs/plugin-react-swc | 4.3.3 | MIT |
| eslint | 10.8.1 | MIT |
| eslint-plugin-react-hooks | 7.1.1 | MIT |
| eslint-plugin-react-refresh | 0.5.4 | MIT |
| globals | 17.11.0 | MIT |
| typescript | 6.0.3 | Apache-2.0 |
| typescript-eslint | 8.67.0 | MIT |
| vite | 8.2.1 | MIT |
