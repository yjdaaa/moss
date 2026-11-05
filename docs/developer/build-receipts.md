# 交付构建回执

阶段收尾或交付时的构建证据：时间、大小、SHA-256 与核对说明。规则见 [`AGENTS.md`](../../AGENTS.md)。新回执追加在最前面；历史条目内嵌的 hash 仍保留在 `CHANGELOG.md` 原条目中。

## macOS 本机 Apple Silicon 安装包构建（含离线语音辅助程序，2026-11-05）

首次在 macOS 开发机（Apple Silicon，macOS 27.0）上用 `scripts/build-macos-installer.sh`（默认内置 `moss-voice-local` sidecar）构建，基于提交 `793d902`（干净工作树，前端同步重新构建）。为与云端 Mac 版内容一致：构建期间临时移开未纳入 git 的 `native-license-evidence/`、`rust-license-evidence/`（云端没有这两个目录），并设 `CI=true` 跳过 Finder AppleScript 窗口排版（未授予自动化权限时 `bundle_dmg.sh` 会因 AppleEvent 超时失败）。版本号取仓库 `0.3.0`（未注入 `MOSS_RELEASE_VERSION`）；ad-hoc 签名、未公证。

| 本次产物（0.3.0，`aarch64-apple-darwin`） | UTC时间（2026-11-05） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| macOS Apple Silicon 版（含离线语音）`MOSS_0.3.0_aarch64.dmg` | 15:32:06 | 36,489,010 | `cb00ecd86844b09fe7af5ed657528a5fcee93931669941c31d874615da689014` |

核对：`codesign --verify --deep --strict` 通过；`.app` 内含 `moss-desktop`（arm64）与 `moss-voice-local 0.3.0 (protocol 4, GPL-3.0-or-later)`；`LSMinimumSystemVersion=11.0`；`spctl` 按预期拒绝（ad-hoc 未公证，与云端 Mac 版相同）。构建前同一代码 `cargo test --workspace` 934 通过 / 0 失败、前端 198/198、E2E 35/35。

**未验证**：未在本机或其他 Mac 上安装运行；未连接激光雕刻机与小智设备；不含 Intel（x86_64），需要时用 `MOSS_MACOS_TARGET=universal-apple-darwin` 构建通用版。


## GitHub 仓库 `v0.3.1` 云端四端并行交付安装包构建（Windows 离线语音瘦身版 / 轻量版 / 单文件便携版 / macOS 通用版 / Android 签名版，2026-11-05）

通过 `scripts/release-tag.ps1 -Version 0.3.1` 一键推送 `v0.3.1` 标签，触发 `.github/workflows/release.yml`（Run `41994099595`）在 GitHub Actions 云端执行 1 次前端生产编译 + 四端并行构建（注入 `MOSS_RELEASE_VERSION=0.3.1`），分离大型原生静态库归档使带离线语音辅助程序的 Windows 完整版安装包从 `701.9 MB` 瘦身至 `24.72 MB`，并自动计算 `SHA256SUMS.txt` 发布至公开仓库 `yjdaaa/moss` 的 `v0.3.1` Latest Release（含双画风 `160×120` 动态场景 GIF 瞬切与推屏联动、`moss.local` 跨 Wi-Fi 固定 OTA 地址广播及虚拟网卡过滤）。

| 本次产物（0.3.1，GitHub Actions Run `41994099595`） | UTC时间（2026-11-05） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| Windows 完整版（含离线语音）`MOSS_0.3.1_x64-offline-voice-setup.exe` | 08:59:19 | 25,923,235 | `c53dd77e6439393d57a6cce073c4c35b2bf794ac224c1244944c496b1011ce5c` |
| Windows 轻量版 `MOSS_0.3.1_x64-lite-setup.exe` | 08:59:19 | 20,309,175 | `c5fcb825b85ffa4bd5b0727baf23447e712f9ae3a128caa1fbf87df78a7b8f11` |
| Windows 单文件免安装便携版 `MOSS_0.3.1_windows_x64_portable.exe` | 08:59:19 | 65,723,904 | `112247e0d1a9483ca72c17ff599bd8ce82754c892d4c58ec0e70e2b90d781893` |
| macOS 通用版（Apple Silicon + Intel，含离线语音）`MOSS_0.3.1_macos_universal.dmg` | 08:59:19 | 74,765,042 | `2274e24265391ec4423a58295da9d5839eb77babf15f68f2540913e3b83c05f4` |
| Android 签名版 `MOSS_0.3.1_android_aarch64.apk` | 08:59:19 | 54,872,058 | `f1cebfeba5bf66fed2d7fe27b7bd7452102d30719af59b6b935d2d1f0fbab25b` |
| 离线语音辅助程序源码与许可清单 `moss-voice-local-0.3.1-gpl-source.zip` | 08:59:19 | 120,871 | `21fc4fb2a966c9954ed741443b68f9d1e862524175ef59030dd337ac36d9d1a0` |
| 本机快更正式版 `target/release/moss-desktop.exe` | 08:22:50 | 65,589,760 | `04092828365ac1d0ed915e97f65e36cbe15b7d44b4fc002730c5a6cff47e0a9e` |

**未验证**：全新干净 Windows / macOS 机器首次安装、Android 手机实机覆盖安装与立创·实战派 ESP32-S3 及物理激光雕刻机现场联动待实机核验。

## GitHub 仓库 `v0.3.0` 交付安装包构建（Windows 完整离线语音版 / 常规版 / macOS 通用版 / Android 签名版，2026-11-05）

基于 `main` 分支（含立创·实战派 ESP32-S3 `320×240` 1:1 零缩放预览与确认窗续期、实时加工进度副屏 HUD、零 Flash 三张服务端动态场景卡［待电脑审批警戒卡 / 5 秒完工庆祝与急停徽章卡 / 本地音乐律动卡］、硬核太空舱与可爱像素小机器人双画风不断连热切换、5 姿态护目镜小机器人 GIF89a 橱窗、独立「本地知识库」与单开关无感记忆流、锁死安全底座的助手风格预设与 0ms 同音纠偏、左上角全局 MOSS 光学机械眼升级、手机持久配对与固定配对码哈希存储、流式发送 FluidNC `$RI` 实时坐标直通 3D 数字孪生）完成全量测试后，将版本号升级至 `0.3.0`，依次完成带 `moss-voice-local` sidecar 的 Windows 专用 NSIS 构建、常规轻量 Windows NSIS 构建与 Android `aarch64` 签名 APK（`versionCode=3000`）构建，并经用户授权发布至 `yjdaaa/moss` 的 `v0.3.0` Release（macOS 通用版 DMG 由 GitHub Actions `release-macos.yml` 自动构建附加）。

