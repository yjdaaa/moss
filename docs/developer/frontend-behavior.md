# 前端行为契约

记录有代码或测试固化、改动时必须保留的前端行为，按当前 `frontend/src` 核对。改这些行为前先改本文与对应测试。

## 1. 路由与 API 基址

| 路径 | 页面 | 主要调用 |
| --- | --- | --- |
| `/` | `HomePage` | `/api/ui/runtime-config`、`/api/ui/serial-ports`、`POST /api/ui/discover-controllers`、`/api/ui/material-options`、`/api/connection/check`、`/api/text-task/generate`、`/api/upload`、`/api/workflow`、`/api/system/info` |
| `/draw` | `DrawPage` | `/api/draw/preview`、`/api/connection/check`、`/api/device/framing`、`/api/workflow` |
| `/material-lab` | `MaterialLabPage` | `/v1/material-lab/bootstrap`（首屏一次）、`/api/material-lab/manage` / `recent` / `export` / `preview` / `send` / `status` / `select-cell` |
| `/twin` | `TwinPage` | `/api/workflow`（`recent` / `status`）、`/preview`、`/api/connection/check`、`/api/safe-action`（`status` / `emergency_stop`） |

路由在 `main.jsx` 按 `pathname` 前缀切换，`popstate` 同步。应用渲染前，Tauri 内嵌页通过 `get_backend_origin` 获取实际回环端口；获取失败显示错误与重试入口，不猜测 9666。`authSession.getApiBaseUrl` 在 `127.0.0.1` 或 `localhost` 且非 Vite 端口（5173/5177/5178）时同源；独立 Vite 开发页使用默认 `http://127.0.0.1:9666`。所有请求经 `apiFetch` 附加 Bearer token，收到 401 时派发 `laser-auth-required` 事件弹出配对窗口。桌面端在渲染前还通过 `get_desktop_session` 取得桌面会话（`authSession.refreshDesktopSession`，只存内存，取不到不影响启动），`apiFetch` 与画板的请求只对本机相对路径附加 `X-Moss-Desktop-Session`；收到 403 `desktop_session_invalid` 时重新取会话并只重试一次，其他 403 不重试。浏览器与 Android 没有桌面会话。

## 2. 状态词表与归一化

实现：`src/state/statusNormalize.js`；测试：`tests/statusNormalize.test.mjs`。

- workflow 状态：`collecting`、`preview_ready`、`sending`、`completed`、`failed`、`cancelled`、`feedback_recorded`。
- job 状态：`pending`、`running`、`completed`、`failed`、`cancelled`。
- 别名归一化：`queued → pending`，`canceled → cancelled`；空值归一为 `pending`。
- 终态集合：`completed`、`failed`、`cancelled`。

发送覆盖层映射（`STAGE_MAP`，必须原样保留）：

| 状态 | 步骤 | 中心文字 | 标题 |
| --- | --- | --- | --- |
| pending / queued | 0 | 等待 / 排队 | 等待任务 |
| preparing | 1 | 准备 | 准备发送 |
| running / sending | 2 | 发送中 | 正在向设备发送 |
| completed | 3 | 完成 | 发送完成 |
| failed | 2 | 失败 | 发送失败 |
| cancelled | 2 | 已取消 | 任务已取消 |

`extractWorkflowInfo` 从 `result.workflow` 或顶层取 `status`，从 `runtime_job` 取 `job_id`，并把 `speech` / `result` 作为播报文本。

## 3. 发送进度与轮询

- 确认发送前先调用 `/api/connection/check`，再调用 `/api/workflow` 的 `confirm_send` 并携带 `confirmed:true`、`workflow_id` 与连接参数；客户端不提交快照 SHA，由服务端重算。
- 进度通过轮询 `/api/workflow {action:"status"}` 获得，显示 `send_result.lines_sent / total_lines`、`target` 与 `phase`；可信终态才停止轮询。审批、工作台与标定轮询同时最多一个请求，每次 5 秒超时，结束后间隔 1 秒再查；取消后迟到结果不写入新任务。失败保留最后状态与进度，显示“状态未知、机器可能仍在运行”，继续重试，不伪造 failed。
- 主页支持 `action:"resume"` 从指定行续传（HomePage 内存态路径）。

## 4. 画板重复发送防呆

实现：`src/repeatSendGuard.js`；测试：`tests/repeatSendGuard.test.mjs`。

- 预览成功后记录响应中的 64 位十六进制 `content_fingerprint`。
- 当前指纹与上次已接受指纹相同时，确认发送必须勾选"已移动材料/原点"确认框，否则 `canSubmitConfirmSend` 返回 false。
- 上次已接受指纹保存在 `localStorage` 键 `laserLab.repeatSend.v1.lastAcceptedFingerprint`。这只是浏览器侧提醒，不替代后端 `confirmed=true`、材料冻结与 SHA 门禁。

## 5. `localStorage` 键

| 键 | 页面 | 内容 |
| --- | --- | --- |
| `laser-forge-conn-mode`、`laser-forge-serial-port`、`laser-forge-baud`、`laser-forge-host`、`laser-forge-telnet-port` | 主页 | 连接方式与参数 |
| `laser-forge-mat`、`laser-forge-thick`、`laser-forge-text`、`laser-forge-font-size` | 主页 | 文字任务表单 |
| `laserLab.material`、`laserLab.thickness_mm`、`laserLab.width_mm`、`laserLab.height_mm`、`laserLab.mode`、`laserLab.connection_mode`、`laserLab.network_host`、`laserLab.network_telnet_port`、`laserLab.port`、`laserLab.baudrate`、`laserLab.vector_simplify_factor` | 画板 | 画板表单（`mode` 旧值 `auto` 读入时按 `raster`） |
| `laserLab.authToken` | 全局 | 局域网配对 token |
| `laserLab.repeatSend.v1.lastAcceptedFingerprint` | 画板 | 重复发送防呆 |
| `moss_draw_layout_v1` | 画板 | 创作画板四张卡片（`board` / `draw-material` / `draw-device` / `draw-send`）自由画布位置、宽高与锁定状态 |
| `moss_close_action` | 全局 | 桌面端关闭行为（`ask` / `tray` / `exit`） |
| `moss_hide_top_pills` | 全局 | 顶栏右上角状态胶囊（硬件 / 小智 / 审批）是否隐藏（`1` 隐藏，`0` 或未设置时默认显示） |
| `moss_yisu_workspace_limits_v1`、`moss_yisu_material_config_v1`、`moss_yisu_hud_collapsed_v1` | 数字孪生 | 翼宿 V1.0 有效工作区范围、材料外框/材料内文字位置配置与 3D 视口浮层收起状态 |

