# 前端行为契约

记录有代码或测试固化、改动时必须保留的前端行为，按当前 `frontend/src` 核对。改这些行为前先改本文与对应测试。

## 1. 路由与 API 基址

| 路径 | 页面 | 主要调用 |
| --- | --- | --- |
| `/` | `HomePage` | `/api/ui/runtime-config`、`/api/ui/serial-ports`、`POST /api/ui/discover-controllers`、`/api/ui/material-options`、`/api/connection/check`、`/api/text-task/generate`、`/api/upload`、`/api/workflow`、`/api/system/info` |
| `/draw` | `DrawPage` | `/api/draw/preview`、`/api/connection/check`、`/api/device/framing`、`/api/workflow` |
| `/material-lab` | `MaterialLabPage` | `/v1/material-lab/bootstrap`（首屏一次）、`/api/material-lab/manage` / `recent` / `export` / `preview` / `send` / `status` / `select-cell` |

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
| `moss_close_action` | 全局 | 桌面端关闭行为（`ask` / `tray` / `exit`） |

## 6. 其他必须保留的行为

- 主页文字输入上限 24 字（`maxLength={24}`）。
- 串口下拉只显示 `/api/ui/serial-ports` 返回的真实串口，无结果时提示并允许手动输入；不再提供硬编码的 COM 预设。
- 材料下拉来自 `/api/ui/material-options`，只暴露材料名与厚度，不暴露功率/速度。
- 材料实验室保存成功后通过 `BroadcastChannel`（`laser-material-library-v1`）通知其他已打开页面刷新。
- 多厚度保存由后端单次 `/api/material-lab/manage` 原子完成，前端不循环发送 N 次请求。
- Excalidraw 主题变量覆盖 `--color-primary`（`styles.css`），语言 `zh-CN`。
- 桌面端关闭窗口时先弹出"最小化到托盘 / 退出"选择；选择可被记住到 `moss_close_action`。非 Tauri 环境下"退出"只关闭页面并显示会话终止提示。
- 关闭窗口退出与托盘“退出程序”走同一收尾 `shutdown_and_exit`：先急停，再停止（或放开）语音服务端、内置语音服务与第三方 MCP 子进程，最后退出（2026-10-14 起托盘退出不再漏掉后两项）。
- 画板设备卡（2026-10-14）：连接方式可选网络（Telnet）或串口（`/api/ui/serial-ports` 刷新、可选波特率），发送、走边框、测试连接都用所选方式（`drawConnection.js`）；串口时确认框不显示 SD 脱机选项。宽 / 高留空表示按导出尺寸，填写后后端按填的尺寸出成品；生成策略只有光栅与轮廓（轮廓沿深色图形描边）。“走边框”默认不出光，勾选“走边框时弱光指示”才传 `power:1`；走边框期间顶栏急停能找到并停止它。
- 顶栏“硬件”胶囊显示最近一次握手探测的结果（`state/hardwareStatus.js`：未探测 / 握手通过 / 未连接），提示里写探测时间，不冒充实时在线；探测在雕刻工作台进行。控制台“AI 工具路由”卡中的发送门禁、急停方式、核心调度工具标“固定规则 / 固定”，是设计说明，不是检测结果。
- 视觉基线：`tests/visual-baseline/` 保存三个页面桌面与移动视口截图，用于人工核对布局，不做像素比对。

## 7. 偏好设置“导出”分区与导出流程（技术书 §9.7）

- 所有导出（画板 PNG / 场景 JSON、预览 G-code / 场景下载、材料库导出）都经 `frontend/src/saveFile.js` 的 `exportFile`；不得在组件里直接用 `<a download>`。
- Tauri 中调用宿主命令 `save_export_file`；返回 `null` 表示用户在“另存为”框中取消，提示“已取消导出”，不当作错误。
- 宿主返回 `export_dir_unavailable:<路径>|<原因>` 时弹出两个动作：“本次保存到下载文件夹”（带 `fallbackDefault: true` 只重试一次）与“重新选择”（调用 `pick_export_dir` 后重试；选择框取消则放弃）。关闭弹窗视为放弃。不得静默改存其他目录。
- 成功后弹出“导出完成”，可“打开所在文件夹”（`reveal_exported_file`）。
- “导出”分区只在桌面版 Tauri 显示（`shouldShowExportSettings`）；Android 与浏览器不显示。目录只能经 `pick_export_dir` 的系统选择框修改，没有路径输入框。
- 偏好设置各分区与工作台卡片可折叠，状态存 `localStorage` 键 `moss_ui_fold_<id>`（值 `open` / `closed`），读写失败时按默认值显示。

