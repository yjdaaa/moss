# Web API 接口契约（`/api/*` 与 `/v1/*` 路由、文件与静态服务）

状态：2026-09-21 按 `crates/laser-api/src/lib.rs` 现状核对。路由表与字段以代码为准；黄金样本 `tests/golden/api/` 记录的是 Python 版响应，比对规则见 §6。
历史依据：原稿从 Python 版 `web_server.py` 提取（2026-09-14），设计决定见已归档的 [重构规格](../archive/rust-rebuild-spec.md) §4.3、§17。
用途：前端、MCP 工具转发（`laser_host::InProcessBackend` 直接调用同一 `ApiService::handle_http`）与测试共同遵守的 HTTP 契约。MCP 工具描述和小智提示词的自然语言调整不改变本文约定的字段、状态码和响应结构。

路由总表：

| 方法 | 路径 | 说明 | 详见 |
|---|---|---|---|
| GET | `/api/health` | 存活检查 | §2 |
| GET | `/api/system/info` | 端口、MCP 桥状态与遥测；非回环也公开 | §2 |
| GET | `/api/ui/runtime-config` | 连接默认值与上传后缀 | §2 |
| GET | `/api/ui/serial-ports` | 系统串口列表 | §2 |
| GET | `/api/ui/discover-controllers` | mDNS 发现 FluidNC | §2 |
| GET | `/api/ui/material-options` | 材料名与厚度 | §2 |
| GET | `/api/ui/material-recommendation` | 材料推荐参数 | §2 |
| GET | `/api/material-lab/export` | 材料库导出 | §2 |
| GET / POST | `/api/material-lab/recent` | 最近一条含完整参数的 workflow | §3.11 |
| GET | `/preview` | 受控文件服务 | §2.1 |
| GET | `/`、`/draw`、`/material-lab`、`/assets/*` | 嵌入的前端 | §2.2 |
| POST | `/api/upload` | 上传图片或 G-code | §3.1 |
| POST | `/api/draw/preview` | 画板预览 | §3.2 |
| POST | `/api/connection/check` | 连接检查与设备能力 | §3.3 |
| POST | `/api/text-task/generate` | 文字任务预览 | §3.4 |
| POST | `/api/text-task/send` | 文字任务确认发送 | §3.5 |
| POST | `/api/workflow` | workflow 状态机 | §3.6 |
| POST | `/api/material-lab/preview` / `send` / `status` / `select-cell` / `manage` | 材料实验室 | §3.7～3.10 |
| POST | `/api/device/framing` | 走边框 | §3.12 |
| POST | `/api/safe-action` | 受控安全动作 | §3.13 |
| POST | `/api/settings` | 保存监听端口 | §3.14 |
| GET | `/v1/capabilities`、`/v1/events`、`/v1/material-lab/bootstrap` | 能力、事件、材料页首屏 | §5 |
| POST | `/v1/auth/challenge`、`/v1/auth/pair` | 局域网配对 | §5 |

---

## 1. 通用约定

### Rust 发送状态增补（2026-09-19）

- 完整 workflow/材料矩阵开始发送前，完整 `$$` 响应、`$32=1`、有限正数的 `$30` 和 Idle（含 `Pn:` 无 `E`/`F`/`D`、无 SD 运行标记）必须通过；`$30` 不是 1000 时按 `$30/1000` 在发送流水线中换算 S 值，原文件与 SHA 门禁不变。
  握手失败返回 400 `device_not_ready`，功率/状态检查失败返回 400 `device_preflight_failed`；不写加工指令。
- 活跃 `send_result` 增加 `phase`：`streaming` 或 `awaiting_completion`；兼容已有 `sending`/`running` 任务状态。
- `lines_sent` 仅表示原文件收到 ACK 的行数。所有行收到 ACK 后保持 `running=true` 和设备占用，
  直到 `G4 P0` 同步 ACK 及后续 Idle 通过，才返回 `completed`（`phase=completed`）。
- 失败/取消沿用既有状态，失败文案可能表示执行结果未知；客户端不能把传输 100% 或失败当成物理停机。

### 1.1 响应包装

