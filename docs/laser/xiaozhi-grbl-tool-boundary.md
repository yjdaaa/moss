# 小智 GRBL 工具边界

本仓库是小智/MOSS 的本地 MCP 服务，不是 ESP32 固件侧 `self.grbl.*` 工具实现。小智设备负责语音入口、唤醒、ASR/TTS、显示和音频会话；本仓库通过 `laser-mcp` 的 WebSocket 桥暴露 `laser_tools::ToolCatalog` 中的工具，需要持久化或真实作业的调用再转发到进程内 `laser-api`。

使用者入口见 [快速开始](../user/quick-start.md)，维护者改高风险工具前先看 [开发者安全入口](../developer/safety.md)。

## 边界原则

- `self.grbl.*` 名称属于小智设备固件侧能力，可能直接控制设备本地 UART；不要把它们和本仓库的 MCP 工具混用。
- 完整任务只有一条路径：`laser_workflow_tool` 先 `preview` 冻结文件 SHA-256 与加工参数，再 `confirm_send` 且 `confirmed=true`。发送前后端会重算 SHA、握手、读 `$30`、确认 Idle 与引脚状态。
- `laser_safe_action_tool` 把自然动作映射到受控命令：`status` 与进给/功率倍率不需确认，`unlock`、`home`、`set_origin`、`laser_off`、`absolute_move`、`relative_move`、`emergency_stop`、`soft_reset` 必须 `confirmed=true`。移动动作固定先 `M5` 关光。
- `check_laser_connection_tool` 只做连接检查：打开串口或 Telnet，发 `$$` 读到终止 `ok` 并要求 `$32=1`；`include_detail=true` 时附带固件设置行。没有 HTTP `[ESP800]` 路径。
- `laser_grbl_tool` / `laser_network_grbl_tool` 的 `send_file` 与 `send_command` 已被拒绝，只保留 `status` / `check` / `probe` 与按 `job_id` 的 `cancel`。
- 文字工具（`generate_text_laser_task_tool`、`generate_text_image_gcode_tool`、`ai_laser_gcode_tool`）只接受 `text`，都生成 workflow 预览；图片素材只能从网页或桌面端上传。

## 路由建议

| 用户意图 | 推荐工具 | 关键边界 |
| --- | --- | --- |
| "检查激光机开没开、连没连上" | `check_laser_connection_tool` | 只读握手；排障时再用 `include_detail=true` |
| "查状态、解锁、回零、设原点、关激光、移动、急停、软复位" | `laser_safe_action_tool` | `status` 可直接执行；其他状态改变动作必须确认 |
| "快点 / 慢点 / 亮一点 / 暗一点 / 恢复正常" | `laser_safe_action_tool` 的 `feed_plus_10`、`feed_minus_10`、`power_plus_10`、`power_minus_10`、`feed_reset`、`power_reset` | 实时倍率字节，即时生效，不需确认 |
| "帮我刻 xxx、刻名字、刻一段文字" | `laser_workflow_tool` `action="preview"` 带 `text`、`material`、`thickness_mm` | `xxx` 是文字内容；缺材料或厚度先问；默认不发送 |
| "文字效果太焦/太浅/切不透/毛边/断线" | `refine_laser_params_from_feedback_tool`，必要时 `regenerate_text_laser_task_tool` | 先给建议或重新生成文件，不控制硬件 |
| "先调参、打测试矩阵、找参数" | `run_calibration_grid_tool`，再 `select_calibration_cell_tool` | 生成不发送；`calibration_id + confirmed=true` 才发送 |
| "开始正式雕刻/切割这个任务" | `laser_workflow_tool` `action="confirm_send"`，或 `start_tuned_job_tool` | 必须 `confirmed=true`；`dry_run=true` 只返回状态；改过材料、厚度、文件或参数必须重新预览 |
| "现在到哪了 / 还在跑吗" | `laser_workflow_tool` `action="status"`（无 id 取最近一条）或 `recent` | 只读 |
| "停止 / 取消" | `laser_workflow_tool` `action="cancel"` | 停止后续发送，不等于物理急停；急停走 `laser_safe_action_tool` |
| "发送已有 G-code 文件 / 发一条 $$" | 不提供语音路径 | 已有 G-code 在网页以 `source_type=gcode` 预览后确认；任意单命令不开放 |

## 禁止绕行

不要使用 `command_execution_tool`（默认不注册）、固件侧直接控制假设、HTTP 逐行发送或用户历史上下文中猜测的 IP、串口、文件名来绕过激光安全检查。网络完整任务只走 Telnet；`resolve_connection_spec` 只在参数缺失时回退到 `.env` 的默认值，含 `<` 的占位值视为未配置。