| 本次产物（0.3.0） | UTC时间（2026-11-05） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| 专用 `target/x86_64-pc-windows-msvc/release/moss-voice-local.exe` | 05:07:45 | 20,866,048 | `0a6123aa7114d5e3550418fa45a72b0ad5f30e6b7268333370e1f2695a31d8d7` |
| 专用 `target/x86_64-pc-windows-msvc/release/moss-desktop.exe` | 06:04:05 | 65,454,592 | `5cec442661ce5a54d40b66f8b42a818ec7a2f7024761789c7a8feb1021413768` |
| 专用带离线语音 `target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.3.0_x64-setup.exe` | 06:04:05 | 736,039,327 | `ec0fcdfe4814ba3902c7a4fb62c680f1eafd6b0a316d0904baeadbb498d4bfff` |
| 常规 `target/release/moss-desktop.exe` | 06:07:03 | 65,462,784 | `eacefb9083ebf7a76468c1204753b654351b542a35e2cfabca178a603b56610d` |
| 常规 `target/release/bundle/nsis/MOSS_0.3.0_x64-setup.exe` | 06:07:03 | 20,193,483 | `9dd1ef94ec9bae106342e7259de109f0fa16215a089ee7a3218bce0c265bb5e7` |
| Android `target/aarch64-linux-android/release/libmoss_desktop_lib.so` | 06:08:59 | 48,800,360 | `446751ceaba2eee78a0c1be5a70f09c8b81922b2b4c1d993d42eb23b94774c98` |
| Android `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 06:09:23 | 54,521,502 | `fb4a7ecd2f266d7cdfdecbe472f12990f09c56965bcaba7f34df81eb4a1a298e` |

**未验证**：当前构建宿主为 Windows，macOS `.app`/`.dmg` 由云端 macOS Runner 构建并自动附加到 `v0.3.0` Release；全新干净 Windows 虚拟机安装/卸载、Android 手机实机覆盖安装与连接真实激光雕刻硬件未在本轮执行；专用包仅供用户仓库内部使用（上游静态依赖许可证公开分发审计仍受 §2c 约束）。


## GitHub 仓库 `v0.2.0` 交付安装包构建（Windows 完整离线语音版 / 常规版 / macOS 通用版 / Android 签名版，2026-11-03）

基于 `main` 分支（含内置语音 Jev 云端与本地 `laya_multilingual` 智能信息筛选与续听防抢话、全息语音孪生舱与全局机械眼声波联动、全局激光任务进度浮层、加工完成主动语音播报、小智设备信息与设备端 AEC 模式诊断、声纹按人记忆与 NV19 长期结构化记忆及视觉照片留存打通、低延迟 TTS 预取与本地 TTS 自动降级兜底）完成全量测试后，将版本号升级至 `0.2.0`，依次完成带 `moss-voice-local` sidecar 的 Windows 专用 NSIS 构建、常规轻量 Windows NSIS 构建与 Android `aarch64` 签名 APK（`versionCode=2000`）构建，并经用户授权发布至 `yjdaaa/moss` 的 `v0.2.0` Release（macOS 通用版 DMG 由 GitHub Actions `release-macos.yml` 自动构建附加）。

| 本次产物（0.2.0） | UTC时间（2026-11-03） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| 专用 `target/x86_64-pc-windows-msvc/release/moss-voice-local.exe` | 13:52:10 | 20,866,048 | `8d65898ec19e7b79d859ee8d91c706a2435b06a08699f79b340c6e270f0242b4` |
| 专用 `target/x86_64-pc-windows-msvc/release/moss-desktop.exe` | 14:11:53 | 62,629,376 | `53735b942573a044693ffbde0c7a0aa35781d10fd0768d96fea452a6149b212a` |
| 专用带离线语音 `target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.2.0_x64-setup.exe` | 14:11:53 | 735,483,996 | `865ad8cfe22854dbfc6eeb931a4277d5f7e403dc0d382de1a34ea4edf839ceea` |
| 常规 `target/release/moss-desktop.exe` | 14:18:55 | 62,638,080 | `b308b668bbe6a48a4e8c23988a77c08f8f8efa185b9de0b20c38c465330e2a46` |
| 常规 `target/release/bundle/nsis/MOSS_0.2.0_x64-setup.exe` | 14:18:55 | 19,649,185 | `aec61ab51f0e5495e8ee93b3c09f160150681c9c2542dcc0866fff196816b403` |
| macOS 通用版 `MOSS_0.2.0_macos_universal.dmg`（GitHub Actions `41658582438`） | 15:23:02 | 72,531,497 | `867aae31ef088742f9ec23d74904101a0c7f7903b87f4916be0e3a6087518250` |
| Android `target/aarch64-linux-android/release/libmoss_desktop_lib.so` | 14:26:48 | 48,409,920 | `e8ff6be9bbb3b941770521e64fb1c52668e92cc2ea34eacc2e822160d3fa75fd` |
| Android `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 14:28:59 | 54,131,046 | `9112395f9542efa2a9fb5a70495f1ee550304f98f1e1ece6aac83080069a1fd9` |

**未验证**：当前构建宿主为 Windows，macOS `.app`/`.dmg` 由云端 macOS Runner 构建；全新干净 Windows 虚拟机安装/卸载、Android 手机实机覆盖安装与连接真实激光雕刻硬件未在本轮执行；专用包仅供用户私有仓库内部使用（上游静态依赖许可证公开分发审计仍受 §2c 约束）。
## GitHub 私有仓库 `v0.1.0` 交付安装包构建（Windows 完整离线语音版 / 常规版 / Android 签名版，2026-10-25）

基于 `main` 分支（含 macOS 跨平台运行与打包脚本、Unix 本机 Agent 进程组与 CLI 自动读取、换设备迁移路径降级兜底、Windows 静态内嵌 VC++ 运行库、跨电脑系统字体按字形选字及中英文双语 README）完成全量测试后，重新执行前端生产构建、带 `moss-voice-local` sidecar 的 Windows 专用 NSIS 构建、常规 Windows NSIS 构建与 Android `aarch64` 签名 APK 构建，并经用户授权上传至私有仓库 `yjdaaa/moss` 的 `v0.1.0` Release。