| 情况 | 响应体 | HTTP 状态 |
|---|---|---|
| 成功 | `{"success": true, "result": <任意>}` | 200 |
| 失败 | `{"success": false, "result": "<错误文案>"}`，有细节时加 `"detail": <任意>` | 400 / 401 / 404 / 429 / 500；401、429 仅用于新增鉴权层 |

所有 JSON 响应带 `Cache-Control: no-store`。工具层返回的 `success/result/detail` 结构原样透传。

### 1.2 请求体

- 只接受 JSON 对象；`Content-Length` 为 0 视为空对象。
- 超限 → 400 `请求体过大`；解析失败 → 400 `JSON 解析失败: …`；顶层非对象 → 400 `JSON 顶层必须是对象`。
- 上限：`/api/upload` 20 MiB；`/api/draw/preview` 10 MiB；其余 64 KiB。
- 未知路径 → 404 `未找到路径`。

### 1.3 字段转换规则

| 规则 | 行为 |
|---|---|
| bool | 原生 bool 原样；`None`/`""` 取默认；否则转小写去空格后属于 `1 true yes y on 确认 confirmed` 为 true，**其余任何值都是 false** |
| float | `None`/`""` 取默认；`float()` 失败 → 400 `<key> 必须是数字` |
| int | `None`/`""` 取默认；`int(float())`，`"3.7"` → 3；失败 → 400 `<key> 必须是整数` |
| 可选正数 | 空 → 未设置；负数 → 400 `<key> 不能小于 0`；0 → 未设置 |
| 必填文本 | `strip()` 后为空 → 400 `<key> 不能为空` |

Rust 实现复刻这些规则（`parse_bool` / `parse_float` / `parse_int`），包括 `"false"` 字符串被当作 false 只是因为它不在真值集合里。

### 1.4 `result.web` 链接补充

成功时给 `result` 追加 `web` 对象（由 `result.image_file` / `result.gcode_file` 或 `result.artifacts.*` 推导）：

| 字段 | 来源 |
|---|---|
| material_preview_url | `image_file` → `/preview?path=` |
| gcode_preview_url | G-code 渲染的 PNG 预览 |
| gcode_download_url | `/preview?path=<gcode>&download=1` |
| image_preview_url | 等于 material_preview_url，没有则 gcode_preview_url |
| uploaded_image_url、scene_download_url | 仅 `/api/draw/preview`：上传的画板图片与 `.excalidraw.json` 场景 |

没有 `gcode_download_name`、`task_json_url`、`workflow_json_url`、`calibration_session_url`。

非回环场景下这些 `/preview` URL 仍受 Bearer 保护。前端必须通过统一鉴权 `fetch` 取得图片或下载文件，再使用 Blob/Object URL 展示或触发下载；不得把受保护 URL 直接交给不能附加 `Authorization` 的 `<img src>`、普通下载链接或原生 `EventSource`。回环场景可继续直接使用 URL。

### 1.5 默认值来源

连接参数由 `laser_tools::resolve_connection_spec` 解析：请求字段优先，缺失时回退 `.env` 的 `GRBL_DEFAULT_PORT`、`GRBL_BAUDRATE`（115200）、`LASER_NETWORK_HOST`、`LASER_NETWORK_TELNET_PORT`（23）、`LASER_DEFAULT_CONNECTION_MODE`（network）。请求中可用的连接键：`connection_mode` / `preferred_mode`、`port` / `serial_port`、`baudrate` / `baud`、`network_host` / `host`、`network_telnet_port` / `telnet_port`。文中写"设置默认"即指此。

## 2. GET 路由