## 8. 全局审批卡（技术书 §9.5.2，P1-09）

2026-10-10 修补：审批与工作台覆盖层在通信未知或任务记录终态时仍保留急停，直到用户关闭该覆盖层。停止/取消的请求回复不直接将本地状态改成 cancelled，继续查询并明确提示物理停止未确认；急停绑定当前任务编号，旧请求返回不能修改后续任务。

- 组件 `components/ApprovalCenter.jsx`，挂在 `main.jsx` 的 `App` 根组件，与 `AuthModal`、`CloseConfirmModal` 同级，任何页面都会弹出。纯逻辑在 `approvalState.js`（单元测试 `tests/approvalState.test.mjs`）。
- **只在桌面端工作**：没有桌面会话（浏览器、Android、手机连接电脑的远程模式）时不轮询、不显示，顶栏也不显示“审批”按钮。
- 每 4 秒请求 `GET /v1/approvals?status=pending`；出现新的请求时自动弹出（批准后的发送覆盖层显示期间不打断）。多个请求按创建时间先后处理，卡片顶部提示“还有 N 个请求等待处理”。
- 卡片内容：刀路预览（加工任务读取 workflow 记录的 `image_file` / `preview_file`）、内容、材料与厚度、功率（带标尺）/ 速度 / 遍数、尺寸与预计时长、目标设备（只显示连接方式与串口名，网络设备显示“网络设备”）、来源、摘要前后各 4 位、倒计时条（按服务端时间计算）。记录里没有的字段显示“—”，不编造。
- 交互：必须勾选“我在机器旁，材料已放好、原点已确认，排风已开启”才能点“批准并开始”；批准时提交卡片上的 `digest`。`approval_mismatch` / `approval_expired` 等失败在卡内显示原因，按钮收起为“关闭”，不发送任何指令。倒计时归零自动关闭；“拒绝”立即生效；“稍后处理”只关闭卡片，请求仍在等待，可从顶栏“审批”重新打开。
- 批准后：加工任务与标定矩阵进入 `SendOverlay`（`STAGE_MAP` 四步不变，标题旁标“桌面审批”），全程显示“取消后续发送”（标定矩阵为“停止”）与“急停 0x18”，并注明 0x18 不代替控制器硬件急停；设备动作与走边框执行后弹出结果提示。
- 顶栏“审批”按钮：没有请求为灰色，有请求变红并显示数量；没有请求时点击提示“当前没有待批准的请求”。
- 家居控制请求（`kind=home_control`，2026-10-14）：全局审批卡照常弹出并显示内容与倒计时，但不提供现场确认勾选与“批准并开始”，改为“去私密窗口审批”（`voiceApi.openPrivateSettings`）与“拒绝”；实际批准只在私密窗口的“家居控制审批”里进行（`approvedInPrivateWindow`）。私密窗口里家居卡的“开启 / 关闭家居语音控制”按钮可双向切换（开启前确认）；声纹卡新增“简易标定”区（每人“录测试音”、“计算阈值并自检”、“清除标定”，`speakerCalibrationView`），标定通过前“按人使用对话记忆”不可勾选。
- `SendOverlay` 新增可选属性 `onEmergencyStop`、`cancelLabel`、`badge`；不传时与原来完全一致（工作台发送不变）。

## 9. 语音中心页 `/voice`（技术书 §9.5.1，P4-08）