| 本次产物（0.1.0） | UTC时间（2026-10-25） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| 专用 `target/x86_64-pc-windows-msvc/release/moss-voice-local.exe` | 08:58:27 | 20,862,464 | `4799237bad7d9b1589fd8d14aed92772bc8d36777cfe7700cb1c66d423191945` |
| 专用 `target/x86_64-pc-windows-msvc/release/moss-desktop.exe` | 09:21:25 | 61,456,896 | `53d907af148bf13d069fb73ce1a1906c365285d6925f52675e18bf399026be1e` |
| 专用带离线语音 `target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 09:21:25 | 735,182,650 | `e9cd328e13d544f69a1d1b3c7d2b4713b51b2ed93fa14ecb371f31cdc146f412` |
| 常规 `target/release/moss-desktop.exe` | 09:25:27 | 61,465,600 | `71a3882de3785785d93c2d8ee20a0cda8503b5451e7dd5a9bfa392cfd9d65537` |
| 常规 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 09:25:27 | 19,381,014 | `c5bbcbcccf04d337e1d69ca6913a4709b946eb48765b8b611a5feca45a77aca9` |
| Android `target/aarch64-linux-android/release/libmoss_desktop_lib.so` | 09:29:44 | 48,250,072 | `4236c4956342e57d262da0c4ffdda7ba42b62e46d353e64b458b3b54075f53ab` |
| Android `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 09:31:12 | 53,971,214 | `0b29908b59488b3aec17cebe8e566fbc79053289d4e8b1360f963837491a8bd3` |

**未验证**：当前构建宿主为 Windows，macOS `.app`/`.dmg` 未在本机编译；全新干净 Windows 虚拟机安装/卸载、Android 手机实机覆盖安装与连接真实激光雕刻硬件未在本轮执行；专用包仅供用户私有仓库内部使用（上游静态依赖许可证公开分发审计仍受 §2c 约束）。

## 雕刻工作台默认布局改为「左窄右宽」双栏排布并自动对齐左右底边（2026-10-23）

更新 `workbenchLayout.js`、`WorkbenchCanvas.jsx`、`HomePage.jsx`、`styles.css`：① `DEFAULT_LAYOUT` 由三栏改为**左窄右宽双栏**默认排布——左栏（16 列）竖排「通信链路 (`h: 23`)」「加工内容 (`h: 27`)」「材质与参数 (`h: 23`)」，右栏（32 列）上方「加工监视舱 (`h: 64`)」、下方紧贴「加工反馈 (`h: 10`)」，并将 `LAYOUT_VERSION` 升至 `5` 使旧三栏缓存自动切换为新双栏默认布局，彻底消除原独立第三列下半截的大块空位；② `WorkbenchCanvas.jsx` 新增 `alignDualColumns`（配合 `.ob-monitor .chamber-viewport` 最小高度调至 `180px`），在双栏默认列位置且未手动拖拽过 `monitor` 高度时，让 `monitor` 自动随左栏总高度伸缩，保持首屏、切换加工内容模式及折叠/展开左栏卡片时左右两栏底边始终齐平。本次工作树：前端 `npm test` **145 PASS**、`tests/workbench-layout.browser.mjs` **PASS**、`npm run build` 成功、`npm run test:e2e` **32/32 PASS**；Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布。

| 本次产物（0.1.0） | UTC时间（2026-10-23） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 08:19:50 | 61,484,032 | `d69862f164ae4879a651251fbbd91b0b1671b5cc63be3bc41bbbdbbc414b8fae` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 08:19:49 | 19,351,484 | `65398b9d649b962528473c6aac1c35cfa3365f09e8bf9444585976bb349e5710` |

**未验证**：真实桌面窗口不同分辨率下的目视观感与连接物理硬件；本轮无实机操作、外部发布或提交推送。

## `/twin` 3D 工作台上下拖拽放大缩小、材料宽 W / 高 H 滑块与激光头联动、默认关闭循环推演并显式提示、雕刻工作台与创作画板自由画布收起贴合/自适应高度消除卡片空白（2026-10-20）

更新 `twinState.js`、`workbenchLayout.js`、`TwinPage.jsx`、`TwinStage3D.jsx`、`WorkbenchCanvas.jsx`、`DrawPage.jsx`、`styles.css`：① `TwinStage3D.jsx` 底部边框新增 `.twin-stage-resize-handle` 上下拖拽手柄（支持 `220px ~ 960px` 放大/缩小 3D 工作台高度，拖拽过程同步更新 DOM 高度、`renderer.setSize(w, h, false)` 并立即执行 `renderer.render(scene, camera)`，同时在 `h < 520` 时对垂直 `fov` 做平滑补偿，双击恢复默认 `520px`，高度存 `localStorage` 键 `moss_yisu_stage_height_v1`）；② `resolveMachineWorkspace` 放开 `minMatWidthMm / minMatHeightMm` 至 `1mm` 并计算 `toolpathScaleX / toolpathScaleY`，`TwinPage.jsx` 为 `材料宽 W (mm)`、`材料高 H (mm)` 增加拖动滑块（`10 ~ 机台行程`）与草稿态输入缓冲，调节宽高时 3D 材料板、刀路与 5W 激光头实时跟随移动；③ `simLoop` 默认值改为 `false`，按钮 `#btn-twin-loop` 显示 `循环：开启 / 循环：关闭` 并在紧邻右侧新增 `#twin-loop-status-pill` 状态胶囊（`已开启循环雕刻 / 单次推演（不循环）`），未开启循环时推演到达 `100%` 立即停止并不再循环雕刻；④ 底部收起态按钮文字由「展开推演栏」改为「展开」；⑤ 雕刻工作台与创作画板 (`WorkbenchCanvas.jsx` / `DrawPage.jsx` / `styles.css` / `workbenchLayout.js`) 收起卡片后彻底隐藏 `.ob-card-h` 以外的所有内容、锁定 `overflow: hidden` 禁止滚轮下滑、移除纵向缩放把手，并通过 `reflowVertical` 让同列下方卡片自动往上贴合；同时通过 `.wb-card-content` + `ResizeObserver`（`fitCardRows`）让表单类卡片自动贴合内容高度，并将 `.ob-monitor .chamber-viewport` 设为弹性伸展（`flex: 1 1 260px`）、`.job-dispatch-card` 设为 `margin-top: auto`，彻底消除「加工监视舱」`0 / 0 行 PENDING` 下方与「加工反馈」等卡片底部的大片空白框。本次工作树：前端 `npm test` **145 PASS**、`tests/workbench-layout.browser.mjs` **PASS**、`npm run build` 成功、`npm run test:e2e` **32/32 PASS**；Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 13:29、14:54、15:12 与 15:43 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-20） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 16:25:37 | 61,484,032 | `a4b68f37c58327fa27c36a523805e64cdc6f6acef38b46365d0ebfafda325d90` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 16:25:37 | 19,353,892 | `6dbdad21a8d4e66b6d0c6c508267ed1a8fd832ce1dc37e031bf82169e654f23d` |

