# MOSS 文档索引

本目录描述 MOSS 的 Rust 重构版：`crates/` 下的 8 个 crate、`apps/desktop`（Tauri 2）、`apps/moss-host`（HTTP/MCP 宿主）和 `frontend/`（React + Vite）。文档与代码不一致时，以代码、测试和 `CHANGELOG.md` 为准；发现偏差请一并修文档。

仓库级共享规则在根目录 [`AGENTS.md`](../AGENTS.md)，Claude Code 补充说明在 [`CLAUDE.md`](../CLAUDE.md)，本目录不重复这些规则。

## 用户

- [快速开始](user/quick-start.md)：Windows 上启动桌面端或无界面宿主、`.env` 配置、运行目录、局域网配对、第一次雕刻的安全流程、常见问题。

## 开发者

- [架构总览](developer/architecture.md)：进程与入口、crate 分层与依赖方向、HTTP/SSE/MCP 桥、设备层、存储、退出与急停路径、与早期规划稿的已知偏差。
- [开发规范](developer/standards.md)：目录职责、工具注册与分层约定、测试与构建命令、文档维护规则、PR 检查清单。
- [安全入口](developer/safety.md)：改高风险代码前必读。运行时现实、高风险类别、完整发送链路的每一道门禁、已知未接线项、测试与提示词同步要求。
- [Web API 契约](developer/api-contracts.md)：`/api/*` 与 `/v1/*` 路由的请求、响应、错误码与鉴权规则。
- [数据库结构](developer/db-schema.md)：SQLite 表、schema 版本、材料库 JSON 冻结格式、旧数据迁移流程。
- [前端行为契约](developer/frontend-behavior.md)：路由、状态词表与发送覆盖层映射、`localStorage` 键、必须保留的交互行为。
- [功能字典](developer/feature-glossary.md)：面向非技术读者的功能白话说明，按模块列出用途、逻辑与风险。

## 激光硬件与固件

- [激光文档索引](laser/README.md)：硬件基线、当前 MCP 激光工具面、安全规则、材料参数维护规则。
- [FluidNC 固件侧配置](laser/fluidnc-setup.md)：配合 MOSS 需要在固件上开启什么，以及每一项对上位机行为的影响。
- [翼宿 V1.0 适配说明](laser/fluidnc-yisu-v1.md)：采购清单能证明什么、YAML 模板的交付边界、上位机行为。
- [硬件 BOM 基线](laser/hardware-bom.md)：机器规格、软件默认边界、生效的运行时环境变量、BOM 明细。
- [Grbl_ESP32 / FluidNC 网络命令参考](laser/grbl-esp32-network-commands.md)：固件命令、实时字节、高危命令与本仓库策略。
- [小智 GRBL 工具边界](laser/xiaozhi-grbl-tool-boundary.md)：固件侧 `self.grbl.*` 与本仓库 MCP 工具的分工与路由建议。

## 小智集成

- [提示词归档](integration/xiaozhi-prompts/README.md)：控制台提示词与知识库文本的唯一维护处；`latest.md` 为当前版本，`history/` 为日期快照。

## 历史归档

- [归档说明](archive/README.md)：重构期的需求规格、迁移进度、验收报告与旧前端盘点。只读，不再维护，不作为当前架构依据。