| 路径 | 参数 | 成功 `result` | 失败 |
|---|---|---|---|
| `/api/health` | 无 | `{"status":"ok"}` | 无 |
| `/api/system/info` | 无 | `{actual_port, preferred_port, is_fallback, web_host, mcp:{configured, endpoint(token 脱敏), client_info, last_request, messages_count, tools_count, protocol_version, recent_events[], core_tool, safety_gate, estop_channel}}` | 无；非回环也免鉴权 |
| `/api/ui/runtime-config` | 无 | `{connection:{default_mode, preferred_modes:[默认, 另一], serial:{default_port:"", baudrate:115200}, network:{default_host:"", transport:"telnet", http_port:80, telnet_port:23, timeout:5.0}}, upload:{gcode_suffixes:[".gcode",".nc"]}}` | 无 |
| `/api/ui/serial-ports` | 无 | `{ports:[…], default_port, baudrate}`；只列系统真实识别到的串口 | 无 |
| `/api/ui/discover-controllers` | 无 | `{controllers:[{hostname, ip, telnet_port, http_port}], hint}`；mDNS 浏览约 2 秒，不向设备发命令 | 无 |
| `/api/ui/material-options` | 无 | `{materials:[{name, thicknesses:[数字升序]}]}` 按 name 排序 | 500 `读取材料选项失败` |
| `/api/ui/material-recommendation` | `material` 必填、`laser_mode` 必填、`thickness_mm`、`engraving_mode` 可选、`power_scale` 可选 | 白名单投影：`material, thickness_mm, matched_thickness_mm, match, laser_mode, laser_max_power, feed_rate, passes`；engrave 且有值时加 `engraving_mode` | 400 `material 和 laser_mode 不能为空` / `读取材料推荐失败` |
| `/api/material-lab/export` | 无 | `{params_file, library:{version:1, power_scale, materials}}` | 500 |

### 2.1 `/preview` 文件服务

参数：`path` 必填；`download` 为 `1` 或 `true` 时附加 `Content-Disposition: attachment; filename="<文件名>"`。没有 `filename` 参数。

白名单，两条都要满足：

1. 后缀属于 `.png .jpg .jpeg .bmp .gif .webp .gcode .nc .json`。
2. 解析后位于运行目录的 `previews/`、`uploads/`、`generated_assets/` 之一（`ApiService::with_preview_roots`）。

成功：文件原始字节，`Content-Type` 按扩展名。失败一律 **404** JSON：`缺少 path` / `不支持预览该文件类型` / `文件不存在或不在允许的预览目录内` / `文件不存在`。

### 2.2 静态服务

`/`、`/draw`、`/draw/*`、`/material-lab` 返回嵌入的 `frontend/dist/index.html`，`/assets/*` 返回同目录静态资源（`rust-embed`，路径经 `safe_asset_path` 过滤）。`/material-lab` 是前端路由，首屏数据来自 §2.3。

### 2.3 `/material-lab` 初始数据来源（已决定 2026-09-14：新增 `GET /v1/material-lab/bootstrap`）

决定：不再在 "组合 17 路由" 与 "新增 /v1" 之间二选一，直接新增 `GET /v1/material-lab/bootstrap`，作为材料实验室页面唯一的首屏初始化接口。理由：组合 4~5 个接口拼首屏必然出现分批到达导致的闪烁与竞态，且接口不定则阶段 6（API）与阶段 7（前端）无法并行。

- 响应包装与 `/api/*` 相同：`{"success": true, "result": {...}}`；`result` 为与旧 `__MATERIAL_LAB_DATA__` 语义等价的单个 JSON 对象，一次请求返回页面首屏所需全部数据（材料列表与选项、最近记录、运行时配置、串口列表等，以阶段 1 提取的字段清单为准）。
- 字段表（Rust 实现）：

  | 字段 | 类型 | 来源 | 说明 |
  |---|---|---|---|
  | `version` | int | 恒为整数 `1`（归档规格 §22 `version` 决定） | 不再透传文件值 |
  | `materials` | object | 材料库 `materials` 嵌套结构，与 `material_params_tool` export 同构，按 db-schema.md §3.6.2 冻结 | 主体数据；含内置预设，用户库文件缺失时即为预设，不是 `{}` |
  | `error` | string \| null | 材料库加载错误 | 文件缺失不是错误（`null`）；只有文件存在但无法解析或 `version` 无法归一化时非空，此时 `materials` 为空对象 |
  | `prototype` | bool | 固定 `false` | 保留 |
  | `defaults.network_host` | string | 固定 `""` | 不猜设备地址 |
  | `defaults.network_telnet_port` | int | 固定 `23` | |
  | `source_path` | string | `LASER_MATERIAL_PARAMS_FILE` 的文件名部分，未设置为 `""` | 任何请求不返回目录成分 |
  | `backup_path` | string | 固定 `""` | |