**未验证**：真实桌面窗口目视拖拽手感与连接物理硬件；本轮无实机操作、外部发布或提交推送。

## 偏好设置新增「隐藏右上角状态胶囊（硬件 / 小智 / 审批）」开关（2026-10-20）

在 `SettingsModal.jsx` 新增「顶栏右上角状态显示」分区与 `#settings-hide-top-pills` 复选框（持久化至 `localStorage` 键 `moss_hide_top_pills`，默认 `false` 不隐藏），由 `main.jsx` 同步至 `Navbar.jsx` 的 `hideStatusPills` 属性；开启时隐藏 `#nav-hardware-status`、`#nav-mcp-status` 与 `#nav-approvals` 三个状态胶囊，常驻 `#nav-estop`（急停 0x18）按钮与 `ApprovalCenter` 新审批自动弹窗保持不变。本次工作树：前端 `npm test` **144 PASS**、`npm run build` 成功、`npm run test:e2e` **32/32 PASS**；Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 13:18 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-20） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 13:29:43 | 61,467,648 | `4d7792aa4ddfa7b403aefaf00c29323e6cb9e060502c35d0f1f55d62a6807367` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 13:29:43 | 19,348,377 | `87391be1ce90941df550df9d059d6f1ecb46aaabf45cc7f79bc63de2ec984380` |

**未验证**：真实桌面窗口目视点击体验与连接物理硬件；本轮无实机操作、外部发布或提交推送。

## `/twin` 数字孪生舱支持激光雕刻文字在材料内任意调整位置并严格钳制不出材料边界、G-code 真实雕刻时长 1:1 实时同步推演、3D 视口浮层与底部推演进度栏可一键收起、同步大鱼 DIY 翼宿 V1.0 真实打印件总成（2026-10-20）

按用户要求与《B站【大鱼DIY】激光雕刻机_翼宿 V1.0 图纸包》全套 STEP/DXF 更新 `twinState.js`、`TwinPage.jsx`、`TwinStage3D.jsx`、`DrawPage.jsx`、`HomePage.jsx`、`styles.css`：① **G-code 雕刻时长精确推算与 1:1 实时同步推演（移除倍速缩放）**：`parseGcodeToolpath` 按每段位移长度与实际有效进给速度 `F`（`mm/min`）推算物理耗时 `durationSeconds`、累计时长 `cumulativeSeconds`、出光雕刻耗时 `cutSeconds` 与空移耗时 `rapidSeconds`；`resolvePoseAtProgress` 改为严格按物理时间 `targetSec = ratio * estimatedSeconds` 在 `cumulativeSeconds` 上二分查找并插值；`TwinPage.jsx` 移除 `0.5x~10x` 倍速按钮与人为时长缩放，`requestAnimationFrame` 回放周期严格 1:1 等于 `toolpath.estimatedSeconds`，实时展示 `已走秒数 / 总计秒数（剩余秒数）` 及 `雕刻耗时 + 空移耗时` 明细；② **激光雕刻文字在材料内位置可调且 100% 钳制不出材料边界**：`resolveMachineWorkspace` 将材料台面起点 (`offsetX, offsetY`) 与文字在材料内的相对偏移 (`textOffsetX, textOffsetY`) 解耦，强制材料外框不小于刀路包络（`materialWidthMm >= spanX`、`materialHeightMm >= spanY`），并将 `textOffsetX / textOffsetY` 严格钳制在材料内；③ **3D 视口浮层与底部刀路推演控制栏均支持一键收起/展开**：`TwinStage3D.jsx` 左上角新增「`收起浮层` / `展开浮层`」切换按钮；`TwinPage.jsx` 底部仿真控制、进度条与 G-code 指令栏（`.twin-timeline-bar`）右上角新增「`收起` / `展开推演栏`」切换按钮（通过 `foldState.js` 按 `twin-timeline` 记忆折叠状态）；④ 默认工作区设为翼宿 V1.0 的 `100×100×20mm`，并按图纸包 1:1 还原全套打印件与 5W 激光头总成。本次工作树：前端 `npm test` **143 PASS**、`npm run build` 成功、`npm run test:e2e` **32/32 PASS**；Windows `cargo tauri build` exit 0。

| 本次产物（0.1.0） | UTC时间（2026-10-20） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 13:18:29 | 61,467,648 | `82dac72b60c878031941ba76aa9423dce64a82a05a4b32e81c6acf532b7b0ec7` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 13:18:29 | 19,349,149 | `10747f778486c5e5295d9ae9da035f261c7d102ab3f08425b4c0a127adc7c3bf` |

**未验证**：桌面真实窗口内翼宿 V1.0 3D 视口目视交互与连接真实 FluidNC 硬件时的周期遥测采样；本轮无实机操作、外部发布或提交推送。
## 切到自建链路允许先切后填（2026-10-19）

修掉 n29 收敛引入的死锁：切换校验不再要求“先有自建接入点”，缺地址也允许切过去，切完在「服务 → 自建接入点」填好保存即重连；地址非空但格式不对仍拒绝。旧文案“服务端设置”统一改为新标签名。本次工作树：`cargo test -p moss-voice` **74 PASS / 1 忽略**（新增空地址可切换的用例）、前端 `npm test` **138 PASS**、`npm run build` 成功、`tests/voice-center.browser.mjs` **PASS**、`npm run test:e2e` **32/32 PASS**。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 01:03 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 17:29:47 | 60,713,984 | `1dafd33a82c9bb2d6b05c878a48e8f4072f1ba4d093af328c470fc2d5841b3b7` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 17:29:47 | 19,078,344 | `e44712acf9391ecffb3f9f4e5a3937a24d866ddace0e6bf83e178d84ef9ad48b` |

