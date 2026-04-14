# SSA HDRify

<!-- TODO: project description, features, installation, usage -->

## License

Copyright (C) 2021 ying  
Copyright (C) 2024-2025 gky99  
Copyright (C) 2026 LtxPoi

This project is licensed under the [GNU General Public License v3.0 or later](LICENSE).

### Origin and Derivative Work

This is a Tauri desktop rewrite of [ssaHdrify](https://github.com/gky99/ssaHdrify),
originally created by ying (2021) and later maintained by gky99 (2024-2025).
The original project is also licensed under GPL-3.0.

The HDR color conversion algorithm was reimplemented in TypeScript (using
[Color.js](https://colorjs.io/)) based on the approach in the Python version
(which used [colour-science](https://www.colour-science.org/)). No code was
copied verbatim — the implementation is new, but the project is treated as a
derivative work for license purposes.

### Algorithm Attribution

The font collection algorithm in `src/features/font-embed/font-collector.ts`
is inspired by [Aegisub](https://github.com/Aegisub/Aegisub)'s FontCollector
design (BSD-3-Clause). No Aegisub code was copied; the implementation is
original TypeScript written for this project.

### Third-Party Dependencies

All dependencies use licenses compatible with GPL-3.0.

#### Runtime (shipped with the application)

| Component | License | Usage |
|-----------|---------|-------|
| [Tauri](https://tauri.app/) | MIT OR Apache-2.0 | Desktop app framework |
| [React](https://react.dev/) | MIT | UI framework |
| [Color.js](https://colorjs.io/) | MIT | HDR color space conversion (PQ/HLG) |
| [ass-compiler](https://github.com/nicedoc/ass-compiler) | MIT | ASS subtitle parsing for font collection |
| [font-kit](https://github.com/nicedoc/font-kit) | MIT OR Apache-2.0 | Cross-platform system font discovery (Rust) |
| [serde](https://serde.rs/) | MIT OR Apache-2.0 | Rust serialization |

#### Build-time only (not shipped)

| Component | License | Usage |
|-----------|---------|-------|
| [Tailwind CSS](https://tailwindcss.com/) | MIT | CSS utility framework |
| [TypeScript](https://www.typescriptlang.org/) | Apache-2.0 | Type checking |
| [Vite](https://vite.dev/) | MIT | Build tool |
| [ESLint](https://eslint.org/) | MIT | Linting |
