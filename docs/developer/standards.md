# 开发规范

面向贡献者，说明目录职责、代码约定、测试与文档维护。仓库级安全边界、验证命令与变更记录规则以根目录 [`AGENTS.md`](../../AGENTS.md) 为准，本文不重复，只补充实现层面的约定。

## 项目结构

| 目录 | 职责 |
| --- | --- |
| `crates/laser-contracts` | 跨 crate 共享的强类型数据模型与 JSON Schema，无内部依赖 |
| `crates/laser-core` | 纯算法（字体、光栅、矢量、G-code、材料推荐、参数范围），不做 I/O |
| `crates/laser-device` | 串口 / Telnet 传输、握手、预检、流控、实时字节、`FakeGrbl` |
| `crates/laser-runtime` | `.env` 配置、运行目录、SQLite schema、`RuntimeStore`、旧数据迁移 |
| `crates/laser-tools` | `ToolCatalog` 与不依赖宿主的工具实现 |
| `crates/laser-mcp` | JSON-RPC 服务、工具分发、小智 WebSocket 桥 |
| `crates/laser-api` | HTTP 路由、workflow 状态机、发送门禁、材料实验室、配对鉴权、嵌入前端 |
| `crates/laser-host` | 组装启动、退出协调器 |
| `apps/moss-host` | 命令行宿主 `serve` / `migrate` |
| `apps/desktop` | Tauri 2 桌面与移动外壳 |
| `frontend/` | React + Vite 前端；`tests/*.test.mjs` 为单测，`tests/e2e/` 为端到端 |
| `tests/golden/` | 从 Python 版采集的黄金样本；`mcp_tools.json` 被 `laser-mcp` 编译期嵌入 |
| `tests/fixtures/` | 字体（`laser-core`、`laser-tools` 编译期嵌入）与示例图片 |
| `tests/visual-baseline/` | 三个页面在桌面与移动视口的截图基线 |
| `scripts/` | `start.bat` 启动器、`test_app_buttons.mjs` E2E 调度器、图标与架构图渲染脚本 |
| `docs/` | 本目录，结构见 [文档索引](../README.md) |

分层与依赖方向见 [架构总览](architecture.md)。不要为了"看起来整齐"新增重复的发送器、配置层或通用工具；只有多个 crate 确实需要复用时才下沉到 `laser-core` 或 `laser-contracts`。

## 代码约定

- 新增 MCP 工具：在 `laser_tools::ToolCatalog` 的 `PRIORITY_TOOLS` 或 `SECONDARY_TOOLS` 登记名称，在 `laser_mcp::dispatch_tool_call_with` 增加分发分支，参数结构放 `laser-contracts`，并同步 `tests/golden/mcp_tools.json`。
- 需要持久化或真实设备的工具不要在 `laser-tools` 里直连数据库或设备；经 `HostBackend` 转发到 `ApiService` 现有路由，与网页共用门禁。
- `laser-tools`、`laser-device`、`laser-core` 不得依赖 `serde_json`，也不得使用 `Value` 动态访问器；类型边界测试会拒绝。
- 环境变量集中在 `laser_runtime::LaserSettings` 与 `laser_tools::resolve_connection_spec` 读取，不在各处重复解析。
- 平台差异（Windows 快捷键、串口枚举、控制台钩子）显式分支，并返回清晰的"不支持"原因。
- 公开工具名、HTTP 路由、错误码、响应字段是对外契约；改动前先更新 [Web API 契约](api-contracts.md) 与测试。
- 用户可见文案是契约的一部分：错误文案会被前端与小智播报直接展示，修改时同步测试断言。

## 安全边界

- 高风险能力与确认门禁的定义见 `AGENTS.md`；实现细节与已知未接线项见 [安全入口](safety.md)。
- 前端只能调用后端路由，不得直接连接激光控制器或发送任意命令。
- Web 服务默认只绑定回环地址；绑定非回环地址时依赖配对鉴权，不要暴露到公网。
- `.env` 只存本机配置；真实 token、密码、设备 IP、串口号和私有路径不得进入仓库、文档或提示词。

## 测试与构建

- Rust：各 crate 使用 `#[cfg(test)]` 单元测试，设备层使用 `FakeGrbl` 做字节级校验；`crates/laser-contracts/tests/type_boundary.rs` 是集成测试。
- 前端：`cd frontend && npm test` 运行 `node --test tests/*.test.mjs`；`npm run test:e2e` 由 `scripts/test_app_buttons.mjs` 拉起已构建的 `moss-host.exe`（`target/release` 或 `target/debug`）和回环 GRBL 模拟器，在临时目录中运行，`MCP_ENDPOINT` 被显式禁用。
- 完整命令、构建要求与 `partial` / `unverified` 标注规则以 `AGENTS.md` 为准。
- 串口、HTTP、Telnet、子进程、文件系统副作用必须使用 mock、`FakeGrbl` 或临时目录；测试不依赖本机材料库、运行目录或真实设备。
- 不为"全绿"放宽断言、伪造预期数据或添加虚假桩代码；测试名称必须如实反映覆盖范围。

## 文档维护

- 目录约定：用户操作放 `docs/user/`，开发者与契约放 `docs/developer/`，硬件、固件与命令放 `docs/laser/`，小智提示词只维护在 `docs/integration/xiaozhi-prompts/`（改 `latest.md` 并新增日期快照），重构期历史文档放 `docs/archive/` 且不再修改内容。
- 改变契约、默认值、工具注册、提示词或用户可见行为时，同一次改动内更新对应文档；功能可见变化同步 [功能字典](feature-glossary.md)。
- 文档示例使用 `<device-ip>`、`<token>`、`<path>` 占位符，不写真实连接信息和本机绝对路径。
- 不在文档中硬编码测试用例数、二进制大小等会随构建变化的数字；这类信息放 `CHANGELOG.md` 条目的验证段。
- 提交前检查 Markdown 相对链接、旧路径（`moss_mcp/`、`tools/*.py`、`core/*.py`）、绝对路径和敏感字段。

## Pull Request 检查清单

- [ ] 改动范围与目录职责一致，没有跨层直连设备或数据库。
- [ ] 新工具已登记 `ToolCatalog`、分发分支、参数结构与黄金样本。
- [ ] 外部效果使用 mock 或 `FakeGrbl`，没有连接真实设备。
- [ ] 没有提交 `.env`、运行目录、构建产物或个人配置。
- [ ] 相关 `docs/` 已更新，链接有效。
- [ ] 已按 `AGENTS.md` 运行受影响的测试与构建，并如实标注跳过项。
- [ ] 代码修复或功能变更已写入 `CHANGELOG.md`。