## 6. 其他必须保留的行为

- 主页文字输入上限 24 字（`maxLength={24}`）。
- 串口下拉只显示 `/api/ui/serial-ports` 返回的真实串口，无结果时提示并允许手动输入；不再提供硬编码的 COM 预设。
- 材料下拉来自 `/api/ui/material-options`，只暴露材料名与厚度，不暴露功率/速度。
- 材料实验室保存成功后通过 `BroadcastChannel`（`laser-material-library-v1`）通知其他已打开页面刷新。
- 多厚度保存由后端单次 `/api/material-lab/manage` 原子完成，前端不循环发送 N 次请求。
- Excalidraw 主题变量覆盖 `--color-primary`（`styles.css`），语言 `zh-CN`。
- 桌面端关闭窗口时先弹出"最小化到托盘 / 退出"选择（macOS 显示“隐藏到菜单栏图标”并支持点 Dock 重开窗口；平台自适应文案统一在 `src/platform.js`，单元测试 `tests/platform.test.mjs`，还覆盖手动串口提示、手机连接防火墙提示、音乐目录与 Agent 路径占位及非 Windows 系统的 Agent 沙箱不可用提示）；选择可被记住到 `moss_close_action`。非 Tauri 环境下"退出"只关闭页面并显示会话终止提示。
- 关闭窗口退出、托盘/菜单栏“退出程序”与 macOS `Cmd+Q`（`RunEvent::Exit`）走同一安全收尾：先急停，再停止（或放开）语音服务端、内置语音服务与第三方 MCP 子进程，最后退出（2026-10-14 起托盘退出不再漏掉后两项）。
- 画板设备卡（2026-10-14）：连接方式可选网络（Telnet）或串口（`/api/ui/serial-ports` 刷新、可选波特率），发送、走边框、测试连接都用所选方式（`drawConnection.js`）；串口时确认框不显示 SD 脱机选项。宽 / 高留空表示按导出尺寸，填写后后端按填的尺寸出成品；生成策略只有光栅与轮廓（轮廓沿深色图形描边）。“走边框”默认不出光，勾选“走边框时弱光指示”才传 `power:1`；走边框期间顶栏急停能找到并停止它。
- 顶栏“硬件”胶囊显示最近一次握手探测的结果（`state/hardwareStatus.js`：未探测 / 握手通过 / 未连接），提示里写探测时间，不冒充实时在线；探测在雕刻工作台进行。偏好设置「顶栏右上角状态显示」提供「隐藏右上角状态胶囊（硬件 / 小智 / 审批）」开关（`foldState.js` 的 `readHideTopPills` / `writeHideTopPills`，键 `moss_hide_top_pills`，默认关闭即保持原样显示；开启后隐藏顶栏 `#nav-hardware-status`、`#nav-mcp-status`、`#nav-approvals` 三个胶囊，常驻 `#nav-estop` 急停按钮与 `ApprovalCenter` 新审批自动弹窗保持不变）。控制台“AI 工具路由”卡中的发送门禁、急停方式、核心调度工具标“固定规则 / 固定”，是设计说明，不是检测结果。
- 急停入口一律走 `/api/safe-action {action:"emergency_stop"}` 实时 0x18，不得改走 workflow `cancel`（2026-11-05）：顶栏先按 `/api/jobs/stop-targets` 停后端登记的任务（`globalEstop.js::stopKnownJob`）；没有登记任务时交给当前页面，工作台与画板用 `stopPageTarget` 先按任务编号停，后端找不到该任务（如刚全量发送完、GRBL 仍在执行缓冲）时改按本页已填的设备目标（`estopTargetArgs`）新开连接写 0x18；两者都没有时不发送并提示使用实体急停。审批卡与任务浮层按任务编号急停，数字孪生页无运行任务时按页面连接目标急停。
- 视觉基线：`tests/visual-baseline/` 保存三个页面桌面与移动视口截图，用于人工核对布局，不做像素比对。

## 7. 偏好设置“导出”分区与导出流程（技术书 §9.7）