**未验证**：真实窗口里“先切后填→自动重连”的完整观感；本轮无实机操作、外部发布或提交推送。

## 语音中心按链路做减法（2026-10-19）

纯前端信息架构调整：非当前链路的卡片不再渲染（删掉 4 个「其他链路的…设置」折叠），标签按链路动态（官方 5 个、自建/内置 6 个），链路的配置集中到「服务」，「连接」改名「设备与工具」，音乐/视觉只在内置链路出现。未改后端命令、ACL 与安全契约。本次工作树：前端 `npm test` **138 PASS**（新增 `voiceTabs.test.mjs`）、`npm run build` 成功、`tests/voice-center.browser.mjs` **PASS**（官方链路 5 标签、内置/自建面板 `count=0`）、`npm run test:e2e` **32/32 PASS**。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 00:29 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 17:03:26 | 60,713,984 | `6bd5800f8e4fc8b708a5405bf225f5719ef6adf477e79cf02cb8b2141351c56d` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 17:03:25 | 19,076,729 | `a7b526d3ca25f6b3de4463b71e1921bdbe33278c531e864f00ffcadb65b2c151` |

**未验证**：真实窗口里逐链路点一遍的观感、触摸/小屏下的标签换行；本轮无实机操作、外部发布或提交推送。

## 工作台：48 列栅格 + 拖动像素跟手（2026-10-19）

用户要求“再细一点”：栅格 24 → **48 列**，并把拖动改成**过程按像素跟手、松手才吸附**（拖动中直接写 `transform` / `width` / `height`，不触发重排；松手按 `Math.round` 吸附到整格再 `settle` 让位）。落点步进约 27–33px（宽）/ 12px（高）。默认布局与最小列宽同步换算（×2），折叠仍为 8 行；布局存储键升到 `moss_wb_layout_v3`（旧 v2 自动失效）。本次工作树：前端 `npm test` **137 PASS**、`npm run build` 成功、`tests/workbench-layout.browser.mjs` **PASS**、`npm run test:e2e` **32/32 PASS**。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 00:12 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 16:29:13 | 60,713,984 | `e3ae1b0f80104010bf30aee8761ad1dafa7c4df550a51a6abaa239e2b021009a` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 16:29:13 | 19,083,477 | `57515ce6af9bba47743f0696b188b1e1180598704ae007277df44beb1d140f00` |

**未验证**：真实窗口里的拖动手感与帧率、触摸屏拖动、多显示器 / 超宽屏表现；本轮无实机操作、外部发布或提交推送。

## 工作台栅格加密：边框步进减半（2026-10-19）

用户反馈“边框步进太大”，把画布栅格从 12 列 / 8px 行加密到 **24 列 / 4px 行**（行距仍 8px）：宽度步进由约 110px 降到约 55px，高度步进由 16px 降到 12px。默认布局、最小尺寸、折叠高度（8 行 ≈ 88px）同步换算；布局存储键升到 `moss_wb_layout_v2`，旧 `v1` 布局因列数变化自动失效、回默认排布。本次工作树：前端 `npm test` **137 PASS**、`npm run build` 成功、`tests/workbench-layout.browser.mjs` **PASS**（默认 `span 32`、折叠 `span 8`、拖右边框改宽度、记忆与还原、锁定），`npm run test:e2e` **32/32 PASS**。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 05:10 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 16:12:33 | 60,713,984 | `d161874fe40a27bd7fc243b83c1d0b84f225f5efcc7894a3430605c23ba2524f` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 16:12:33 | 19,085,618 | `f3ee3230e2dc5044b4ee573fb02c315c6e97be1b5cf7119cbecd62653d8f561c` |

**未验证**：真实窗口里的拖动手感与帧率、触摸屏拖动、多显示器 / 超宽屏表现；本轮无实机操作、外部发布或提交推送。

## 工作台自由画布补齐：折叠收缩与八向边框（2026-10-18）

用户试用反馈后的修补：① 折叠卡片时高度收成标题条（`WbCard` 用 MutationObserver 盯 `FoldHeader` 的 `data-open`，交给画布把行数换成 6 行，展开还原用户拖过的行数）；② 卡片四条边与四个角共 8 个 resize 把手（原来只有右下角一个小热区）；③ 滚动移进卡片内层 `.wb-card-body`，边缘留给把手，避免滚动条盖住右/下边框。本次工作树：前端 `npm test` **137 PASS**、`npm run build` 成功、`tests/workbench-layout.browser.mjs` **PASS**（新增“折叠收成标题条 / 展开恢复”“拖右边框改宽度”两条断言）、`npm run test:e2e` **32/32 PASS**。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布；同日 04:46 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 05:10:26 | 60,713,984 | `5197a4f784f4f53a82c99597e85ebe2db8f01b0fe3ccab020822a8b303a77e29` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 05:10:25 | 19,080,614 | `7d6c39132aad342b55e4eb051869916d1ee5f70756f40887dee4ea27f0c0ccce` |

**未验证**：真实窗口里的拖动手感与帧率、触摸屏拖动、多显示器 / 超宽屏表现；本轮无实机操作、外部发布或提交推送。

## 激光工作台自由画布（2026-10-18）

纯前端布局重构：三栏 grid 换成 12 列自由画布（拖动 / 缩放 / 自动让位 / 布局记忆 / 锁定 / 还原），未改后端与 Tauri 命令。本次工作树：前端 `npm test` **137 PASS**（新增 `workbenchLayout.test.mjs` 7 项）、`npm run build` 成功、新增 `tests/workbench-layout.browser.mjs` **PASS**（Playwright 真拖动：落盘、刷新保持、还原、锁定）、`npm run test:e2e` **32/32 PASS**（用当天已构建的 release `moss-host`，本轮未重建）。Windows `cargo tauri build`（含前端）exit 0；首次尝试因 MOSS 正在运行、`moss-desktop.exe` 被占用而失败（`os error 5`），退出 MOSS 后重建成功。未跑 Android 与带语音 helper 的专用包；未安装、未签名、未发布。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 04:46:07 | 60,710,400 | `2a322cd7a3ad5821df808591df051cf4661ae585e00519ade2c12d497121c3b0` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 04:46:06 | 19,079,031 | `7e87e30b53341d0d2e5332fd201feba45665fa5524c613131a4ba02ecfc1dad9` |