- 材料库文件缺失或未配置时返回内置 4 种材料，`error` 为 `null`。
- 前端 `/material-lab` 挂载时只调用本接口一次；后续增删改仍走现有 `/api/material-lab/*` 路由，成功后重新拉取 bootstrap 或局部刷新。
- 认证规则同 §5：回环地址免认证，非回环要求 bearer。只读接口，不涉及设备。

## 3. POST 路由

### 3.1 `/api/upload`

| 字段 | 类型 | 必填 | 规则 |
|---|---|---|---|
| filename | string | 是 | 去路径与 `<>:"/\|?*` 及控制字符；后缀限 `.png .jpg .jpeg .bmp .gif .webp .gcode .nc`（**无 .json**） |
| content_base64 | string | 是 | 允许 `data:…;base64,` 前缀；严格 base64 |

成功 `result`：`{file_path, filename, size, suffix, preview_url}`，`file_path` 与 `filename` 都是保存后的文件名。保存名 `{unix 秒}_{sha1 前 10}_{stem 前 32}{suffix}` 到运行目录 `uploads/`；不登记数据库。
失败 400：`不支持上传该文件类型`（detail `{filename, allowed}`）、`上传内容为空`、`上传内容不是合法 base64`、`上传文件为空`。不做图片魔数校验。

### 3.2 `/api/draw/preview`

| 字段 | 类型 | 必填 | 规则 |
|---|---|---|---|
| image_data_url | string | 是 | `data:image/…;base64,` 图片；解码并校验为图片，尺寸为 0 时 400 |
| scene_json | object 或 JSON 字符串 | 否 | 存为 `<image>.excalidraw.json` |
| material | string | 是 | |
| thickness_mm | float | 是 | >0 |
| mode | string | 否 | `raster`（默认）或 `outline` |
| width_mm、height_mm、pixel_size_mm | 可选正数 | 否 | 缺省按图片像素与默认点距 |
| 连接字段 | 见 §1.5 | 否 | 只记录，不连接设备 |

后端直接生成光栅或轮廓 G-code（`generate_draw_artifacts`），写入运行目录 `generated_assets/`，创建 `source_type="draw"` 的 workflow 并冻结 `confirmation_snapshot`。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`time_estimate`、`draw_lab:{job_summary, content_fingerprint, uploaded_image_file, scene_file}` 与 §1.4 的 `web`。
失败 400：各校验文案；`保存画板素材失败: …`。前端未收录的字段（`threshold`、`dither_algorithm`、`raster_quality_strategy` 等）当前被忽略。

### 3.3 `/api/connection/check`

| 字段 | 类型 | 默认 |
|---|---|---|
| connection_mode / preferred_mode | string | 空或 `auto/default/按配置` 走配置默认；别名 serial/usb/com/串口、network/net/wifi/http/telnet/网络 |
| auto_fallback | bool | true |
| port / serial_port | string | 设置默认 |
| baudrate | int | 设置默认 |
| network_host / host | string | 设置默认 |
| network_transport | string | `telnet` |
| network_http_port、network_telnet_port | int | 设置默认 |
| network_timeout | float | 设置默认 |
| include_detail | bool | false |

**总是 `success:true`**。`result`：`{preferred_mode, selected_mode|null, fallback_used, attempts:[{mode, target, connected, summary, laser_mode_ok, firmware_settings(仅 include_detail), timeout_seconds}], serial_connected, network_connected, summary, machine_limits:{x_travel_mm, y_travel_mm, z_travel_mm}|null, features:{speed_map, has_air_assist, has_door}|null}`。`machine_limits` 来自 FluidNC `$CD` 的 `max_travel_mm` 或经典 GRBL 的 `$130/$131`；前端用它约束画板范围。只有参数类型错或模式非法才 400。检查会真实握手（`$$` 到 `ok`、要求 `$32=1`），不发送运动或出光指令。

### 3.4 `/api/text-task/generate`