- 所有导出（画板 PNG / 场景 JSON、预览 G-code / 场景下载、材料库导出）都经 `frontend/src/saveFile.js` 的 `exportFile`；不得在组件里直接用 `<a download>`。
- Tauri 中调用宿主命令 `save_export_file`；返回 `null` 表示用户在“另存为”框中取消，提示“已取消导出”，不当作错误。
- 宿主返回 `export_dir_unavailable:<路径>|<原因>` 时弹出两个动作：“本次保存到下载文件夹”（带 `fallbackDefault: true` 只重试一次）与“重新选择”（调用 `pick_export_dir` 后重试；选择框取消则放弃）。关闭弹窗视为放弃。不得静默改存其他目录。
- 成功后弹出“导出完成”，可“打开所在文件夹”（`reveal_exported_file`）。
- “导出”分区只在桌面版 Tauri 显示（`shouldShowExportSettings`）；Android 与浏览器不显示。目录只能经 `pick_export_dir` 的系统选择框修改，没有路径输入框。
- 偏好设置各分区与工作台卡片可折叠，状态存 `localStorage` 键 `moss_ui_fold_<id>`（值 `open` / `closed`），读写失败时按默认值显示。
- 工作台与创作画板自由画布（2026-10-18 / 2026-10-20 / 2026-10-23 用户决定）：`pages/HomePage.jsx`（五张卡：通信链路 / 加工内容 / 材质与参数 / 加工监视舱 / 加工反馈，默认排布 `DEFAULT_LAYOUT` 为**左窄右宽双栏**——左栏 16 列竖排 `link` / `source` / `material`，右栏 32 列上方 `monitor`、下方紧贴 `feedback`，存储键 `moss_wb_layout_v3`）与 `pages/DrawPage.jsx`（四张卡：创作画布 `board` / 材料与尺寸 `draw-material` / 设备 `draw-device` / 预览与发送 `draw-send`，默认排布 `DRAW_DEFAULT_LAYOUT`，存储键 `moss_draw_layout_v1`）均接入 `components/WorkbenchCanvas.jsx` 与 `WbCard`，各自有独立的栅格位置与宽高，可拖动、可缩放、可跨列、可折叠收起。**48 列栅格**（列是 `1fr`）+ **4px 行单位**（行间 8px）：**拖动过程按像素跟手**（直接写 `transform` / `width` / `height` 并实时钳制在画布合法列边界与最小宽高内，不重排），**松手才吸附到栅格并让位**，落点步进约 27–33px（宽）/ 12px（高）。拖卡片顶部移动（画板卡片内部 `.excalidraw` / `.ob-board-stage` 区域不触发卡片拖动，避免与画布绘画冲突，且缩放后通过 `ResizeObserver` / `resize` 事件刷新 Excalidraw 视口尺寸）；展开卡片有**四条边 + 四个角共 8 个把手**（`[data-wb-resize]`，光标随方向变化）改宽高；与其它卡相交时把对方往下推（不允许重叠）。**表单类卡片自动贴合内容高度与双栏底边对齐**（`fitCardRows` + `alignDualColumns`）：除带弹性视口的 `monitor`（`.chamber-viewport` 设为 `flex: 1 1 180px` 自动吸收高度、`0 / 0 行 PENDING` 进度条始终紧贴卡片底部）与 `board` 外，其余卡片通过 `ResizeObserver` 监听 `.wb-card-content` 实际高度并自动换算栅格行数（双击上下缩放把手也可恢复自动贴合），消除卡片底部大片空白框；且在双栏默认列位置下，若用户未手动拖拽过 `monitor` 高度且 `monitor` 未折叠，`monitor` 自动随左栏总高度伸缩使左右两栏底边齐平（消除切换工具或折叠左栏卡片时的单侧留白）。**折叠时高度收成标题条**（8 行 ≈ 88px，卡片主体隐藏且移除纵向缩放把手、禁止滚轮下滑或往下拉高），同列下方关联卡片自动往上贴合消除空白洞，展开或调高时再将同列下方卡片推回（`reflowVertical`）；`WbCard` 用 MutationObserver 盯 `FoldHeader` 的 `data-open` 交给画布。滚动在卡片内层 `.wb-card-body`，边缘留给把手。纯逻辑在 `src/workbenchLayout.js`（`LAYOUT_VERSION = 5`、`MIN_ROWS = 10`、`settle`、`reflowVertical`、`normalizeLayout`、`readLayout` / `writeLayout` / `clearLayout`，单元测试 `tests/workbenchLayout.test.mjs`）；工具条有「锁定布局」与「还原默认布局」。窄屏 < 1200px 由 CSS 退回单列堆叠并忽略保存的布局。拖动带 4px 阈值，拖动结束会吞掉那一次 click，避免顺带把卡片折叠。浏览器测试 `tests/workbench-layout.browser.mjs`。

## 8. 全局审批卡（技术书 §9.5.2，P1-09）

2026-10-10 修补：审批与工作台覆盖层在通信未知或任务记录终态时仍保留急停，直到用户关闭该覆盖层。停止/取消的请求回复不直接将本地状态改成 cancelled，继续查询并明确提示物理停止未确认；急停绑定当前任务编号，旧请求返回不能修改后续任务。