**未验证**：真实窗口里的拖动手感与帧率、触摸屏拖动、多显示器 / 超宽屏表现；本轮无实机操作、外部发布或提交推送。

## 严格模式开关热开热关 n28（2026-10-18）

在 n27 的四项降噪之上，把“关闭严格模式需重启”改成立即生效（`HostApplication.strict_from_env` 改为 `AtomicBool` 并在命令里同步更新）。本次工作树：`cargo test --workspace` **816 PASS / 0 失败 / 9 忽略**，`cargo test -p moss-desktop --lib` **41 通过**；前端 `npm test` **130 PASS**、`npm run build` 成功、`tests/voice-center.browser.mjs` **PASS**、`npm run test:e2e` **32/32 PASS**（用当天已构建的 release `moss-host`，本轮未重建）。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包。未安装、未签名、未发布；同日 03:54 的产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 04:08:31 | 60,701,696 | `0d96cb2c13c535658a31e6d8d0f95b9cba399ef1f8d9a1ce027e44ac709f36b2` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 04:08:30 | 19,072,153 | `8555561b56b38f10768a87a20efe58b838022529e2fbf3e4097d41c8d0c09693` |

**未验证**：真实窗口里热开热关的实际效果与 `.env` 写入、自建链路下拒绝关闭的界面表现、Python 探测在用户机器上的命中率、家居“允许查询”自动同步与真实 Home Assistant、真实设备固件推送；本轮无实机操作、外部发布或提交推送。

## 语音中心降噪：严格模式开关、Python 探测、家居合并、固件移位 n27（2026-10-18）

含两个新 Tauri 命令（`voice_strict_set` 写运行目录 `.env`、`voice_detect_python` 只查文件）。本次工作树：`cargo test --workspace` **816 PASS / 0 失败 / 9 忽略**（新增 `.env` 单行改写测试），`cargo test -p moss-desktop --lib` **41 通过**；前端 `npm test` **130 PASS**、`npm run build` 成功、`tests/voice-center.browser.mjs` **PASS**、`npm run test:e2e` **32/32 PASS**（用当天已构建的 release `moss-host`，本轮未重建）。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包。未安装、未签名、未发布。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 03:54:49 | 60,701,184 | `21799c3cd779dffc7ff965b0d8d49fc053fbab5a8b1acdf67bf01ed5dec5c90e` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 03:54:49 | 19,071,611 | `f8d5c5962f54cb4fecb2068a6f1ef4c8523ec63e38a95347ed92bd59694fa178` |

**未验证**：真实窗口里点严格模式开关后的 `.env` 写入与重启生效、Python 探测在用户机器上的命中率、家居“允许查询”自动同步与真实 Home Assistant、真实设备固件推送；本轮无实机操作、外部发布或提交推送。

## 隐私与家居四项下拉就地展开 n26（2026-10-18）

含权限与原生来源门禁改动（详见 [`voice-private-acl.md`](../integration/voice-private-acl.md) 的 n26 节）。本次工作树：`cargo test --workspace` **815 PASS / 0 失败 / 9 忽略**（含改写后的 ACL 契约测试），`cargo test -p moss-desktop --lib` 40 通过；前端 `npm test` **130 PASS**、`npm run build` 成功、`tests/voice-center.browser.mjs` **PASS**、`npm run test:e2e` **32/32 PASS**（用当天已构建的 release `moss-host`，本轮未重建）。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包。核对：生成的 `gen/schemas/capabilities.json` 已含新 capability `voice-private-settings-main`，`moss-desktop.exe` 内可检索到新增的 `voice-private-home-approvals`。下列为本次生成的普通交付产物，未安装、未签名、未发布；同日 10:44 的旧产物已被本次覆盖。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 03:12:23 | 60,506,624 | `963c72185fc13fea5095bc1921c40af5d7a4de2fc398de27169cf33aedce3ee1` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 03:12:22 | 19,035,645 | `6480968dad9111d4073a3efb66ad05ee5292fc57afb268cbf1ca7a02f5235672` |

真实 Windows WebView2 窗口内的目视点击与家居/设备实机验收未做；本轮无实机操作、外部发布或提交推送。

## 隐私与家居入口改默认收起的下拉（2026-10-18）

纯前端 UI 调整（主语音页入口折叠），未改后端、私密窗口页面与 ACL。本次工作树前端 `npm test` **130 PASS**、`npm run build` 成功、`tests/voice-center.browser.mjs` **PASS**（新增“下拉默认收起、点标题展开”断言）、`npm run test:e2e` **32/32 PASS**（用当天早先构建的 release `moss-host`，本轮未重建）。Windows `cargo tauri build`（含前端）exit 0；未跑 Android 与带语音 helper 的专用包。产物已核对内嵌资源：在 `moss-desktop.exe` 中能检索到 `voice-private-dropdown`，确认新页面已嵌入二进制。下列为本次生成的普通交付产物，未安装、未签名、未发布。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 02:44:47 | 60,433,920 | `5f841c3ffec16f7167f31c0fc6179945a3af89c49dcbeb4d7ebcad5463ca622d` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 02:44:47 | 19,022,802 | `35588c6e9f8f1a2c6b7566e567ed08f729422099517164224da8db94cc1f5bfc` |

真实 Windows WebView2 窗口内的目视确认未做；本轮无实机操作、外部发布或提交推送。

## UI-4 隐私与家居任务入口（2026-10-18）

本次工作树的前端 `npm test`、`npm run build`、私密任务/信息服务/声纹/长期记忆/语音中心浏览器测试通过；`cargo test --workspace` 通过。重新构建 release `moss-host` 后，回环 FakeGrbl E2E **32/32 PASS**。Windows `cargo tauri build`（含前端）及 Android `cargo tauri android build --target aarch64 --apk` 均 exit 0。Android Studio 默认 JBR 25.0.3 使 Gradle `:buildSrc` 配置失败；仅在重试命令进程内设置 `JAVA_HOME` 为本机 JDK 17 后成功，不改全局 Java。下列均为本次生成的普通交付产物，未安装或发布。