| 字段 | 类型 | 必填 | 默认 |
|---|---|---|---|
| text、material | string | 是 | `text 不能为空` |
| thickness_mm | float | 是 | >0，否则 400 `thickness_mm 必须是大于 0 的数字` |
| engraving_mode | string | 否 | `raster`；`outline` 走字体矢量轮廓 |
| font_size_mm | float | 否 | 10.0 |
| letter_spacing_mm | float | 否 | 0.5 |
| font_path | string | 否 | 缺省使用内置字体 |
| 连接字段 | 见 §1.5 | 否 | 只记录 |

后端用材料库推荐参数生成 G-code 与预览 PNG（运行目录 `generated_assets/`），创建 `source_type="text"` 的 workflow 并冻结快照。
成功 `result` 含 `workflow_id`、`status=preview_ready`、`gcode_file`、`gcode_sha256`、`image_file`、`font_path`、`material`、`thickness_mm`、`width_mm`、`height_mm`、`glyphs`、`path_optimization`、`time_estimate:{estimated_seconds, estimated_minutes, laser_seconds, non_laser_seconds}` 与 §1.4 的 `web`。
没有材料安全门：`laser_core::safety::evaluate_material` 未接入本路由，PVC 等材料目前只由提示词与人工核对拦截。MCP 工具 `generate_text_laser_task_tool` / `generate_text_image_gcode_tool` / `ai_laser_gcode_tool` 与 `laser_workflow_tool preview`（带 `text`）都转发到本路由；工具的 `font_size` 按 `32px≈1mm` 折算为 `font_size_mm` 并限制在 3～80。

### 3.5 `/api/text-task/send`

白名单字段，其余全部丢弃：

| 字段 | 说明 |
|---|---|
| workflow_id | 必填 |
| confirmed | bool，默认 false；HTTP 层校验，缺失或为 false 时 400 `请确认发送安全选项 (confirmed=true)` |
| connection_mode | 规则同 §3.3 |
| run_in_background、wait_for_response | 默认 true |
| serial 时 | `port \| serial_port`、`baudrate` |
| network 时 | `network_host`（默认 ""）、`network_http_port`、`network_telnet_port`、`network_timeout`；`network_transport` 强制 `telnet` |

确认请求只需提交 `workflow_id` 与 `confirmed=true`（以及本表允许的连接参数）；客户端不提交快照 SHA。服务端加载该 workflow 的 `confirmation_snapshot`，重新计算当前 G-code 文件 SHA-256，并按 §3.6 的门禁顺序校验；任一不一致均拒绝发送。本路由与 `/api/workflow confirm_send` 等价。

### 3.6 `/api/workflow`

`action` 默认 `status`；`workflow_id`。支持的 action：

| action | 说明 | 失败 |
|---|---|---|
| `preview` / `plan` / `create` | `source_type=text` 时等价于 §3.4；`source_type=image` 需要 `image_file`（已上传文件名，位于预览白名单目录）、`material`、`thickness_mm`（默认 3.0）；`source_type=gcode` 需要 `gcode_file`。生成产物、创建记录并冻结快照 | 400 `未指定图片素材路径` / `找不到指定的素材图片文件` / `未指定 G-code 刀路路径` / `找不到指定的 G-code 刀路文件` / `图片尺寸为 0` |
| `status` | 返回记录并叠加内存中的实时 `send_result`（`running, total_lines, lines_sent, target, phase, error, sd_hash_verified, warning`） | 400 `workflow_id 不能为空`；404 `未找到 workflow` |
| `recent` | 最近记录列表 | |
| `confirm_send` / `send` | 必须 `confirmed=true`；门禁顺序：记录存在 → 状态可发送（`sending/completed/failed/cancelled` 拒绝，`workflow_not_sendable`）→ 快照存在（`confirmation_snapshot_missing`）→ 物理文件 SHA-256 重算一致（`send_gate_rejected`）→ 设备连接、握手、`$CD`、预检、行校验（§1 增补）→ 同设备无在途作业（409 `device_busy`）。通过后后台逐行发送，响应 `status=sending` 与 `send_result`。可带 `offline_sd=true`（或 `send_mode=sd_offline`）走 SD 脱机模式 | 400 / 404 / 409 |
| `resume` | 从 `start_from_line` 断点续传；同样要求 `confirmed=true`、快照与 SHA 一致，且记录未全量完成（`workflow_completed` / `workflow_not_resumable`） | 400 / 404 |
| `cancel` | 置取消并走取消序列；不承诺物理停机 | 400 / 404 |

