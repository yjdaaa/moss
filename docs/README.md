# MOSS 文档索引

文档与代码不一致时以代码和测试为准，并顺手修文档。仓库规则只在根目录 [`AGENTS.md`](../AGENTS.md)。

## 用户

- [快速开始](user/quick-start.md)：Windows 上启动桌面端或无界面宿主、`.env` 配置、运行目录、局域网配对、第一次雕刻的安全流程、常见问题。

## 开发者

- [架构总览](developer/architecture.md)：进程与入口、crate 分层与依赖方向、HTTP/SSE/MCP 桥、设备层、存储、退出与急停路径、与早期规划稿的已知偏差。
- [安装包与卸载规范](developer/packaging-install.md)：通用 Windows NSIS 已实施；自定义安装目录、快捷方式、升级和可选保留数据。签名、干净 VM 验收及 NV18 原生依赖许可仍未完成，普通 NSIS 与语音 helper 专用包分别验收。
- [开发规范](developer/standards.md)：目录职责、代码约定、文档约定、PR 检查清单。
- [构建回执](developer/build-receipts.md)：阶段收尾与交付构建的时间、大小、SHA-256。
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

- [融合实施 TODO](integration/moss-fusion-implementation-todo.md)：**唯一进度记录**，先看这里。
- [实机验证顺序与操作手册](integration/moss-hardware-acceptance-plan.md)：从单电脑到三件套的六个阶段，每档的目的、准备、操作与判据。接设备前先看这个。
- [融合技术书](integration/moss-desktop-fusion-technical-spec.md)：外部自建链路、审批、CORS、语音中心、安装、导出、手机连接的设计。
- [内置语音技术书](integration/moss-native-voice-spec.md)：内置语音总体设计与安全边界（含 NV19 规划）。
- [NV10～NV19 实施细则](integration/moss-native-voice-implementation.md)：各 NV 的模块、接口、安全与验收标准。
- [外部链路实施细则](integration/moss-self-hosted-extension-implementation.md)：P2b / P3 / P5。
- [提示词归档](integration/xiaozhi-prompts/README.md)：控制台提示词与知识库文本的唯一维护处；`latest.md` 为当前版本，`history/` 为日期快照。
