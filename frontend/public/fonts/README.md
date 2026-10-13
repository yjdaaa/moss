# MOSS bundled fonts

Unmodified Geist and Geist Mono variable webfonts from the official
[vercel/geist-font](https://github.com/vercel/geist-font) repository, pinned to
commit `10dc7658f13c38a474cde201bb09a4617267545b` (retrieved 2026-10-11).
Copyright 2024 The Geist Project Authors. SIL Open Font License 1.1;
the complete license is distributed alongside the fonts in `OFL.txt`.

| File | Upstream path | Bytes | SHA-256 |
| --- | --- | ---: | --- |
| Geist-Variable.woff2 | fonts/Geist/webfonts/Geist[wght].woff2 | 69760 | 2ffebe993e969069a9789d15164b7715d42491b5835516c5e3b935d5f81b05f1 |
| GeistMono-Variable.woff2 | fonts/GeistMono/webfonts/GeistMono[wght].woff2 | 71596 | afaacc4c5fbba89d2ebf7a02dc4070208540874592a5504d57175782fe893101 |
| OFL.txt | OFL.txt | 4383 | c683bfbcc7e087f5d37a54ef628f10387c451a83ddc459b151403a164ac46c90 |

Vite copies this directory into `dist/fonts/`. The HTTP host embeds that same
distribution and Tauri bundles it for Windows and Android. No font CDN is used.
These fonts supply Latin characters; Chinese keeps the existing system fallbacks.