未知 action → 400 `未知 workflow action`。`update`、`feedback`、`regenerate` 不是 workflow action：反馈调参与重生成是独立 MCP 工具。响应经 §1.4。

### 3.7 `/api/material-lab/preview`

| 字段 | 类型 | 必填 | 默认 / 规则 |
|---|---|---|---|
| material | string | 是 | |
| thickness_mm | float | 是 | >0 |
| laser_mode | string | 否 | `engrave`；`engrave/cut` |
| engraving_mode | string | 否 | `raster`；cut 时置空 |
| matrix_kind | string | 否 | `auto` 随 laser_mode；`raster/outline/engraving` → engrave；否则须 `engrave/cut` |
| rows、columns | int | 否 | 5、5 |
| power_min、power_max、speed_min、speed_max | int | 否 | -1 |
| passes、pass_min、pass_max | int | 否 | 0、1、0 |
| cell_size_mm、gap_mm、x0、y0、fill_step_mm | float | 否 | 5.0、2.0、0、0、0.8 |
| network_host | string | 否 | "" |
| network_transport | string | 否 | `telnet` |
| network_http_port、network_telnet_port、network_timeout | int、int、float | 否 | 设置默认 |
| wait_for_response、run_in_background | bool | 否 | true |

Rust 实现：只生成不出光。区间字段缺省或为 -1 时以材料推荐参数为中心取 ±40%；`rows/columns` 限 1..25。成功 `result` 为完整 calibration 记录：`calibration_id`（`cal-<ts>-<seq>`）、`status=prepared`、`cells[]`（`cell_number/id/row/column/x_mm/y_mm/size_mm/power/speed/passes`）、`cell_count`、`gcode_file`、`gcode_sha256`、`confirmation_snapshot`、`time_estimate`、`web.gcode_download_url`。记录写入 `calibrations` 表（无 SQLite 时写内存表）。

### 3.8 `/api/material-lab/send`

| 字段 | 规则 |
|---|---|
| calibration_id | 必填 |
| confirmed | **必须为 true**，否则 400 `发送测试矩阵需要 confirmed=true` |
| network_host | 空且无默认 host → 400 `请填写设备 host/IP` |
| network_transport | `telnet` |
| network_http_port、network_telnet_port、network_timeout | 设置默认 |
| wait_for_response、run_in_background | true |

Rust 实现：与 `/api/workflow confirm_send` 同一套门禁——记录不存在 404 `calibration_missing`；`sending/completed/failed/cancelled` 状态 400 `calibration_not_sendable`；物理刀路 SHA-256 重算不一致 400 `send_gate_rejected`；设备不可达/握手失败 400 `device_unreachable`/`device_not_ready`；同一设备已有在途作业 409 `device_busy`。通过后由 JobWorker 后台逐行下发，响应 `status=sending` 与 `send_result{running,total_lines,lines_sent,target}`，终态由发送线程写回记录。`POST /api/material-lab/status {calibration_id}` 返回带实时进度的记录，供前端轮询；缺 id 400 `calibration_id 不能为空`，不存在 404 `未找到标定矩阵`。

### 3.9 `/api/material-lab/select-cell`

`calibration_id` 必填；`cell_number` int 默认 0；`thickness_mm` float 默认 0.0；`material`、`laser_mode`、`engraving_mode`、`notes` string 默认 ""。

Rust 实现：`cell_number` 必须在 1..cell_count；缺省的 material/thickness/laser_mode/engraving_mode 从 calibration 记录回填；把单元格的 `power→laser_max_power`、`speed→feed_rate`、`passes`（及 `pixel_size_mm`）以 `source=calibration` 经 `manage(save)` 写入材料库文件与 SQLite，并在记录上写 `selected_cell`。成功 `result`：`{calibration_id, cell_number, selected_cell, materials, speech}`。