- 组件 `components/ApprovalCenter.jsx`，挂在 `main.jsx` 的 `App` 根组件，与 `AuthModal`、`CloseConfirmModal` 同级，任何页面都会弹出。纯逻辑在 `approvalState.js`（单元测试 `tests/approvalState.test.mjs`）。
- **只在桌面端工作**：没有桌面会话（浏览器、Android、手机连接电脑的远程模式）时不轮询、不显示，顶栏也不显示“审批”按钮。
- 每 4 秒请求 `GET /v1/approvals?status=pending`；出现新的请求时自动弹出（批准后的发送覆盖层显示期间不打断）。多个请求按创建时间先后处理，卡片顶部提示“还有 N 个请求等待处理”。
- 卡片内容：刀路预览（加工任务读取 workflow 记录的 `image_file` / `preview_file`）、内容、材料与厚度、功率（带标尺）/ 速度 / 遍数、尺寸与预计时长、目标设备（只显示连接方式与串口名，网络设备显示“网络设备”）、来源、摘要前后各 4 位、倒计时条（按服务端时间计算）。记录里没有的字段显示“—”，不编造。
- 交互：必须勾选“我在机器旁，材料已放好、原点已确认，排风已开启”才能点“批准并开始”；批准时提交卡片上的 `digest`。`approval_mismatch` / `approval_expired` 等失败在卡内显示原因，按钮收起为“关闭”，不发送任何指令。倒计时归零自动关闭；“拒绝”立即生效；“稍后处理”只关闭卡片，请求仍在等待，可从顶栏“审批”重新打开。
- 批准后：加工任务与标定矩阵进入 `SendOverlay`（`STAGE_MAP` 四步不变，标题旁标“桌面审批”），全程显示“取消后续发送”（标定矩阵为“停止”）与“急停 0x18”，并注明 0x18 不代替控制器硬件急停；设备动作与走边框执行后弹出结果提示。
- 顶栏“审批”按钮：没有请求为灰色，有请求变红并显示数量；没有请求时点击提示“当前没有待批准的请求”。
- 家居控制请求（`kind=home_control`，2026-10-14）：全局审批卡照常弹出并显示内容与倒计时，但不提供现场确认勾选与“批准并开始”，改为“去私密窗口审批”（`voiceApi.openPrivateSettings`）与“拒绝”（优先调 `voice_home_approval_cancel` 同步清理协调器内存 `pending`）；实际批准只在私密窗口的“家居控制审批”里进行（`approvedInPrivateWindow`）。本机 Agent 请求（`kind=agent_run`）角标显示“本机 Agent · 本次批准”并展示 CLI 类型、工作目录名、任务摘要与读写/计费提示（后端 `agent.permission="workspace_write"`）。私密窗口里家居卡的“开启 / 关闭家居语音控制”按钮可双向切换（开启前确认）；声纹卡新增“简易标定”区（每人“录测试音”、“计算阈值并自检”、“清除标定”，`speakerCalibrationView`），标定通过前“按人使用对话记忆”不可勾选。
- `SendOverlay` 新增可选属性 `onEmergencyStop`、`cancelLabel`、`badge`；不传时与原来完全一致（工作台发送不变）。
- 激光任务进度浮层 `components/JobProgressDock.jsx`（2026-11-02 用户决定，App 级挂在 `main.jsx` 的 `ApprovalCenter` 之后，不在任何页面内）：**不管谁发起的雕刻**（工作台、画板、内置语音、官方接入点）都能在任何页面看到进度，解决语音发起的任务原先只有顶栏急停知道、看不到进度与内容的问题。数据只读 `GET /api/jobs/stop-targets`（后端已补 `workflow_id` / `phase` / `lines_sent` / `total_lines` / `elapsed_ms` / `mode`），每 1 秒轮询一次，纯逻辑在 `jobProgressState.js`（`jobView` / `jobsView` / `jobPercent` / `jobRemainingText` / `jobPhaseText` / `dockPillText`，单元测试 `tests/jobProgressState.test.mjs`，浏览器测试 `tests/job-dock.browser.mjs`）。显示百分比、已发/总行数、用时、按比例外推的剩余时间（样本不足不显示）、设备与模式、任务标识；总行数未知或状态为 `unknown` 时明确显示“进度未知”，**不把旧百分比当成还在跑**，并提供「已确认停稳」按钮（调用 `POST /api/jobs/acknowledge-stop` 清除已现场核实的待确认停止记录）；轮询失败显示“读取任务进度失败，进度可能已过期”。动作分两个通道，都复用既有接口：`#job-progress-cancel-{i}` 走 `POST /api/workflow {action:"cancel", workflow_id}`（温和取消、记断点可续雕，兼容标定矩阵与活动走边框，需确认），`#job-progress-estop-{i}` 走 `POST /api/safe-action {action:"emergency_stop", confirmed:true, job_id}`（实时 0x18，危险确认，并注明请求成功不等于物理停机；急停按钮与胶囊急停不受 `busy` 禁用，取消 / 确认停稳请求挂起时仍可立即发出，2026-11-05）；缺少 `workflow_id` 时按钮说明只能急停，绝不猜任务。`#job-progress-gcode-{i}` 先取 `POST /api/workflow {action:"status"}` 的 `gcode_download_url` / `artifacts.gcode_file` 再下载（兼容标定矩阵），没有文件就提示而不是拼接假地址。可收成胶囊（`#job-progress-pill`，记忆在 `localStorage` 的 `moss_job_dock_minimized`），胶囊显示百分比或任务数并提供急停，展开/收起不影响顶栏急停；任务从运行列表消失后只说“可能已完成、已取消或已失败”，保留 5 秒后隐藏，不冒充雕刻成功。

## 9. 语音中心页 `/voice`（技术书 §9.5.1，P4-08）