- 页面 `pages/VoicePage.jsx`；纯逻辑在 `voiceState.js`（单元测试 `tests/voiceState.test.mjs`）；命令封装在 `voiceApi.js`；整页浏览器测试 `tests/voice-center.browser.mjs`（Vite 回环 + 有状态假 Tauri 桥：切换链路确认、服务端启停与意外退出、分组只提交本组字段、未保存标记跨标签保留、先测试后保存、窄屏无横向溢出；`--baseline` 重新生成 `tests/visual-baseline/voice-*.png`）。
- **只在桌面端出现**：同时满足“有 Tauri `invoke`、有桌面会话、不是手机远程模式”（`voiceCenterAvailable`）才显示左侧第四个标签“语音中心”（徽标 `VOICE`）并接受 `/voice` 路由；其他环境访问 `/voice` 回到工作台。
- 与后端通信全部经 Tauri 命令（`voice_status`、`voice_save_settings`、`voice_mode_changes`、`voice_set_mode`、`voice_server_start` / `stop` / `restart`、`voice_server_log`、`voice_models_test`、`voice_models_save`），不走 HTTP；审批记录读 `GET /v1/approvals?status=all`。页面每 4 秒刷新状态与审批记录。
- 链路单选：先取 `voice_mode_changes` 的改动项显示确认框，确认后才切换；切回官方且 MOSS 启动的服务端仍在运行时再询问是否停止。OTA 一致性提示与设备卡依据 `device.state`（`online` / `waiting` / `unknown`），无法判断时显示“未知”。
- 布局（UI-3，技术书 §9.5.1 r15）：顶部“语音链路”卡（三选一 + 一行状态胶囊：服务、设备、严格模式 `#voice-strict-state`，及 OTA 一致性提示）；下面五个标签 `VOICE_TABS`（服务与设备 / 模型 / 连接 / 功能扩展 / 记录与诊断），上次打开的标签记在 `localStorage` 的 `moss_voice_tab`。标签内容始终挂载、只用 `hidden` 隐藏。每个标签用 `ModeSplit` 按链路摆放：当前链路用得到的部分在前，其他链路的部分排在后面、默认折叠（“其他链路的…设置”按钮展开，状态记在 `foldState`）；所有部分在同一父元素下按固定顺序渲染、只用 CSS `order` 调位置，切换链路不重建组件、不丢填写。
- 分组与保存（`components/VoiceLayout.jsx`）：`VoiceGroup` 折叠时显示一行摘要，展开状态按 id 记在 `foldState`（默认收起）；`SaveBar` 的按钮文字为“保存{对象}”，没改动或有阻止原因（如“先测试连接，通过后才能保存”）时不可点，保存结果（`savedNoteText`：对象、时间、何时生效；需要重启时附“立即重启服务端”）显示在按钮旁，失败也显示在按钮旁。分组通过 `useVoiceDirty` 上报未保存状态，分组标题显示“未保存”，所在标签显示提示点（`dirtyTabs`）。改动判断 `formDirty` 按去空白的字符串比较，缺省比较两边字段的并集（只出现在表单里的字段也算改动）；带令牌的字段填了才算改动（`settingsGroupDirty`）；第三方 MCP 表单只有内容与打开时不同才算未保存。按钮旁的阻止原因不受“其他操作进行中”影响（进行中只禁用按钮，不改原因文字）。折叠摘要一律反映已保存的值，不反映草稿。保存成功后若读回最新状态失败（视觉、Agent），仍显示已保存并附读取失败原因，按提交内容更新基线。
- 服务与设备：内置链路显示内置服务（`BuiltinServicePanel`）；自建链路显示服务端（启动 / 停止 / 重启，外部运行不显示停止；意外退出显示退出码）及设备、健康检查；官方与自建链路显示小智接入点（MOSS 桥是否连接）。
- 自建服务端模型（模型标签，`ExternalModelGroup`，大模型 / 语音识别 / 语音合成各一组）：显示最终写入地址（与桌面端规则一致：大模型为基础地址，语音识别 / 合成补全为完整接口地址）；“保存{项}到服务端配置”只在该组最近一次测试成功且测试后未改动时可点（`canSaveSection`，比较内容指纹，指纹只在内存中）；全模块部署时不可保存。MOSS 不读回服务端配置，分组摘要只显示最近一次测试结果；保存后本组以刚提交的值为基线。密钥框为密码样式，不回填，保存后清空。
- 链路与服务端设置（连接标签，`SettingsGroup`）拆成四组，各自只提交本组字段（`SETTINGS_GROUPS` / `settingsGroupPayload`，后端按字段合并）：官方接入点（官方、内置链路）、内置服务端口（内置）、自建接入点与健康检查（自建）、自建服务端程序（目录、Python、启动参数、端口；自建）。带令牌的地址（自建接入点、健康检查、官方接入点）只显示脱敏后的当前值，输入框留空表示不改；保存一组后只重置这组的填写，其他组未保存的内容保留。保存结果的生效说明由 `settingsSaveEffect` 按后端返回的 `reconnected` 决定：只有后端真的重新发起了连接才写“正在重新连接”（是否连上看页面顶部状态），地址未变时写“没有重新连接”。
- 记录与诊断标签：审批记录（所有链路）；内置链路下最近对话耗时与内置服务事件；自建链路下服务端日志（`voice_server_log`，并用 `maskLogLine` 再遮挡一次）。
- `SettingsModal`：有语音中心时，“小智 MCP 网关端点”一节只显示说明与“前往语音中心”按钮；没有语音中心的环境（浏览器访问独立 `moss-host`、手机）仍显示原输入框，`/api/settings` 的 `mcp_endpoint` 字段保持兼容。
- `CloseConfirmModal`：选择“完全退出”且 MOSS 启动的语音服务端在运行时，显示“同时停止语音服务端”（默认勾选），结果作为 `exit_application` 的 `stopServer` 参数；记住了“退出”但服务端在运行时仍会弹窗。
- 内置语音服务 `components/VoiceBuiltinCard.jsx`（NV1 起；NV5 起配齐模型后为对话模式；r15 起拆为 `useBuiltinStatus`、`BuiltinServicePanel`、`BuiltinModelGroups`、`LocalModelsGroup`、`ExperienceGroup`、`TurnTimings`、`BuiltinEvents`、`PrivateSettingsEntry`，由语音中心页分别放进各标签；状态刷新失败时保留上次状态并提示，不卸载分组）：显示运行状态与模式胶囊（对话 / 回声测试，`builtinModeView`）、是否随 MOSS 自动启动、自动启动失败原因、两个端口与设备要填的 OTA 地址；启动 / 停止；设备列表（待允许 / 已允许 / 已撤销、在线标记、型号与固件、最近来访），“允许”前弹确认框说明需重启设备，“撤销”为危险确认；可改设备名；显示最近 20 条服务事件。每 4 秒刷新。纯逻辑 `builtinDeviceView`、`allowDeviceConfirmText` 在 `voiceState.js`。连接标签的“内置服务端口”组可改两个端口（`builtin_ws_port`、`builtin_http_port`，不能相同）。模型标签“内置服务 · 云端模型”（NV5）：大模型 / 语音识别 / 语音合成各一组、各自保存，与外部服务端向导相同的“先测试、后保存”（`canSaveSection` + 指纹）；表单用已保存设置预填（`builtinModelForm`，不含密钥），密钥框只显示“已设置 / 未设置”（`builtinKeyPlaceholder`），留空即沿用；“删除此项”需确认并同时删除密钥。识别、合成两栏有“接口类型”（NV7：OpenAI 兼容 / 豆包流式，`switchKind` 切换时填入或清掉豆包默认地址与资源 ID；豆包时标签改为 API Key、资源 ID、音色 ID，语速 0.5–2.0；接口类型参与指纹）。语音链路卡片新增第三项“MOSS 内置服务”（`builtin`）；顶栏小智胶囊在内置链路下只看内置服务上报的设备会话数（`mcpPillState`：未运行为“未连接”、0 为“等待设备”）。 本地模型（开关、选用、音色、语速、下载地址）与语音体验（快捷查询、热词、插话）共用 `voice_local_save`，但分两组保存：每组只取本组草稿（`LOCAL_MODEL_KEYS` / `EXPERIENCE_KEYS`，`mergeGroup`），另一组字段用已保存的值，不会顺带提交另一组未保存的改动；每组也只检查本组字段（`localModelSettingsError` / `experienceSettingsError`），另一组已保存的值不挡本组保存，由后端最终校验；模型下载、取消、删除立即执行。
- 接入点与工具 `components/VoiceEndpointsCard.jsx`（NV8，连接标签，分“接入点状态”“工具开关与调用统计”两组，勾选、添加、删除立即生效）：接入点表格（主接入点、附加接入点 N；状态胶囊 已连接 / 连接中 / 重连中 / 已暂停，`endpointView` 附自动重连次数、上次断开原因、对方客户端；`identityView` 显示 token 中的标识字段与有效期，过期标红；附加接入点可删除，需确认）；添加附加接入点输入框（`extraEndpointError` 先在页面校验，提交后清空，地址只在内存中）；工具开关与调用统计表（复选框提交整份关闭名单 `toolSwitchPayload`，停止与急停工具不可点；`toolStatText` 显示本次运行以来的调用、失败、拒绝次数）。数据来自 `voice_status.mcp`，随状态每 4 秒刷新。
- 长期记忆私有卡片 `components/VoiceContextCard.jsx`（NV19-08，只挂在 `voicePrivate.jsx` 的本机安全窗口，主窗口不引用）：纯逻辑在 `voiceContextState.js`（单元测试 `tests/voiceContextState.test.mjs`），浏览器测试 `tests/voice-context.browser.mjs`。所有操作经 `voice_context_manage`（按 `action` 分发）与 `voice_context_pick_folder`；分开关、记忆、资料目录、故障经验、恢复摘要五块，各块结果只显示在本块，失败保留填写。检索与自动提取两个开关默认关闭，开启提取、深度检索、作废、删除、清空、撤销、移除资料前都先弹确认；查找结果不被随后的刷新覆盖；恢复摘要再次保存时带上版本。