### 3.10 `/api/material-lab/manage`

`action` 必填，`save / delete / import / export`，否则 400。

| action | 字段 | 结果 |
|---|---|---|
| export | 无 | 工具透传；`version` 恒为整数 1，`materials` 含内置预设（db-schema.md §3.6.1、§3.6.2） |
| import | `library_json \| params_json`、`import_mode` 默认 `merge` | 工具 result + `materials` 全量快照 + `backup_path`。`version` 校验严格：只接受 JSON 数值 `1`/`1.0`，`"1"` 或其他值一律 400 `导入材料库格式不支持：需要 version=1 的 JSON 对象`。`backup_path` 只返回文件名 |
| delete | `material` 必填、`thickness_mm` float、`laser_mode` 默认 engrave、`engraving_mode` 默认 raster、`confirmed` **必须 true** 否则 400 `删除当前参数需要 confirmed=true` | 工具 result + `materials` |
| save | delete 的字段 + `laser_min_power` 0、`laser_max_power` 800、`feed_rate` 1200、`travel_rate` 设置默认、`pixel_size_mm` 0.1、`threshold` -1、`passes` 1（数值字段在入库边界统一转为数字，无法转换返回 400）、`aliases`（数组或中英文逗号分隔）、`notes`、`source` 默认 manual、`machine_profile_id` 默认 `yisu-v1-100x100`、`confidence`、`evidence`（对象转 JSON）、`version_mode` 默认 update。**原子批量扩展（D9 已拍板）**：除单厚度参数外，允许传入 `thicknesses` 对象/列表进行多厚度一次性原子保存，后端在单个数据库事务内完成全部更新，失败整体回滚；禁止前端通过循环发送 N 次 HTTP 请求进行多厚度保存 | 工具 result + `materials` |

### 3.11 `/api/material-lab/recent`

GET 与 POST 都接受；忽略请求体（仍受 64 KiB 限制）。从 workflow 按 `updated_at` 倒序取第一个含完整参数键的记录。
成功 `result`：`{workflow_id, material, thickness_mm, laser_mode, engraving_mode, params}`。
失败 400：`最近的激光 workflow 中没有可保存的完整材料参数`。

### 3.12 `/api/device/framing`

真实驱动机器沿外接矩形走边框，用于校对材料摆放位置。字段：`min_x`、`min_y`（默认 0）、`max_x`、`max_y`（默认 100）、`feed_rate`（默认 2000）、`power`（默认 1，极低功率光斑）与 §1.5 连接字段。流程：生成 framing G-code → 打开传输 → 握手 → `$CD` → `JobWorker::send_lines`。成功 `result`：`{message:"走边框完成", bounds}`；失败 400（生成、连接、握手、行校验）或 500（执行失败）。不经 workflow 快照，也不需要 `confirmed`，改动时按发送链路对待。

### 3.13 `/api/safe-action`

请求体为 `laser_contracts::LaserSafeActionArgs`：`action` 必填，可选 `confirmed`、`dry_run`、`x`、`y`、`feed_rate`、`power` 与连接字段。动作集合见 [开发者安全入口](safety.md) §2；`status` 与进给/功率倍率不需确认，其余需 `confirmed=true`。成功 200，失败 400，响应体 `{success, result, detail}`。MCP `laser_safe_action_tool` 在有宿主时转发到本路由。

### 3.14 `/api/settings`

`{web_port}`，范围 1024～65535，否则 400 `端口号必须在 1024 ~ 65535 之间`。写入运行目录 `settings.json`，成功 `result.message="设置已保存，将在下次启动时生效"`。

## 4. 需要在实现时保持的怪异行为

这些是现状，照搬，不修：

