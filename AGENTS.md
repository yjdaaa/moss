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
- **多会话并行避让**：开工与写共享文件前查 `git status --short` 及运行中的 `cargo` / `node` 进程。存在其他会话未提交改动时：① 避开对方正在改的业务文件，范围重叠先向用户确认；② 写 `CHANGELOG.md`、`frontend/src/styles.css`、`docs/developer/*.md` 等共享汇合文件前，当场重读目标邻近行并只做最小局部编辑，不用整文件覆写；③ 并行期间只对本会话改过的 `.rs` 跑 `rustfmt`（不跑 `cargo fmt --all` 以免改乱对方未完代码）；④ 跑 `npm run build`、`npm run test:e2e`（占 `frontend/dist` 与 9666 端口）、`quick-update-desktop.ps1` 或缓存清理前，先确认无其他会话的构建/测试进程占锁，有则等待锁空后再执行。
- 提交信息格式 `<类型>: <中文结论>`，类型取 `feat`、`fix`、`refactor`、`docs`、`test`、`build`、`chore`；破坏性变更在类型后加 `!`。
- 不写入真实设备地址、凭据、令牌、串口号或本机绝对路径；文档用 `<device-ip>`、`<token>`、`<path>` 占位。

## 安全边界

- 运动、出光、发送或执行 G-code 文件、急停实测、删除、格式化、重启、固件刷写/OTA、上传、网络重配置、部署服务、填写真实密钥：每次先获得用户明确同意。
- 生成和预览不得向硬件发送数据；前端只能调用后端路由。
- 测试中的硬件、网络、GUI、子进程、文件系统副作用一律用 mock、`FakeGrbl` 或临时目录。
- 急停 `0x18` 走实时通道，不得排在 G-code 队列之后；刀路门禁对物理 G-code 文件重新计算 SHA-256。
- 不为全绿放宽断言、伪造预期数据或添加虚假桩代码。
- 新的外部控制或隐私能力（家居、Agent 执行、声纹、固件、联网插件）默认关闭，改由用户自己显式开启（2026-10-14 / 2026-10-15 用户决定）：家居控制须在语音中心“隐私与家居”设置（经来源校验的本机主窗口就地展开；2026-11-07 n67 起不再有独立安全窗口）填好地址、令牌、勾选实体并明确同意，每个动作仍逐次在桌面审批；声纹识别须完成简易标定（仅作候选身份提示，不授权任何动作）；本机 Agent 执行仅在严格模式下开放，每次运行须在桌面审批卡单独批准；固件导入与按台推送、设备唤醒词推送仅在语音中心（经来源校验的本机主窗口）由用户逐台确认（2026-11-07 n67 用户决定取消独立安全窗口）；唯一例外是“副屏预览 › 自动保持全屏画风”（默认关，开启时确认一次）：开启后 MOSS 可给还没装好全屏画风包（含换画风后按新画风重刷）的在线彩屏小智自动推送资源包，沿用原唤醒词，加工中、有待批准请求、对话中不推，每台每天最多一次（2026-11-09 n78 用户决定）。mock 结果不能充当实机验收。

## 开发约束

- 激光 MCP 工具在 `laser_tools::ToolCatalog` 登记，由 `laser_mcp::dispatch_tool_call_with` 分发；启动编排在 `laser-host`。
- 改变契约、默认值、工具注册、提示词或用户可见行为时，同一次改动内同步实现、测试和 `docs/`。
- 用户在对话中确认的设计决定，写入对应技术书的修订记录并注明日期，后续以此为准。

## 验证

命令均从仓库根目录出发，各自独立执行。

| 时机 | 必须执行 |
| --- | --- |
| 每次代码改动（修复或新增功能） | `cargo fmt --all`，`cargo clippy --workspace --all-targets -- -D warnings` 零警告，改前端加 `cd frontend; npm run lint` 零 error（CI 同样把关，2026-11-07）；受影响范围的测试；跨模块、注册、运行时或安全改动跑 `cargo test --workspace`；改前端跑 `cd frontend; npm test` 和 `npm run build`；**验证通过后必须执行 `powershell -File scripts/quick-update-desktop.ps1`（改 `apps/moss-voice-local` 加 `-WithVoiceHelper`，改 `moss-host` 加 `-WithHost`），把最新正式版 `moss-desktop.exe` 编译并原地同步到本机正式程序**（2026-11-01 用户决定；跳过 NSIS 重压包，MOSS 运行时不强杀，自动将旧文件改名挪开并写入新版，用户关掉重开即生效）；macOS 上执行 `bash scripts/quick-update-desktop.sh`（对应参数 `--with-voice-helper` / `--with-host`，原地替换 `/Applications/MOSS.app` 内的程序并重新 ad-hoc 签名，2026-11-05） |
| 改 HTTP 行为、前端或发送链路 | `cd frontend; npm run test:e2e`（API 链路 + 工作台界面，使用回环 GRBL 模拟器；`moss-host` 编译时内嵌 `frontend/dist`，须先 `npm run build` 再构建 release `moss-host`；界面用例需本机 Edge） |
| 改 `apps/moss-voice-local`（独立工作区） | 在该目录 `cargo test`，并在快更时加 `-WithVoiceHelper`；根工作区测试不覆盖它 |
| 阶段收尾、交付 | 以上全部，加三端完整 release 安装包构建（见下） |

三端 release 构建：

- 前端：`cd frontend; npm run build`
- Windows：`cd apps/desktop/src-tauri; cargo tauri build`，产物 `target/release/moss-desktop.exe` 与 `target/release/bundle/nsis/MOSS_<版本>_x64-setup.exe`
- Android：`cd apps/desktop/src-tauri; cargo tauri android build --target aarch64 --apk`，产物在 `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/`

`--debug` 构建只用于临时调试（桌面热重载 `scripts/dev-desktop.ps1`），不算验证证据。日常本机快速同步正式版主程序用 `scripts/quick-update-desktop.ps1`（`cargo tauri build --no-bundle`，跳过 NSIS 压包；阶段收尾仍须跑完整三端构建），细则与带语音 helper 的专用安装包流程见
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

- 代码修复或功能变更：CHANGELOG 已更新、对应验证已通过、**正式版桌面主程序已通过 `scripts/quick-update-desktop.ps1`（macOS 用 `scripts/quick-update-desktop.sh`）同步到最新代码**、文档已同步，才能报告完成；阶段收尾还要求三端 release 构建成功。
- 报告实际运行的检查、跳过项及原因、未验证风险。

## 构建缓存清理

- **3 天自动清理历史编译缓存（必须）**：每次构建／验证收尾或日常维护时，必须执行 `cargo sweep --time 3` 或运行 `powershell -File scripts/clean-build-cache.ps1`（脚本已内置 `cargo sweep --time 3`，可先加 `-WhatIf` 预览），自动清理超过 3 天未使用的历史编译缓存。`Cargo.toml` 的 `[profile.dev]` 保持 `debug = "line-tables-only"` 以精简 `.pdb` 体积。
- `scripts/clean-build-cache.ps1` 会一并清理超过 3 天的编译产物、`incremental`、`target/tmp`、`codex-*`、子目录误生成的 `target/`，以及 `deps` 里本仓库 crate 的旧哈希产物；保留近 3 天活跃的第三方依赖、交付产物、`node_modules`、源码和 `.runtime/`。
