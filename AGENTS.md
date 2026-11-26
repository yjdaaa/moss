# Project Instructions

仓库级唯一规则源，供 Antigravity (agy)、Codex、Claude Code、Cursor 等工具共用，其他文档只引用本文件。这里只写从代码里看不出来的约定；能由脚本保证的规则交给脚本（`scripts/verify.mjs`、`scripts/check-*.mjs`、`.githooks/`、CI），不在这里重复。

- 增删改 MCP 工具、Tauri 命令、数据库迁移或前端弹窗前，先读 [`standards.md`](docs/developer/standards.md)。
- 改 `laser-device`、`laser-api`（发送门禁 / 审批 / CORS）、`laser-host`、`moss-voice-server`、`moss-mcp-client` 或 `apps/desktop/src-tauri` 前，先读 [`safety.md`](docs/developer/safety.md)。这些代码直接驱动激光机，出错会伤人或损坏设备。

## 项目

MOSS 是小智 AI 生态中枢与激光数控雕刻上位机（Rust），面向 FluidNC/GRBL 与 ESP32 下位机。激光主链路在 `crates/laser-*`，内置语音在 `crates/moss-voice*` 等，宿主是 `apps/desktop`（Tauri 2）与 `apps/moss-host`，前端在 `frontend/`（React + Vite）。`apps/moss-voice-local` 是 GPL 独立工作区，根工作区命令不覆盖它。文档入口 [`docs/README.md`](docs/README.md)，实施进度看 [实施 TODO](docs/integration/moss-fusion-implementation-todo.md)，结构以代码为准。

## 工作方式

- 先读待改文件、直接依赖、相关测试和对应文档，不扫描无关目录；复用现有接口、注册方式、命名和错误处理，不为整齐新增抽象。
- 不推送、重置或覆盖他人改动。一组改动通过收尾级验证后可以本地提交（2026-11-23 用户授权），只 `git add` 本会话改过的文件；推送仍需用户明确同意。提交信息格式由 `commit-msg` 钩子检查（`<类型>: <中文结论>`）。
- 多会话并行：开工前看 `git status --short`。别的会话有未提交改动时，避开对方正在改的业务文件，范围重叠先问用户；写 `CHANGELOG.md`、`frontend/src/styles/` 分片、`docs/developer/*.md` 等共享文件前当场重读邻近行、只做局部编辑；只对本会话改过的 `.rs` 跑 `rustfmt`（`--fmt-write` 已是这样）。`verify.mjs` 会自动检测并发的构建 / 测试进程并拒绝开跑，等对方结束再试，不要用 `--force` 硬闯。
- 文档与代码里不写真实设备地址、凭据、令牌、串口号或本机绝对路径，用 `<device-ip>`、`<token>`、`<path>` 占位（`check-repo-hygiene.mjs` 会扫）。

## 安全边界

这些需要判断，脚本替代不了，所以写在这里：

- 运动、出光、发送或执行 G-code 文件、急停实测、删除、格式化、重启、固件刷写 / OTA、上传、网络重配置、部署服务、填写真实密钥：每次先获得用户同意，上一次的同意不延续到下一次。
- 生成和预览不向硬件发送数据；前端只调用后端路由。
- 测试里的硬件、网络、GUI、子进程、文件系统副作用用 mock、`FakeGrbl` 或临时目录。
- 急停 `0x18` 走实时通道，不排在 G-code 队列之后；刀路门禁对物理 G-code 文件重新计算 SHA-256。
- 不为全绿放宽断言、伪造预期数据或添加虚假桩代码。mock 结果不能充当实机验收。
- 语音对话可能在用户话没说完时就抢跑（2026-11-13 n86），所以新增由大模型触发、有副作用的执行路径要排在 `TurnGate`（`Turn::wait_release`）之后；只有整句停止这类必须立即执行的快路径例外。
- 外部控制与隐私能力（家居、本机 Agent 执行、声纹、固件与唤醒词推送、联网插件）默认关闭，由用户在经来源校验的桌面主窗口开启；家居动作与严格模式下的本机 Agent 每次运行都要桌面审批卡逐次批准；声纹只作候选身份提示，不授权任何动作；固件与唤醒词按台确认推送。各模块门禁与唯一的自动推送例外见 [`safety.md`](docs/developer/safety.md) §4。

## 开发约束