| 本次产物（0.1.0） | UTC时间（2026-10-18） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 13:16:52 | 60,429,824 | `b55a8b6c38ad0ccd02fc59c08f1a4c9ed84dce14a8a14782decee368158da163` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 13:16:52 | 19,029,493 | `ead286045bf392e4d7d17f8af2ca056024efdb4b0c742df1f197410b0a153d06` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 13:27:51 | 53,008,066 | `5cf994485a4872e1004b871dc7fc29f8d2e13b48792d74aba046da34344adbcc` |

Android arm64 Rust 库 `target/aarch64-linux-android/release/libmoss_desktop_lib.so` 本次编译 47,286,960 B（13:27:27 UTC），SHA-256 `69d6484ed1e4cfb12e53a6404d9c750b934bc400e80fa9e580423aa831a1083e`；未另行比对 APK 内嵌字节。真实 Windows WebView2 点击、真实 HA/声纹/设备、安装与签名均未验收；普通 NSIS 不代表带 helper 的专用包。本轮无实机操作、外部发布或提交推送。

## NV19 长期记忆、UI-3 语音中心、家居控制与声纹用户开启（2026-10-15）

**本次回执**：同一工作树合并两个会话的改动（NV19-01～08、UI-3 语音中心改版；家居控制与声纹改为用户开启、功能真实性审查修复）。`cargo test --workspace` **804 PASS / 7 ignored**，前端 `npm test` **124 PASS**，`npm run build` 成功，6 个语音浏览器测试通过，release `moss-host` 的 FakeGrbl E2E **32/32 PASS**；`cargo tauri build`（含前端构建）与 `cargo tauri android build --target aarch64 --apk` 均 exit 0。

| 本次产物（0.1.0） | UTC时间（2026-10-15） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 07:38:51 | 59,948,544 | `0807a82493c7a0fa6489a6caf488a493406714bb1da140882b37879cbc571933` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 07:38:51 | 18,931,857 | `37f4b28353f33fafd171569e78d589a437f9b5b1ebc8a69dcc2ace0d3291fe17` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 07:42:46 | 52,870,898 | `02417a0701e3fed311134e0d9acd2dccbf04b4740c12b3d143b48b5d3b802dbc` |

APK 内 `lib/arm64-v8a/libmoss_desktop_lib.so` 与本次 `target/aarch64-linux-android/release/libmoss_desktop_lib.so`（47,149,800 B，07:41:34）SHA 一致：`d45eb4178ae47cb8712188723e2d15d493e3576b5fa0d86a2472f058a6a6745c`。未构建带 helper 的专用内部 NSIS；未签名、未安装、未发布；GUI（HW-15、HW-17、HW-18）与真实设备验收未做。

## NV11 六类供应商契约（2026-10-12）

**NV11 本次最终回执（2026-10-12）**：定向插件 **33/33 PASS**；最终workspace **710 PASS / 7 ignored**（含最新RSS坏尾部和Metaso应用码），前端 **109 PASS**，新release moss-host的FakeGrbl E2E **32 PASS**。实际React假native、前端build、普通Windows release/NSIS、Android aarch64 release APK全部exit0；5份文档/46本地链接与围栏检查、5份Markdown渲染无横向溢出通过。只读复核确认六类TLS夹具经过实际生产adapter、证书验证未关闭、DNS固定本机且无真实账号请求。配置存储故障原子性与真实供应商仍未验收，NV11整体保留partial。

