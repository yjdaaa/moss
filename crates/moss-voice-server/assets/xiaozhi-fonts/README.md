# 小智设备屏幕中文字库文件（`noto-v1`）

这些文件原样取自官方 [`78/xiaozhi-fonts`](https://github.com/78/xiaozhi-fonts)（Apache-2.0；字形源为 Google Noto Sans / Noto Sans CJK，SIL Open Font License 1.1）提交 `d45dbc64052d57048f20ab1770074172ce9eb53b`：

- `font_noto_sans_common_14_1.bin`（14px 1bpp，单色屏常用字库）
- `font_noto_sans_common_16_4.bin`（16px 4bpp，小彩屏常用字库）
- `font_noto_sans_common_20_4.bin`（20px 4bpp，320×240 等主流彩屏常用字库）
- `font_noto_sans_common_30_4.bin`（30px 4bpp，大尺寸彩屏常用字库）
- `basic.json`：小智固件内置 `basic` 字库已包含的 Unicode 码点列表（890 个字符），用于 `glyph_push` 跳过设备已有字符。

用途：
1. 生成唤醒词 / 全屏画风资源包（未导入自定义 `base-assets.bin` 或基础包未带 `text_font` 时）自动写入匹配规格的 `common` 字库（6,845 字）与 `text_font_meta`，避免覆盖设备资源分区后丢失常用汉字；
2. 配合按需下载并校验 SHA-256 的服务端第 3 层全量分片包 `noto-v1-full.tar.gz`（`<models>/noto_v1_full/`，简/繁/日/韩/泰/阿 7 套分片 × 4 档字号，共 48,662 字，同源 `78/xiaozhi-fonts`，SIL OFL 1.1），对声明 `features.glyph_push = true` 的 `basic` 或 `common` 在线小智设备，随 `stt` 与 `tts(sentence_start)` 动态下发缺失生僻字与多语种点阵；全量包未就绪或离线时自动回退本目录内置 `common` 字库兜底。

文件按字节原样保存（见仓库 `.gitattributes`），SHA-256 由 `glyph_font` 单测核对；字体许可全文见同目录 `OFL.txt`。