- 激光 MCP 工具在 `laser_tools::ToolCatalog` 登记，由 `laser_mcp::dispatch_tool_call_with` 分发；启动编排在 `laser-host`。
- 行程、轴数、回零 / 软限位、急停引脚、`speed_map`、运动参数等机器能力，以 `laser-device` 从 FluidNC / GRBL 读回的字段为准（握手 `probe.rs`、解析 `protocol.rs`、发送前实时读 `connection.rs`），读不到按“未知”处理，不猜。界面行程按“手填 → 固件 → 默认 100 mm”取值，发送前仍按当场读到的固件行程复核（2026-11-26 用户确认，融合技术书 r65 / r68）。
- crate 依赖只向下，激光域与语音域互不依赖（[`architecture.md`](docs/developer/architecture.md) §2）；新增 crate 先在 `scripts/check-crate-layers.mjs` 登记。脚本与文档按用途命名，任务代号只写正文。
- 改契约、默认值、工具注册、提示词或用户可见行为时，同一次改动里同步实现、测试和 `docs/`。
- 用户在对话中确认的设计决定，写入对应技术书的修订记录（[`docs/integration/revisions/`](docs/integration/revisions/)）并注明日期，后续以此为准。

## 验证

统一入口 `node scripts/verify.mjs`，按 git 改动自动决定跑什么。分三级，重的检查只在收尾跑一次（2026-11-23 用户确认）：

| 级别 | 时机 | 命令 |
| --- | --- | --- |
| 迭代 | 任务中途每改完一处 | `node scripts/verify.mjs --affected --fmt-write`：静态门禁、格式化改动 `.rs`、改动 crate 的 clippy 与单测、前端 lint 与单测。要更快用 `--quick --fmt-write`（不跑测试） |
| 收尾 | 一组改动全部完成、报告完成前，跑一次 | `node scripts/verify.mjs --finish`：全工作区 clippy / test、前端 lint / test / build，按改动路径自动追加浏览器测试与 E2E，通过后自动快更本机桌面程序 |
| 交付 | 阶段收尾、发版 | `node scripts/verify.mjs --full --e2e`，本机构建 macOS 安装包；Windows / Android 安装包以 CI（`ci.yml` / `release.yml`）结果为准。命令与产物见 [`packaging-install.md`](docs/developer/packaging-install.md) §0 |

- `--finish` 的自动规则在 `planFinish`（`scripts/verify.mjs`）：改 `frontend/src` 追加浏览器测试；改 `crates/laser-*`、`apps/moss-host`、前端 API / 急停 / 发送相关模块追加 E2E；改运行时代码才快更（文档、测试、脚本不快更），按路径自动带 `--with-host` / `--with-voice-helper`。你判断还需要更多时，手动加 `--browser` / `--e2e`。
- 收尾某项失败、修好后，只重跑失败项（直接跑对应命令）和 `--affected`；修复本身改到了新的触发路径时再跑 `--finish`。
- 只想验证不想更新本机程序时加 `--no-update`。`--debug` 构建只用于临时调试，不算验证证据。

## 结果状态

| 状态 | 含义 |
| --- | --- |
| `PASS` | 要求的测试与构建在本次改动后实际通过，且不依赖未做的实机验证 |
| `partial` | 部分检查通过；有构建失败、环境阻塞或未执行项（写明原因） |
| `unverified` | 软件检查通过，但未连接硬件、未实机出光、未用真实账号或设备 |

不用旧产物充当本次结果。构建成功不等于实机通过；安装、发布、设备操作另需授权。

## 变更记录

代码修复或功能变更在 `CHANGELOG.md` 的 `[Unreleased]` 顶部加一条（文档-only、测试-only 不需要）。标题直接写结论，正文四项、各一两句；格式与长度由 `check-repo-hygiene.mjs` 检查：

- **现象**：触发条件与影响。
- **根因与修复**：原因、关键改动；行为变更另写默认值、契约、安全门禁、跨端影响，破坏性变更显式标注。
- **验证**：检查项与测试数量、关键测试名、未验证项（可直接用 `verify.mjs` 结尾打印的摘要）。
- **关联**：主要文件与符号。

已发布版本的记录归档在 [`docs/changelog/`](docs/changelog/)。交付构建的时间、大小、SHA-256 写入 [`build-receipts.md`](docs/developer/build-receipts.md)。

## 完成标准

- 代码修复或功能变更：CHANGELOG 已更新、文档已同步、`--finish` 已通过（含自动快更），才能报告完成；阶段收尾还要求交付级验证通过。
- 报告实际运行的检查、跳过项及原因、未验证风险。

## 构建缓存

快更脚本结尾自动执行 `cargo sweep --time 3`。磁盘紧张时运行 `bash scripts/clean-build-cache.sh`（Windows `scripts/clean-build-cache.ps1`，可先加 `--dry-run` / `-WhatIf` 预览），它还会清理 `incremental`、`target/tmp` 与本仓库 crate 的旧哈希产物。`Cargo.toml` 的 `[profile.dev] debug = "line-tables-only"` 是为了控制调试符号体积，不要改回。
