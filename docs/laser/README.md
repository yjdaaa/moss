# 激光文档索引

本目录收集激光硬件、固件与 MCP 激光工具面的人类可读文档。源码事实以 `crates/laser-device`、`crates/laser-tools`、`crates/laser-api` 及对应测试为准；文档用于说明路由、安全边界和维护约束。

## 固件与硬件基线

- 默认机型是大鱼 DIY 翼宿 V1.0：`100 mm x 100 mm` 行程、G3 主板、`24 V 5 W` 激光头、`24 V 6 A` 电源，详见 [硬件 BOM 基线](./hardware-bom.md)。采购清单不能证明主板 MCU、GPIO 或 TTL 极性，配置模板尚不可部署，见 [翼宿 V1.0 适配说明](./fluidnc-yisu-v1.md)。
- 固件按 FluidNC 3.x 理解。上位机通过 `$$`、`?`、`$CD`、`G4 P0` 与固件交互，读取但不改写 `$30`、`$32` 等只读代理；固件侧需要开启的项见 [FluidNC 固件侧配置](./fluidnc-setup.md)。
- 旧版 Grbl_ESP32 / ESP3D 的 `[ESPxxx]` 命令与 HTTP `/command` 入口只作参考，不是当前通信契约；两代固件的差异与命令表见 [网络控制命令参考](./grbl-esp32-network-commands.md)。

## 当前 MCP 激光工具面

`laser_tools::ToolCatalog` 登记 20 个工具，`command_execution_tool` 只在 `MOSS_ENABLE_DEVELOPER_COMMANDS=true` 时列出。没有摄像头、Home Assistant 或 LaserGRBL GUI 工具。激光相关工具如下：

| 工具 | 主要用途 | 是否启动机器 |
| --- | --- | --- |
| `check_laser_connection_tool` | 打开串口或 Telnet 做有界握手（`$$` 到 `ok`、要求 `$32=1`），返回连通与就绪摘要 | 否 |
| `laser_safe_action_tool` | 语义化单动作：`status`、`unlock`、`home`、`set_origin`、`laser_off`、`absolute_move`、`relative_move`、`emergency_stop`、`soft_reset`，以及进给/功率倍率 `feed_plus_10` 等 | `status` 与倍率不需确认；其余需 `confirmed=true` |
| `laser_workflow_tool` | 小智与网页共享的高层工作流：`preview`（文字素材转 `/api/text-task/generate`）、`plan`、`create`、`status`、`cancel`、`confirm_send` / `send`、`recent` | 只有 `confirm_send` 且 `confirmed=true` 才发送 |
| `run_calibration_grid_tool` / `select_calibration_cell_tool` | 生成测试矩阵；`calibration_id + confirmed=true` 时发送；选中最佳格写回材料库 | 生成不发送；发送需确认 |
| `start_tuned_job_tool` | 对已有 `workflow_id` 执行 `confirm_send`；未确认或 `dry_run=true` 时只返回状态 | 需 `confirmed=true` |
| `material_params_tool` / `recommend_laser_params_tool` | 材料库 `list` / `get` / `recommend` / `save` / `delete` / `import` / `export`；参数推荐 | 否 |
| `generate_text_image_tool` | 生成白底黑字文字图片 | 否 |
| `generate_text_laser_task_tool` / `generate_text_image_gcode_tool` / `ai_laser_gcode_tool` | 三者同一分支，只接受 `text` 素材，转发 `/api/text-task/generate`；可传 `text_fill`（`outline` 默认 / `fill` / `hatch`）、`text_align`、`origin`（`center` 默认）与木牌尺寸 `width_mm` / `height_mm`（不给字号时自动适配）；2026-11-06 起 `send_after_generate` 不再顺带发送，预览弹到桌面，提示模型等用户在预览窗“同意开刻”或（未开严格模式时）口头确认后再 `confirm_send` | 否 |
| `refine_laser_params_from_feedback_tool` / `regenerate_text_laser_task_tool` | 按"太焦/太浅/切不透/毛边/断线"有界调参；用原任务输入重新生成 | 否 |
| `laser_grbl_tool` / `laser_network_grbl_tool` | 只保留 `status` / `check` / `probe` 与 `cancel`；`send_file`、`send_command` 直接拒绝并引导走 workflow 或安全动作 | 否 |

图片素材不经语音工具处理：在网页或桌面端上传后走 `source_type=image` 的 workflow；已有 G-code 文件走 `source_type=gcode`。

## 文档

- [FluidNC 固件侧配置说明](./fluidnc-setup.md)：`$10=3`、Telnet/HTTP/mDNS、`Laser:` 段、可选项对上位机行为的影响。
- [翼宿 V1.0 与 FluidNC 配合说明](./fluidnc-yisu-v1.md)：采购资料能证明什么、YAML 模板交付边界、上位机行为。
- [硬件 BOM 基线](./hardware-bom.md)：机器规格、软件默认边界、生效的运行时环境变量、BOM 明细。
- [Grbl_ESP32 与 FluidNC 网络控制命令参考](./grbl-esp32-network-commands.md)：网络入口、`$` 命令、实时字节、高危命令与本仓库策略。
- [小智 GRBL 工具边界](./xiaozhi-grbl-tool-boundary.md)：固件侧 `self.grbl.*` 与本仓库 MCP 工具的分工与路由建议。
- [用户快速开始](../user/quick-start.md)、[开发者安全入口](../developer/safety.md)、[提示词归档](../integration/xiaozhi-prompts/latest.md)。

## 安全规则

文档示例只能使用 `<device-ip>`、`<telnet-port>`、`<path>` 等占位符，不要把真实设备 IP、串口号、Wi-Fi 密码、WebUI 密码、Token 或用户私有路径写入提交内容。

生成图片、生成 G-code、生成文字任务、推荐参数和反馈调参都不等于启动机器。完整发送必须经过 `confirmed=true`、物理文件 SHA-256 重算、`$$` 握手、`$30` 标尺读取、Idle 与引脚预检；每一道门禁的顺序与错误码见 [开发者安全入口](../developer/safety.md)。网络完整任务只走 Telnet。

`laser_workflow_tool cancel` 只停止后续 G-code 发送并走取消序列，不承诺控制器已物理停机。用户明确说"急停、暂停机器、关激光"时走 `laser_safe_action_tool`；`0x18` 是软件复位请求，不替代硬件急停。

## 材料参数维护规则

材料库存储在运行目录的 SQLite 中；设置 `LASER_MATERIAL_PARAMS_FILE` 时另外写入该 JSON 文件并生成 `.bak` 备份，未设置时使用内置四种材料作为初始数据。不要通过手改源码或任务 JSON 来维护材料经验。

- 每格参数按材料、厚度、`engrave` / `cut`、`raster` / `outline` 区分；雕刻的 raster 与 outline 是两套参数，即使数值相同也分别记录，不能自动混用。
- 材料别名可以指向同一材料记录；导入支持 `merge` / `replace`，导出恒为 `version=1` 的 JSON，格式冻结在 [数据库结构](../developer/db-schema.md)。
- 完成测试矩阵后，用户说"第 N 格最好"时使用 `select_calibration_cell_tool` 把该格功率、速度、次数以 `source=calibration` 写回材料库。
- 网页 `/material-lab` 提供增删改、导入导出与矩阵闭环；"应用参数"只影响当前任务或预览，不代表发送设备。切换材料参数后必须重新生成预览并重新确认。