| 本次产物（0.1.0） | UTC时间（2026-10-12） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 15:27:17 | 58,604,544 | `32e0e739f9bd68d282dab354754b8fbc66c19a993b34a7dc18fbedac80dfbf1e` |
| 普通`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 15:27:17 | 18,608,125 | `d7a468f5bde56fbc8f8c4e3f714fbe03711cbe9d24ac62587e428830cb2967e0` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 15:30:07 | 52,601,878 | `c3ba05878048983b2203c917352c8281c7469e605cb9f339235e18a5d310245f` |

三件交付文件及本次arm64 Rust `.so`再次独立读取核对hash；APK内嵌`.so`和`target/aarch64-linux-android/release/libmoss_desktop_lib.so`一致，SHA `c8105b21404dee4bb4ed2ee977d2c75844527f23c0f1a7f848c79c8930c69865`（46,880,784B，15:29:35 UTC）。本轮范围NV11，helper源码/协议及NV18资料未变，**未重新构建NV18专用包，也不以旧专用包代表本次应用代码**；普通NSIS不代表helper发行验收。无实机/真实账号/安装/签名/外部分发/提交推送，未删除既有缓存和用户文件。

## NV13→18 续做（2026-10-12）

**NV13→18 本次最终回执（2026-10-12）**：workspace **698 PASS / 7 ignored**，前端 **109 PASS**，使用本次release moss-host的FakeGrbl E2E **32 PASS**；前端build、普通Windows release/NSIS、Android aarch64 release APK、helper构建检查及专用内部NSIS全部exit0。构建流程中的Node工具测试10项通过，之后新增helper环境白名单测试并重跑最新 **11/11 PASS**，实际helper合成WAV诊断再跑通过；不冒充真人标定。7份文档/32本地链接/围栏检查与7份Markdown渲染无横向溢出通过，3份PowerShell脚本AST检查通过。当前文档入口为融合r13/内置语音n20/实施i3，NV19仅为后续规划。

| 本次产物（版本0.1.0） | UTC时间（2026-10-12） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 12:45:57 | 58,601,984 | `32edbfbca3b6dce9dcd6a5a21d36b0de27b72c837faa3276c49dfbf0ed4420c9` |
| 普通`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 12:45:56 | 18,603,117 | `aa7f68d8cb2d13bd77198b74c1d79fafaed791afbce5184b380e6a6edceb4f84` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 12:48:10 | 52,601,878 | `85904055d55c0bbeec7c241084f89266482bd4f8949e561a9ce778e9cdbdfb52` |
| 专用内部`target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 12:57:31 | 734,429,264 | `e3d2f17ce6223d3ececd7c931c45098ba23bd1526403a93dfc63e9b63be7670c` |

本次独立只读解包核对专用NSIS **875条资源路径**，38份ORT候选归档逐份核对实际字节/hash，证据JSON SHA `f80798fb0746466ca3734c1cf9301dd524ed2302447c665e8c330e0c4b985a49`一致；90份许可证回执只证明收集和字节，不证明实际链接/递归闭环。清单为`target/nv18-690efb906d344d739048a9874fc240db/release-manifest.json`，协议4/schema9，源码树SHA `da26e438672991267d211c38174ad896bb166752c745ec1dfc48af86f7e33b1f`。包内helper20,828,160B/SHA `f8feb516ef7ad28247ddb0eeec4cf502dcce153a62dbb7957b977feeae4ff0e1`与构建检查产物一致；其源码未变、Cargo复用03:59:50 UTC二进制，不声称重新生成字节。包内desktop SHA `a0835b6e645aca55c567fd2601df9a06df8e35fd469ee6be6c64a72b00b0d52e`，仅唯一Tauri bundle标记3字节变化，内存校正后全文件一致。APK内嵌arm64 `.so`与本次编译产物SHA均为`ad2b58be73af74dc5a5ec30517e5eb2ccf283fc3e9ebb48acdb5e498ac27dbbd`。

本次Windows桌面EXE、普通NSIS和专用NSIS Authenticode均 **NotSigned**；SDK36 apksigner只读验证APK **v2签名有效（1个签名者）**，未验发行身份、升级兼容或签名验收。NV13内层沙箱仍blocked；NV15真实HA按用户决定暂缓；NV16真人标定/可信报告开放、NV17设备持钥/生产下载实现、NV18独立原生对应审核/递归许可闭环及安装升级仍partial/unverified。专用包因纳入源码资料增加到734.43MB，仅内部待验，不可外部分发；无实机、私人录音、账号、安装、发布、提交或推送，未清理既有缓存。

## 阶段B 最终交付（2026-10-12）

**阶段B最终交付回执（2026-10-12，当前代码）**：`cargo test --workspace` **698 PASS / 7 ignored**、`npm test` **109 PASS**、新release moss-host的FakeGrbl E2E **32 PASS**，实际React登记/摘要CAS/窄屏通过；前端build、普通Windows、Android、独立helper构建检查和专用内部NSIS全部exit0。8份修改文档/91本地链接与围栏结构检查，9份Markdown渲染无横向溢出；技术书引用已统一r13/n19/i2，历史版本保留。NV8a中间整句停止现有实现及本轮测试已同步，不再把final-only历史当当前行为。

| 产物（版本0.1.0） | UTC时间 | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 2026-10-12 09:57:05 | 58,601,984 | `fefd563d1c4900f8f2d6ce5dccaf95d06a950b1c043e6daf2b8fdf2de289805e` |
| 普通`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 09:57:05 | 18,603,192 | `a3b9142b2705670eafe8c0b00e2f6b855ad1876cc91adc7cfe6066fcb8743300` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 09:59:14 | 52,601,878 | `04e726e322a70149330e209f997ec661bd36e0e0d95fc28f098bf2ae23d8f336` |
| 专用`target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 10:05:08 | 366,144,877 | `044730bf3f37cec94539176ef2059c9fe824fe1db253657925b100a59cfb8f3d` |

APK内arm64 `.so`和本轮Rust产物完整SHA均为`3b11d0e37a075eea031afba9defd799ba51d08fda60235dcc2e0281219240a20`。专用包清单`target/nv18-a11f4d32a8d040e380d5c90495fab4f0/release-manifest.json`：协议4/schema9，source tree SHA `da26e438672991267d211c38174ad896bb166752c745ec1dfc48af86f7e33b1f`。独立7-Zip扫描749条资源路径，实际内存解包helper20,828,160B的完整SHA `f8feb516ef7ad28247ddb0eeec4cf502dcce153a62dbb7957b977feeae4ff0e1`与构建检查产物一致；helper源码本阶段未变，Cargo复用同一二进制（原时间03:59:50 UTC），**不声称重新生成了其字节**。本轮重新使用公开模型+合成PCM实际运行：192维/768B、44ms单次合成推理、错误请求和模型写入/替换拒绝；不代表真人准确率。

专用包desktop58,594,304B与构建产物仅Tauri `__TAURI_BUNDLE_TYPE_VAR_UNK`→`NSS`的3字节差异，在内存仅补该唯一标记后整文件SHA一致，包内SHA `7b7e1c263f03bcda90de2201049e637bb94cf4e51a6fc1a0cf4e82844a2721d2`。原生/Rust证据JSON及两版r-efi的AUTHORS完整字节/hash都与项目文件一致，未执行安装器。严格许可门禁仍拒绝2类阻塞；生产HA、声纹识别/按人模式、可信OTA下载和真实CLI沙箱开放均未通过，无实机动作、真人采集、账号调用、安装或外部发布。未清理既有缓存或无关产物。

## 阶段A 交付（2026-10-12）

**本轮阶段A交付回执（后续代码增量需重新构建）**：workspace686 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS；前端及Windows/Android/独立helper/专用NSIS均exit0。Windows `target/release/moss-desktop.exe`58,232,320B / `f60ffad372f47a7ee6baee5731d1e7e00062f598abcfdb3832a2c891c1c50907`，普通NSIS18,544,601B / `37148466b0e28996077acdc5aee5d8a115f4c4c69323f8adec10812f063535d2`（UTC 2026-10-12 04:32）。Android APK52,596,158B / `b5da0dc08a5bc30aa64a8e962951242b51729bab16f1fd30aa893d61d8af94cf`（04:43 UTC），内嵌arm64 Rust `.so`与当前产物同为 `3f4b68ea91c1cb08415cc4dc5e60669a0fe49bb0f5857096201d0ad3bf4e7f1f`。专用内部NSIS366,098,342B / `1de45451b9effbc4b8676089f01099ed09db2c8ac97203035f3ae572a8f093b5`（04:44 UTC），协议4/schema9清单 `target/nv18-6d11b13bef0a47a2b89e23b2fd3c9ae7/release-manifest.json`。Docker未运行，用官方7-Zip26.04 MSI（SHA `0b01334a418654293513449f61d6bfc99e5196ca371e3c3dc961eda57cd535c6`）只读MSI/CAB提取到任务临时目录，未安装；NSIS独立列748条路径，确认原生/Rust资料和清单，内存解包helper20,828,160B完整SHA `f8feb516ef7ad28247ddb0eeec4cf502dcce153a62dbb7957b977feeae4ff0e1`与构建完全一致。许可证严格门禁仍拒绝2类阻塞。随后新增NV15协调器内存fake+真实临时SQLite接线，7/7专项通过；测试入口仅cfg(test)，生产控制仍硬关闭，后续全量/交付回执单列。