- 页面 `pages/VoicePage.jsx`；纯逻辑在 `voiceState.js`（单元测试 `tests/voiceState.test.mjs`）；命令封装在 `voiceApi.js`；整页浏览器测试 `tests/voice-center.browser.mjs`（Vite 回环 + 有状态假 Tauri 桥：切换链路确认、服务端启停与意外退出、分组只提交本组字段、未保存标记跨标签保留、先测试后保存、窄屏无横向溢出；`--baseline` 重新生成 `tests/visual-baseline/voice-*.png`）。
- **只在桌面端出现**：同时满足“有 Tauri `invoke`、有桌面会话、不是手机远程模式”（`voiceCenterAvailable`）才显示左侧第四个标签“语音中心”（徽标 `VOICE`）并接受 `/voice` 路由；其他环境访问 `/voice` 回到工作台。
- 与后端通信全部经 Tauri 命令（`voice_status`、`voice_save_settings`、`voice_mode_changes`、`voice_set_mode`、`voice_server_start` / `stop` / `restart`、`voice_server_log`、`voice_models_test`、`voice_models_save`），不走 HTTP；审批记录读 `GET /v1/approvals?status=all`。页面每 4 秒刷新状态与审批记录。
- 链路单选：先取 `voice_mode_changes` 的改动项显示确认框，确认后才切换；切回官方且 MOSS 启动的服务端仍在运行时再询问是否停止。OTA 一致性提示与设备卡依据 `device.state`（`online` / `waiting` / `unknown`），无法判断时显示“未知”。
- 布局（n29，2026-10-18 / 2026-11-01 用户决定；UI-3 的后续整理）：顶部“语音链路”卡（三选一 + 一行状态胶囊：服务、设备、严格模式开关，设备提示与严格模式提示合并成一条）。标签由 `voiceTabsFor(mode)` 决定：**官方链路 4 个**（服务 / 设备与工具 / 扩展 / 记录与诊断），**自建链路 5 个**（多“模型”），**内置链路 6 个**（多“模型”与“隐私与家居”，因对话记忆、信息服务、声纹、家居四项均挂在 MOSS 内置语音会话上）；上次打开的标签记在 `localStorage` 的 `moss_voice_tab`，当前链路没有该标签时回退到第一个。**当前链路用不到的部分不渲染**——`ModeSplit` 只渲染 `modes` 命中当前链路的 part，删掉了原来的“其他链路的…设置”折叠按钮：官方链路不渲染内置服务面板与自建服务端面板，自建链路不渲染内置面板，本地知识库 / 本地音乐 / 视觉理解 / 隐私与家居只在内置链路渲染，MCP 与 Agent 三个链路都有。标签内容始终挂载、只用 `hidden` 隐藏，切换标签不丢填写。**缺前置配置也允许切换**（n30，2026-10-19）：官方链路下切到自建不会被“没填自建接入点”拦住，切过去后在「服务」标签填好保存即重连；服务端卡片显示“尚未填写自建接入点…”，地址格式不对仍然拒绝切换。
- 分组与保存（`components/VoiceLayout.jsx`）：`VoiceGroup` 折叠时显示一行摘要，展开状态按 id 记在 `foldState`（默认收起）；`SaveBar` 的按钮文字为“保存{对象}”，没改动或有阻止原因（如“先测试连接，通过后才能保存”）时不可点，保存结果（`savedNoteText`：对象、时间、何时生效；需要重启时附“立即重启服务端”）显示在按钮旁，失败也显示在按钮旁。分组通过 `useVoiceDirty` 上报未保存状态，分组标题显示“未保存”，所在标签显示提示点（`dirtyTabs`）。改动判断 `formDirty` 按去空白的字符串比较，缺省比较两边字段的并集（只出现在表单里的字段也算改动）；带令牌的字段填了才算改动（`settingsGroupDirty`）；第三方 MCP 表单只有内容与打开时不同才算未保存。按钮旁的阻止原因不受“其他操作进行中”影响（进行中只禁用按钮，不改原因文字）。折叠摘要一律反映已保存的值，不反映草稿。保存成功后若读回最新状态失败（视觉、Agent），仍显示已保存并附读取失败原因，按提交内容更新基线。
- 降噪（n27/n28，2026-10-18）：`#voice-strict-state` 由状态胶囊改成可点开关（`toggleStrict`），点开经 `showConfirm` 二次确认后调用 `voiceApi.strictSet`；**开启与关闭都立即生效**（命令同时更新 `strict_from_env` 快照与运行时值，并按返回的 `needs_restart` 兜底提示），自建链路 `locked` 时命令返回 `strict_mode_locked`、界面只提示不可关闭。自建服务端组的 Python 字段带「自动探测」（`voice_detect_python`，只查文件存在、不执行程序）并回填路径；`args` 收进该组「高级选项」折叠（`SETTINGS_GROUPS.selfHostedServer.advanced`）。家居「只读查询与同步」把“授权 + 手动同步”合成一个「允许小智查询家里的状态（只读）」（`allowReadOnlyQuery`：授权成功后自动同步一次），已授权时显示「重新同步」与「撤销查询权限」。固件入口从“服务与设备”移到“记录与诊断 → 设备维护（固件升级）”折叠组（`#voice-group-device-maintenance`）。
- 隐私与家居（n26，2026-10-18；n49/n50 合并为单层无感记忆面板）：第六个标签里是四个各自的下拉（`#voice-private-{memory,plugins,speaker,home}-item`），点开哪一项就在主页面原地挂载对应卡片（`VoiceMemoryCard` / `VoicePluginsCard` / `VoiceSpeakerCard` / `VoiceHomeCard`）；其中 `VoiceMemoryCard` 直接渲染单层 `VoiceContextCard`（不再包二级 `#voice-private-memory-advanced` 折叠），家居卡内的“家居审批”（`VoiceHomeApprovalCard`）仍为二级折叠、首次展开才挂载。收起状态不发任何私密命令。`PrivateSettingsEntry` 接收语音页的 `builtinStatus` 交给记忆卡。打包私密窗口 `voicePrivate.jsx` 与 `open_voice_private` 保留给固件入口（“记录与诊断 → 设备维护”的 `PrivateFirmwareEntry`）。来源与 ACL 变化见 [`voice-private-acl.md`](../integration/voice-private-acl.md) 的 n26 节。
- 服务与设备：内置链路显示内置服务（`BuiltinServicePanel`）；自建链路显示服务端（启动 / 停止 / 重启，外部运行不显示停止；意外退出显示退出码）及设备、健康检查；官方与自建链路显示小智接入点（MOSS 桥是否连接）。
- 自建服务端模型（模型标签，`ExternalModelGroup`，大模型 / 语音识别 / 语音合成各一组）：显示最终写入地址（与桌面端规则一致：大模型为基础地址，语音识别 / 合成补全为完整接口地址）；“保存{项}到服务端配置”只在该组最近一次测试成功且测试后未改动时可点（`canSaveSection`，比较内容指纹，指纹只在内存中）；全模块部署时不可保存。MOSS 不读回服务端配置，分组摘要只显示最近一次测试结果；保存后本组以刚提交的值为基线。密钥框为密码样式，不回填，保存后清空。
- 链路与服务端设置（连接标签，`SettingsGroup`）拆成四组，各自只提交本组字段（`SETTINGS_GROUPS` / `settingsGroupPayload`，后端按字段合并）：官方接入点（官方、内置链路）、内置服务端口（内置）、自建接入点与健康检查（自建）、自建服务端程序（目录、Python、启动参数、端口；自建）。带令牌的地址（自建接入点、健康检查、官方接入点）只显示脱敏后的当前值，输入框留空表示不改；保存一组后只重置这组的填写，其他组未保存的内容保留。保存结果的生效说明由 `settingsSaveEffect` 按后端返回的 `reconnected` 决定：只有后端真的重新发起了连接才写“正在重新连接”（是否连上看页面顶部状态），地址未变时写“没有重新连接”。
- 记录与诊断标签：审批记录（所有链路）；内置链路下「实时对话排障镜（仅内存）」（`LiveDebugTraces`，默认关闭不存文字，热开启不断连设备，展示最近 10 轮原始识别、0ms 同音纠偏、脱敏工具调用与播报回复，支持划词一键加入同音纠偏或热词）、最近对话耗时与内置服务事件；自建链路下服务端日志（`voice_server_log`，并用 `maskLogLine` 再遮挡一次）。
- `SettingsModal`：有语音中心时（桌面端），“小智 MCP 网关端点”一节只显示说明与“前往语音中心”按钮，“OpenRouter · Jev 判断服务”已统一收归语音中心「模型」标签而不再在右上角偏好设置重复渲染；没有语音中心的环境（浏览器访问独立 `moss-host`、手机）仍显示原输入框，`/api/settings` 的 `mcp_endpoint` 与 `jev_*` 字段保持兼容。
- `CloseConfirmModal`：选择“完全退出”且 MOSS 启动的语音服务端在运行时，显示“同时停止语音服务端”（默认勾选），结果作为 `exit_application` 的 `stopServer` 参数；记住了“退出”但服务端在运行时仍会弹窗。
- 内置语音服务 `components/VoiceBuiltinCard.jsx`（NV1 起；NV5 起配齐模型后为对话模式；r15 起拆为 `useBuiltinStatus`、`BuiltinServicePanel`、`BuiltinModelGroups`、`LocalModelsGroup`、`ExperienceGroup`、`LiveDebugTraces`、`TurnTimings`、`BuiltinEvents`、`PrivateSettingsEntry`，由语音中心页分别放进各标签；状态刷新失败时保留上次状态并提示，不卸载分组）：显示运行状态与模式胶囊（对话 / 回声测试，`builtinModeView`）、是否随 MOSS 自动启动、自动启动失败原因、两个端口与设备要填的 OTA 地址；启动 / 停止；设备列表（待允许 / 已允许 / 已撤销、在线标记、型号与固件、最近来访），“允许”前弹确认框说明需重启设备，“撤销”为危险确认；可改设备名；展开已允许设备时提供小智硬件在线快捷控制条（`builtinDeviceControlsView`，显示 `self.screen.get_info` 上报的屏幕分辨率胶囊如 `320×240 彩屏` / `128×64 OLED`，提供音量滑块与一键静音 `0%`、屏幕亮度滑块、深浅色主题切换、立创实战派等支持 `self.set_press_to_talk` 设备的「单击唤醒 / 长按说话（抗噪）」一键切换，以及自适应「投送雕刻预览 / 投送预览参数卡」按钮——彩屏按 `(width/2, height/2)` 推送 1:1 点对点加粗刀路 PNG + 底部参数栏并在确认窗口内 4s 无声续期、加工发送期间自动推送实时进度副屏 HUD、严格模式待电脑批准期间推送待审批警戒倒计时卡、加工结束/急停瞬间推送 5 秒自动淡出的庆祝/断点徽章卡、本地音乐播放期推送律动唱机/听歌小机器人卡，普通单色 OLED 官方固件自动推送材料/尺寸/功率速度滚动文字预览卡；偏好保存在 `builtin_device_prefs` 并在设备重连时自动同步恢复）；设备表下方提供 `#voice-goggle-bot-card`「小智彩屏动态卡片 & 护目镜小机器人 GIF」面板（零 Flash 分区擦写风险，支持 `screen_card_style` 在「硬核太空舱（`moss_hud`）」与「可爱像素小机器人（`goggle_bot`）」间不断连热切换，实时渲染 `idle / engrave / approval / music / celebrate` 五组像素护目镜小机器人循环 GIF89a，并支持 `push_bot_pose` 一键推到在线彩屏小智试看）；显示最近 20 条服务事件。每 4 秒刷新。纯逻辑 `builtinDeviceView`、`allowDeviceConfirmText` 在 `voiceState.js`。连接标签的“内置服务端口”组可改两个端口（`builtin_ws_port`、`builtin_http_port`，不能相同）。模型标签“内置服务 · 云端模型”（NV5 / n35 / n43）：顶部按真实语音执行链路从左到右排列五栏下拉选择框——**`1. 拾音判停` (`vad`) → `2. 语音识别` (`asr`) → `3. Jev 前置粗筛` (`jev`，识别后·进大模型前) → `4. 大模型` (`llm`) → `5. 语音合成` (`tts`)**（`#voice-builtin-mix-bar`，`#voice-builtin-mix-select-vad/asr/jev/llm/tts`，`MIX_SECTIONS` / `builtinSectionSourceView` / `builtinSourceSwitchPayload`），用于直接下拉选择每项当前使用云端、内置能量判停、具体本地模型，或直接关闭 / 切换 `typesafe/jev-1.14` / `1.13` 决策模型（选完立即调 `voice_local_save` 生效，支持任意混搭；切换为本地模型或关闭 Jev 时原样保留云端地址与本机密钥作备用；选到未下载的本地模型或未配密钥的 Jev 项时自动展开并滚动至下方对应卡片）；下方配置卡片同样按链路顺序排列：`语音识别` → `#voice-group-builtin-model-jev`「OpenRouter · Jev 前置粗筛与反馈模型」 → `大模型` → `语音合成`（Jev 卡片为桌面端唯一 Jev 配置入口，读写 `app_secrets["jev_api_key"]` 与顶层 `jev_enabled`，若大模型已配置 `openrouter.ai` 且留空密钥则自动复用大模型密钥，支持下拉选取或手填 `typesafe/jev-*`、独立开关内置语音筛选与加工反馈判断、Decisions `noul` 连接测试与清除密钥，全程不回显密钥）；各卡片独立测试与保存（`localBusy` 互不阻塞），与外部服务端向导相同的“先测试、后保存”（`canSaveSection` + 指纹）；表单用已保存设置预填（`builtinModelForm`，不含密钥），密钥框只显示“已设置 / 未设置”（`builtinKeyPlaceholder`），留空即沿用，且当其他支持音频的 OpenAI 兼容分区已有地址时提供「一键复用{分区}地址与密钥」按钮（`findPeerCloudSource`、`#voice-builtin-reuse-peer-*`，自动排除 Cline / OpenRouter / DeepSeek 等仅对话网关，后端自动复用同源已存密钥且不回显）；模型栏为可输入筛选与下拉选取的组合框（填妥地址与密钥或沿用已存密钥时经 `voice_builtin_models_list` 读取服务商全量模型列表与原生类型标签 `model_sections` 并在 `REMOTE_MODELS_CACHE` 缓存复用，结合 `classifyModelSections` 自动识别每个模型可用于大模型、语音识别还是语音合成——支持 2~3 项兼具的多模态模型同时出现在各对应分区——默认筛出当前分区可用模型并标注用途标签，0 匹配时不把纯文本大模型冒充语音模型列出，同时提供“显示全部 / 只看本类”切换、多词子串查询及直接手填兜底；豆包流式直接列出常用资源 ID，`cline-pass/*` 标注“ClinePass 订阅”、`cline-free/*` 标注“Cline 免费”）；语音合成选定模型后若音色为空则按模型家族自动填入默认音色（`applyDefaultTtsVoice`）并提供常用音色下拉建议（`ttsVoiceOptionsFor`、`#voice-builtin-voice-options-tts`），测试连接通过后若有返回音频则在结果旁显示「试听语音」按钮（`#voice-builtin-model-play-tts`）；“删除此项”需确认并同时删除密钥。识别、合成两栏有“接口类型”（NV7：OpenAI 兼容 / 豆包流式，`switchKind` 切换时填入或清掉豆包默认地址与资源 ID；豆包时标签改为 API Key、资源 ID、音色 ID，语速 0.5–2.0；接口类型参与指纹）。语音链路卡片新增第三项“MOSS 内置服务”（`builtin`）；顶栏小智胶囊在内置链路下只看内置服务上报的设备会话数（`mcpPillState`：未运行为“未连接”、0 为“等待设备”）。 本地模型（下载管理、音色、语速、下载地址；当前启用哪个模型由顶部下拉框统一控制）与语音体验（助手语气风格预设与工坊补充说明、小智彩屏/单色 OLED 预览与加工副屏自动同步、小智彩屏卡片画风切换、快捷查询、激光同音错词 0ms 纠偏、热词、插话）共用 `voice_local_save`，但分两组保存：每组只取本组草稿（`LOCAL_MODEL_KEYS` / `EXPERIENCE_KEYS`，`mergeGroup`），另一组字段用已保存的值，不会顺带提交另一组未保存的改动；每组也只检查本组字段（`localModelSettingsError` / `experienceSettingsError`），另一组已保存的值不挡本组保存，由后端最终校验；模型下载、取消、删除立即执行。
- 接入点与工具 `components/VoiceEndpointsCard.jsx`（NV8，设备与工具标签，官方/自建链路渲染“接入点状态”与“MOSS 内置工具”两组，内置链路不渲染“接入点状态”、只保留“MOSS 内置工具”，勾选、添加、删除立即生效）：接入点表格（主接入点、附加接入点 N；状态胶囊 已连接 / 连接中 / 重连中 / 已暂停，`endpointView` 附自动重连次数、上次断开原因、对方客户端；`identityView` 显示 token 中的标识字段与有效期，过期标红；附加接入点可删除，需确认）；添加附加接入点输入框（`extraEndpointError` 先在页面校验，提交后清空，地址只在内存中）；内置工具表（复选框提交整份关闭名单 `toolSwitchPayload`，停止与急停工具不可点；`toolStatText` 显示本次运行以来的调用、失败、拒绝次数）。**内置工具与第三方 MCP 在界面上明确区分**（2026-11-02 用户决定）：组标题写“MOSS 内置工具（只能开关，不能在这里增删）”，并说明与「扩展 → 第三方 MCP 工具」不是一回事（前者编译在 MOSS 内、只能开关，后者是外部独立程序、可添加与移除）；组底部有折叠的 `#voice-tool-prompts`，内含“新增内置工具”“删除或下线内置工具”两段可复制提示词（`TOOL_EDIT_PROMPTS` / `toolEditPrompt`，`#voice-tool-prompt-copy-add` / `-remove`，复制成功显示“✓ 已复制”，剪贴板不可用时提示手动选中），内容给出真实改代码路径（`crates/laser-tools/src/lib.rs` 名单、`tests/golden/mcp_tools.json`、`crates/laser-mcp/src/lib.rs` 的 `tools_list_with` / `dispatch_tool_call_with`、`crates/laser-contracts/src/risk.rs` 的 `tool_risk`、`crates/moss-voice-server/src/jev.rs` 的 `groups_of_builtin_tool` 粗筛归组）与门禁约束（审批与严格模式不得放宽、急停工具不许删、禁止为单工具平铺 `noul`）；模板只用于复制，MOSS 不执行。数据来自 `voice_status.mcp`，随状态每 4 秒刷新。
- 无感记忆卡片 `components/VoiceContextCard.jsx` 与 `components/VoiceMemoryCard.jsx`（NV19-08；2026-11-04 n50 用户决定合并精简；挂在主语音页“隐私与家居 → 记忆”与打包私密页 `voicePrivate.jsx`）：纯逻辑在 `voiceContextState.js`（单元测试 `tests/voiceContextState.test.mjs`），浏览器测试 `tests/voice-context.browser.mjs`。所有操作经 `voice_context_manage`（支持 `status`、`set_settings`、`list_all`、`quick_create`、`update`、`delete`、`clear_all`、`revert_extraction` 等）；界面收敛为零配置三要素：① 唯一总开关 `#context-memory-switch`「让小智记住我的习惯」（默认关闭，首次开启经二次确认同步联动检索、提取与旧会话摘要开关，并自动无感迁入旧设备摘要）；② 一句话快捷添加栏 `#context-quick-input` + `#context-quick-add`（回车或点添加直接落库并立即生效，失败保留草稿）；③ 单层可编辑/可删除记忆列表（超过 5 条时自动显示关键词即时过滤框 `#context-filter-query`，支持行内修改、单条删除、一键清空全部及有近期自动提取记录时的一键撤销）。
- 本地知识库卡片 `components/VoiceKnowledgeCard.jsx`（2026-11-04 n50 新增；挂在内置语音链路“扩展”标签的 `#voice-group-knowledge`）：独立于私人记忆总开关，经 `voice_context_pick_folder` 直接将任意本机文件夹挂载至 `workspace:local-kb` 范围（无需预先设置项目名称），展示目录名称、文件数与索引时间，支持按目录勾选启用/暂停（`knowledge_toggle`）、重建索引（`knowledge_reindex`）与移除（`knowledge_remove`，不删磁盘原文件）。