1. `/api/material-lab/export` 与 `manage(action=export)` 两个入口都保留，但 `/api/material-lab/export` 的响应形态是 `{params_file, library}`，`manage(export)` 返回工具结果。
2. `confirmed` 在 HTTP 层统一校验：`text-task/send`、`workflow confirm_send/resume`、`material-lab/send`、`manage(delete)` 缺失都返回 400；工具层门禁是第二道防线。
3. `manage(save)` 的功率、速度、阈值、次数、像素尺寸等数值字段在入库边界统一转为数字；字符串 `"800"` 入库为数值 `800`，无法转换时报 400。导出统一输出数字。
4. `/api/upload` 不校验图片魔数，`draw/preview` 校验。
5. `/preview` 的所有失败都是 404，包括参数缺失。
6. `/api/material-lab/recent` 同时接受 GET 与 POST。

## 5. `/v1` 接口与鉴权

| 路径 | 方法 | 用途 | 认证 |
|---|---|---|---|
| `/v1/capabilities` | GET | `{http:true, serial:true, network:true, shell:false, tray:<桌面平台>, command_execution:<MOSS_ENABLE_DEVELOPER_COMMANDS=true>}` | 公开 |
| `/v1/events` | GET SSE | 一次性事件：返回单条 `event: ready`，`data` 为 `{workflow_id, status}`（按 `?workflow_id=` 查库，缺省 `connected`），随后关闭连接。不是持续推送；前端不使用 | 同旧路由规则 |
| `/v1/material-lab/bootstrap` | GET | 材料实验室首屏一次性初始化数据，字段按 §2.3；只读，不触设备 | 同旧路由规则 |
| `/v1/auth/challenge` | POST | 生成 6 位数字配对码，`result:{pairing_code, expires_at}`，5 分钟过期；**仅回环地址允许**，否则 403 | 回环 |
| `/v1/auth/pair` | POST | `{pairing_code}` → `result:{token, pairing_id}` | 非回环时唯一免 Bearer 的数据接口 |

认证规则：来源为回环地址时所有路由免认证。非回环时，`GET /api/health`、`/v1/capabilities`、`/api/system/info`、页面与静态资源（`/`、`/draw`、`/material-lab`、`/assets/*`、`/vendor/*`）公开，`POST /v1/auth/pair` 免 Bearer，其余数据接口（`/api/*`、`/v1/*`、`/preview`）要求 `Authorization: Bearer <token>`，缺失返回 401 `需要 Bearer 鉴权`，无效返回 401 `Bearer 鉴权失败`。

配对语义（`AuthManager`）：配对码按来源 IP 限流，连续错误 5 次锁定 15 分钟，锁定时返回 429 `配对请求过于频繁，请稍后重试` 并带 `Retry-After`；配对码无效、已使用或已过期统一返回 400 `配对码无效或已过期`，不通过文案区分原因。token 为 32 字节 CSPRNG 随机值的 Base64，服务端只保存其 SHA-256 哈希与 `pairing_id`。**全部状态在进程内存中**，`auth_pairings` 表当前不写入，宿主重启后所有配对失效；没有撤销接口。非回环部署默认仅限受信任局域网明文 HTTP，禁止公网直连；跨网必须由前置代理终结 TLS。移动端复用同一套路由，不新增独立的危险操作接口。

## 6. 黄金样本生成要求（阶段 1）

黄金样本采集自 Python 版（`tests/golden/api/`），Rust 版不再自动差分，保留作为字段形态与错误文案的参考。掩码字段：`workflow_id`、`task_id`、`calibration_id`、`job_id`、`created_at`、`updated_at`、所有绝对路径、`content_fingerprint`、`source_path`、`backup_path`、`params_file`（Python 返回绝对路径，Rust 只返回文件名，值本身不可比）。注意样本文件本身仍含采集机器的绝对路径。材料库样本比对时，Python 侧 `null`/`{}` 墓碑节点视为该格不存在（db-schema.md §3.6.2）。比对时 `result.web.*_url` 只比路径部分。

比对方法（归档规格 §20.1 级别 1）：响应体解析为 JSON 后 deep-equal，不比文本。HTTP 状态码、`success` 值、`result`/`detail` 结构必须相等；错误文案字符串逐字相等，因为用户会看到；数值按数值比较；键序与空白忽略。`/preview` 返回的文件按 SHA-256 比较。黄金样本保存原始响应，掩码与规范化在比对时进行。
