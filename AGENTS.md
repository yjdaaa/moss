# Project Instructions

仓库级唯一规则源，供 Antigravity (agy)、Codex、Claude Code、Cursor 等工具共用。实现约定见
[`docs/developer/standards.md`](docs/developer/standards.md)，高风险代码先读
[`docs/developer/safety.md`](docs/developer/safety.md)。其他文档只引用本文件，不复制规则。

## 项目

MOSS 是小智 AI 生态中枢与激光数控雕刻上位机（Rust 重构版），面向 FluidNC/GRBL 与 ESP32 下位机。

- 激光主链路：`crates/laser-*`（contracts、core、device、runtime、tools、mcp、api、host）。
- 内置语音：`crates/moss-voice*`、`moss-xiaozhi-proto`、`moss-mcp-client`；本地模型 helper `apps/moss-voice-local` 是 GPL 独立工作区。
- 宿主：`apps/desktop`（Tauri 2 桌面/Android）、`apps/moss-host`（HTTP/WS）；前端 `frontend/`（React + Vite）。
- 文档入口 [`docs/README.md`](docs/README.md)；实施进度只看 [实施 TODO](docs/integration/moss-fusion-implementation-todo.md)。结构以代码为准。

## 工作方式

- 先读待改文件、直接依赖、相关测试和对应文档；不扫描无关目录。
- 复用现有接口、注册方式、命名和错误处理，不为整齐新增抽象。
- 未经明确授权，不提交、推送、重置或覆盖他人改动。
- 提交信息格式 `<类型>: <中文结论>`，类型取 `feat`、`fix`、`refactor`、`docs`、`test`、`build`、`chore`；破坏性变更在类型后加 `!`。
- 不写入真实设备地址、凭据、令牌、串口号或本机绝对路径；文档用 `<device-ip>`、`<token>`、`<path>` 占位。

## 安全边界

- 运动、出光、发送或执行 G-code 文件、急停实测、删除、格式化、重启、固件刷写/OTA、上传、网络重配置、部署服务、填写真实密钥：每次先获得用户明确同意。
- 生成和预览不得向硬件发送数据；前端只能调用后端路由。
- 测试中的硬件、网络、GUI、子进程、文件系统副作用一律用 mock、`FakeGrbl` 或临时目录。
- 急停 `0x18` 走实时通道，不得排在 G-code 队列之后；刀路门禁对物理 G-code 文件重新计算 SHA-256。
- 不为全绿放宽断言、伪造预期数据或添加虚假桩代码。
- 新的外部控制或隐私能力（家居、Agent 执行、声纹、固件、联网插件）默认关闭，改由用户自己显式开启（2026-10-14 / 2026-10-15 用户决定）：家居控制须在私密窗口填好地址、令牌、勾选实体并明确同意，每个动作仍逐次在私密窗口批准；声纹识别须完成简易标定（仅作候选身份提示，不授权任何动作）；本机 Agent 执行仅在严格模式下开放，每次运行须在桌面审批卡单独批准；固件导入与按台推送仅在私密窗口由用户确认。mock 结果不能充当实机验收。

## 开发约束

- 激光 MCP 工具在 `laser_tools::ToolCatalog` 登记，由 `laser_mcp::dispatch_tool_call_with` 分发；启动编排在 `laser-host`。
- 改变契约、默认值、工具注册、提示词或用户可见行为时，同一次改动内同步实现、测试和 `docs/`。
- 用户在对话中确认的设计决定，写入对应技术书的修订记录并注明日期，后续以此为准。

## 验证

命令均从仓库根目录出发，各自独立执行。

| 时机 | 必须执行 |
| --- | --- |
| 每次代码改动 | 受影响范围的测试；跨模块、注册、运行时或安全改动跑 `cargo test --workspace`；改前端跑 `cd frontend; npm test` 和 `npm run build` |
| 改 HTTP 行为、前端或发送链路 | `cd frontend; npm run test:e2e`（需先构建 `moss-host`，使用回环 GRBL 模拟器） |
| 改 `apps/moss-voice-local`（独立工作区） | 在该目录 `cargo test`；根工作区测试不覆盖它 |
| 阶段收尾、交付 | 以上全部，加三端 release 构建（见下） |

三端 release 构建：

- 前端：`cd frontend; npm run build`
- Windows：`cd apps/desktop/src-tauri; cargo tauri build`，产物 `target/release/moss-desktop.exe` 与 `target/release/bundle/nsis/MOSS_<版本>_x64-setup.exe`
- Android：`cd apps/desktop/src-tauri; cargo tauri android build --target aarch64 --apk`，产物在 `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/`

`--debug` 构建只用于临时调试，不算验证证据。带语音 helper 的专用安装包流程见
[`packaging-install.md`](docs/developer/packaging-install.md)。

## 结果状态

| 状态 | 含义 |
| --- | --- |
| `PASS` | 要求的测试与构建在本次改动后实际通过，且不依赖未做的实机验证 |
| `partial` | 部分检查通过；有构建失败、环境阻塞或未执行项（写明原因） |
| `unverified` | 软件检查通过，但未连接硬件、未实机出光、未用真实账号或设备 |

不得用旧产物充当本次结果。构建成功不等于实机通过；安装、发布、设备操作另需授权。

## 变更记录

每个代码修复或功能变更在 `CHANGELOG.md` 的 `[Unreleased]` 下加一条（文档-only、测试-only 不需要）。标题直接写结论，正文四项，各一两句：

- **现象**：触发条件与影响。
- **根因与修复**：原因、关键改动；行为变更另写默认值、契约、安全门禁、跨端影响，破坏性变更显式标注。
- **验证**：测试名或数量、未验证项。
- **关联**：主要文件与符号。

交付构建的时间、大小、SHA-256 写入 [`docs/developer/build-receipts.md`](docs/developer/build-receipts.md)，不写进 CHANGELOG。

## 完成标准

- 代码修复或功能变更：CHANGELOG 已更新、对应验证已通过、文档已同步，才能报告完成；阶段收尾还要求三端 release 构建成功。
- 报告实际运行的检查、跳过项及原因、未验证风险。

## 构建缓存清理

- **3 天自动清理历史编译缓存（必须）**：每次构建／验证收尾或日常维护时，必须执行 `cargo sweep --time 3` 或运行 `powershell -File scripts/clean-build-cache.ps1`（脚本已内置 `cargo sweep --time 3`，可先加 `-WhatIf` 预览），自动清理超过 3 天未使用的历史编译缓存。`Cargo.toml` 的 `[profile.dev]` 保持 `debug = "line-tables-only"` 以精简 `.pdb` 体积。
- `scripts/clean-build-cache.ps1` 会一并清理超过 3 天的编译产物、`incremental`、`target/tmp`、`codex-*`、子目录误生成的 `target/`，以及 `deps` 里本仓库 crate 的旧哈希产物；保留近 3 天活跃的第三方依赖、交付产物、`node_modules`、源码和 `.runtime/`。