## 10. 数字孪生舱 `/twin`

- 页面 `pages/TwinPage.jsx`，三维机台视口 `components/TwinStage3D.jsx`，纯逻辑在 `src/twinState.js`（单元测试 `tests/twinState.test.mjs`）；左侧导航项 `数字孪生 (TWIN)`，雕刻工作台「加工监视舱」卡片内另有快捷跳转入口。
- **Three.js 3D 翼宿 V1.0 真实机台、打印件总成与材料/坐标联动**：严格按《大鱼 DIY 翼宿 V1.0 图纸包》全套 STEP / DXF 建模，包含 `193×222×6mm` PETG 底板、`121.9×121.9×4.5mm` 排烟槽切割垫板、默认 `100×100mm` 可调有效雕刻工作区（支持 `100×100mm` 满行程 / `90×90mm` 安全区一键切换与 `20–100mm` 行程微调）、`3×140mm MGN9H` 线性导轨、后置双 `42` 步进电机与 `Ø8×186mm` 联动光轴、前置 Y 轴惰轮调节块与星形手拧螺帽、后置 `4028` 排烟风扇与 `G3` 主板盖、可开关的 `3mm` 半透防激光亚克力护罩（顶部观察板 `186.8×213mm` + 前遮光板 `158×107mm`），以及由《X轴连接件》《激光头转接板_5W》（含双侧竖向调焦滑槽与 M3 螺丝）《同步带卡齿》《线束固定器》与 `24V 5W` 散热鳍片激光头组成的移动刀头总成。3D 视口底部边框提供上下拖拽调节手柄（`.twin-stage-resize-handle`，支持 `220px ~ 960px` 放大/缩小 3D 工作台高度，拖拽过程同步更新 Three.js 渲染缓冲区并立即重绘，且在高度小于 `520px` 时对垂直 `fov` 做平滑补偿，双击恢复默认 `520px`，高度存 `localStorage` 键 `moss_yisu_stage_height_v1`）；左上角提供「收起浮层 / 展开浮层」切换按钮（状态存 `localStorage` 键 `moss_yisu_hud_collapsed_v1`），收起后隐藏顶部坐标/视角工具条与底部图例标签，仅保留左上角胶囊入口，避免遮挡 3D 工作台。3D 材料板材默认 `80×80mm`（并支持一键 `1:1 同步刀路尺寸`、`外扩 +10mm`、`铺满台面` 及通过数字输入框 + 拖动滑块自由调节外框宽高 `W/H` 与台面起点坐标 `X0/Y0`）；调节 `W/H` 时刀路偏移与比例（`toolpathScaleX / toolpathScaleY`）自动联动，同时提供「激光雕刻文字在材料内位置（始终在材料内）」调节区，支持调整文字/刀路相对于材料左下角的横向/纵向偏移（`textOffsetX / textOffsetY` 输入框 + 滑块 + `材料左下 / 材料内居中 / 材料左上 / 材料右上 / 材料右下` 快捷对齐）。`resolveMachineWorkspace` 将文字在材料内的坐标严格钳制在 `[minTextOffsetX, materialWidthMm - effectiveContentMaxX]` 与 `[minTextOffsetY, materialHeightMm - effectiveContentMaxY]` 范围内，确保无论如何调整文字位置或缩放材料尺寸，雕刻文字始终 100% 处于材料边界内部，且 3D 刀路、内包络框与 5W 激光头实时跟随移动。
- **三种信号源驱动模式（`resolveTwinHeadPose`）**：
  1. `离线刀路推演 (sim)`：解析当前/最近 workflow 或本地导入 `.gcode/.nc` 的 G-code（`parseGcodeToolpath`），按每段运动长度与实际进给速度 `F`（`mm/min`）推算物理加工总时长（`estimatedSeconds = cutSeconds + rapidSeconds`），并在回放与进度条拖拽（`resolvePoseAtProgress`）中按各段真实物理耗时 `cumulativeSeconds` 严格 **1:1 实时同步推演**（不进行人工倍速缩放），支持播放/暂停、重置、按段单步与循环（「循环」默认关闭 `simLoop = false`，按钮 `#btn-twin-loop` 显示 `循环：开启 / 循环：关闭` 并在紧邻右侧通过 `#twin-loop-status-pill` 状态胶囊展示 `已开启循环雕刻 / 单次推演（不循环）`，未开启时推演到 `100%` 立即停止并不再循环雕刻）；底部刀路推演控制、进度条与实时 G-code 指令栏支持一键「收起 / 展开」（状态按 `twin-timeline` 存入 `foldState`），收起后仅保留单行紧凑进度摘要（含循环状态）与「展开」入口。
  2. `加工下发同步 (job)`：轮询 `/api/workflow {action:"status"}`，按 `send_result.lines_sent` 实时定位已加工轨迹（`resolvePoseAtLine`），并自动消费流式发送/脱机监控期间固件 `$RI=200` 或 `?` 回传的 `send_result.last_status_report` 实时 `<Run|MPos|WPos|FS>` 报文，融合驱动 3D 激光头物理坐标、功率与遥测面板。
  3. `实机遥测映射 (telemetry)`：空闲或点动时通过 `/api/safe-action {action:"status"}` 只读采样控制器 `<State|MPos|WPos|Bf|FS|Pn|Ov|SD>` 报文（`parseGrblStatusReport`），任务运行期间同步复用 `send_result.last_status_report`（免抢占独占串口），展示机械/工作坐标、规划器与串口缓冲余量、限位/门开/急停引脚状态；支持手动调节工作区范围或通过 `/api/connection/check` 同步实机 `$CD` / GRBL 物理行程并自动做刀路越界预警。
- **安全边界**：离线推演与本地 G-code 导入不触碰硬件；遥测采样仅发送只读 `status`（`?`）与 `connection/check`，不触发运动或出光；页面常驻 `0x18` 急停按钮。
