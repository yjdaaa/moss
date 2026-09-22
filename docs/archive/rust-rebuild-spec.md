> **历史归档（只读）**：本文是 Python 版到 Rust 版重构期间的文档，内容截止于文中日期，不再维护。第一部分需求与 §22 决定仍是设计依据；第二部分技术方案未按稿实现，实际架构与差异清单见 [架构总览](../developer/architecture.md)。 当前状态以代码、`CHANGELOG.md` 与 [`docs/developer/`](../developer/) 为准；归档说明见 [README](README.md)。

# MOSS Xiaozhi MCP Rust 重构：需求与技术实施（合并版）

| 项目 | 状态 |
|---|---|
| 文档状态 | 历史规格：需求已实施完成；技术方案部分未按稿实现（见归档横幅） |
| 确认日期 | 2026-09-14 |
| 源项目（只读） | `<python-source-root>`（本机路径已移除） |
| 目标项目 | `<rust-target-root>`（本机路径已移除） |

本文件是本次重构唯一有效的需求与实施基线，取代此前的 `docs/requirements.md`、`docs/technical-implementation.md` 与 `docs/rust-migration/` 下全部草案。第一部分讲"做什么"，第二部分讲"怎么做"，第三部分是已确认决定与授权边界。

---

# 第一部分：需求

## 1. 目标

以源项目当前工作区为功能事实基线，用 Rust 重建全部后端，配新的 React/TypeScript 前端、Tauri 桌面端和 Android/iOS 客户端。必须达到：

1. 现有全部功能在新项目中有明确归属并完成迁移。
2. 小智/MOSS 直接连接 Rust MCP 服务，不再有 Python 子进程桥接。
3. Windows 用户无需安装 Python、venv 或 pip 依赖。
4. 本期冻结 Python 实际注册集合的 MCP 工具机器契约与 17 个 `/api/*` 路由兼容。当前源代码默认实际注册 20 个工具（包括 `command_execution_tool`）；`homeasstant_tool`、`lasergrbl_gui_tool` 不注册，摄像头工具仅在 `ENABLED_IP_CAMERA=true` 时注册。开发者模式是否另设，必须先有源 Python 开关和黄金样本证据，不能按文档臆定。小智/MOSS 直接连接 Rust MCP。控制台提示词以及工具、参数的自然语言描述允许根据 Rust 工程的实际能力和行为调整，但不得与工具 schema、安全门禁、确认流程或已约定的用户可见行为矛盾。
5. 激光安全门禁、材料限制、确认流程和设备探测不得弱化。
6. Windows 是首要且阻断验收的平台；Raspberry Pi 以 CI 交叉编译产物验收；Android 与 iOS 源码和工程在范围内，本机缺 SDK/NDK/JDK 与 macOS，安装验收一律标 `partial`。
7. 旧运行数据非破坏性迁移到新的 SQLite 存储，材料参数、任务状态和历史记录继续可用。

## 2. 已核实的现状

（原表记录了 2026-09-14 时源 Python 工作区与本机环境的统计数据，含远端仓库与本机路径，归档时已移除。当时的关键事实：Python 版默认注册 20 个 MCP 工具、17 个 `/api` 路由、默认 `127.0.0.1:8766`；桥接为每帧一条 JSON-RPC、1s 起 60s 封顶指数退避；`.runtime/` 约 47MB 历史数据。）


## 3. 文件边界

- 源项目整个过程只读：不增删改任何文件，不改 Git 状态；可只读运行测试和生成样本，产物写目标目录或临时目录。
- 新工程只写入目标目录。迁移期间保留旧 Python 与旧前端作对照。
- 全部验收完成、用户再次确认精确清单后，才删除旧内容；`.git`、新数据、用户配置、仍在用的视觉资源不得顺手删。
- 不自动 commit、push、发布或清理 Git 历史；现有 1030 项暂存改动原样保留。

## 4. 功能范围

### 4.1 核心计算与 G-code

覆盖：文字、图片、已有 G-code/NC 输入；解码、灰度、缩放、裁切、阈值；栅格、抖动、轮廓追踪、矢量填充、路径优化；中文字体字形渲染与轮廓；雕刻、轮廓、切割 G-code/NC；尺寸适配、边界、功率/速度/次数约束；预览、轨迹解析、摘要、时间估算；材料推荐、校准矩阵、最佳单元格、反馈调参；任务准备、重生成、状态、取消、记录。

G-code 加工语义等价，量化容差：

| 项目 | 标准 |
|---|---|
| 加工边界与坐标极值 | 误差 ≤0.05mm |
| S、F、次数、模式、连接方式、安全字段 | 100% 一致 |
| 轮廓追踪多边形 | 与 Python 版 IoU ≥98% |
| 光栅扫描线数量 | 差 ≤3% |
| `core/ai_laser_gcode` 内不经过字体或 OpenCV 的纯算术路径 | 指令序列等价（§20.1 级别 1）：解析后逐条比较 G/M 码、X/Y/I/J（3 位小数）、S/F/P 数值；时间戳与随机 ID 掩码 |
| 空格、换行、注释、非关键浮点格式 | 允许变化 |

### 4.2 MCP 工具（本期冻结）

| 模块 | 工具 | 默认状态 |
|---|---|---|
| check_laser_connection_tool | `check_laser_connection_tool` | 注册 |
| laser_safe_action_tool | `laser_safe_action_tool` | 注册 |
| laser_workflow_tool | `laser_workflow_tool` | 注册 |
| laser_material_calibration_tool | `material_params_tool`、`recommend_laser_params_tool`、`run_calibration_grid_tool`、`select_calibration_cell_tool`、`start_tuned_job_tool` | 注册 |
| text_laser_task_tool | `generate_text_laser_task_tool`、`refine_laser_params_from_feedback_tool`、`regenerate_text_laser_task_tool` | 注册 |
| text_image_gcode_tool | `generate_text_image_gcode_tool` | 注册 |
| laser_grbl_tool | `laser_grbl_tool` | 注册 |
| laser_network_grbl_tool | `laser_network_grbl_tool` | 注册 |
| laser_asset_gcode_tool | `ai_laser_gcode_tool` | 注册 |
| command_execution_tool | `command_execution_tool` | 默认彻底禁用；仅本地电脑显式开启开发者调试开关后可用；开关默认值、作用域和审计字段须在实现阶段冻结 |
| open_app_tool | `open_app_tool`、`open_jsjds_website_tool` | 注册 |
| shortcut_key_execution_tool | `shortcut_key_execution_tool` | 注册 |
| text_image_tool | `generate_text_image_tool` | 注册 |
| camera_tool | `camera_tool`、`adjust_the_camera_view_tool` | 仅 `ENABLED_IP_CAMERA=true` |
| homeasstant_tool | `set_xiaomi_screen_light_status_tool`、`get_xiaomi_screen_light_status_tool` | 服务端禁用 |
| lasergrbl_gui_tool | `lasergrbl_gui_tool` | 服务端禁用 |

冻结字段：名称、参数名、JSON 类型、必填集合、默认值、枚举/范围、返回 JSON 结构及安全相关约束。工具和参数描述不要求与 Python 版逐字一致，但必须语义准确、与当前 Rust 实际行为一致，并说明用途、单位、范围、默认值、前置条件、确认要求及失败/能力不可用行为。允许调整提示词的措辞、组织方式、示例和工具选择指导，但不得引入不存在的工具或参数，也不得要求跳过确认或绕过安全门禁。注册顺序先 `PRIORITY_TOOLS` 8 个模块再字典序。`inputSchema` 的 `title` 不要求一致。断线、重连、心跳、协议错误、关闭有明确行为；日志与协议通道隔离。

`tools/list` 验收以源 Python 当前实际注册集合为准：默认隔离采样得到 20 个工具，包含 `command_execution_tool`；本期不臆造不存在的开发者开关。服务端能力列表与提示词必须反映当前实际模式。

开发者调试开关只能由运行 host 的本机启动配置或命令行显式设置，不能通过 HTTP、Web、MCP 或移动端远程打开；切换后需重启 host，并在审计记录中标明当前模式。未设置或值非法时一律按默认安全模式处理。

### 4.3 Web 与 API

本期保持 17 个路由、请求字段、`success/result/detail/status` 包装、状态码和 `/preview?path=` 白名单不变：
`/api/health`、`/api/ui/runtime-config`、`/api/ui/serial-ports`、`/api/ui/material-options`、`/api/ui/material-recommendation`、`/api/upload`、`/api/draw/preview`、`/api/connection/check`、`/api/text-task/generate`、`/api/text-task/send`、`/api/workflow`、`/api/material-lab/preview`、`/api/material-lab/send`、`/api/material-lab/select-cell`、`/api/material-lab/manage`、`/api/material-lab/export`、`/api/material-lab/recent`。

旧版 `/material-lab` 是服务端模板页，靠 `__MATERIAL_LAB_DATA__` 注入初始数据，React 单页应用没有这条通道：`/material-lab` 路径保留为前端路由，初始数据由新增的只读接口 `GET /v1/material-lab/bootstrap` 一次返回（api-contracts.md §2.3，已决定 2026-09-14），前端挂载时只调用它一次，不再组合多个 `/api` 接口拼首屏；阶段 1 提取 `__MATERIAL_LAB_DATA__` 字段清单只用于冻结该接口的字段表。允许新增 `/v1/*` 版本化接口（能力发现、实时状态、材料实验室首屏只读数据、配对），须文档化。所有客户端经后端工作流调用硬件，不得绕过安全状态机。上传、预览、下载限制路径、大小、类型、文件名。错误响应可读且不泄露凭据、私有路径。

### 4.4 新前端

React + TypeScript 重新实现，与后端移植并行：

- 统一主页、材料库、任务工作流和 Excalidraw 画板为一个工程；去掉 IIFE bundle，统一 React 实例。
- 保留现有布局、配色、字体、主要动画、交互流程和安全提示；不要求 DOM、CSS 类名一致。
- 保留文字、图片、G-code、预设、画板入口；保留材料管理、推荐、连接检查、预览、状态、取消、反馈、重生成、确认发送。
- Excalidraw 必须是完整画板。
- 重构前对旧页面截图形成视觉基线；重构后同视口比对。
- 前端先对接 Python 版 `/api`，后端就绪后切换，接口不变故零改动。

### 4.5 激光设备

串口 GRBL 与 Grbl_ESP32/ESP3D 网络：连接、状态、受控语义动作、完整文件发送；后台任务状态、进度、取消、错误记录；连接检查、能力识别；急停、暂停、关激光、解锁、归零、设原点、软复位、受控移动。替代传输方案必须满足或加强现有确认、探测、完整性和失败停止要求。

带外急停通道（必须）。`laser-device` 中每个设备连接必须拆成两条逻辑信道：

- 顺序流信道：逐行发送 G-code，等待 `ok`/`error` 流控，可被取消。
- 抢占式信道：纯化为发送 GRBL 原生单字节实时命令（`0x18` 软复位、`!` 暂停/进给保持、`~` 恢复、`0x85` jog 取消、`?` 状态查询）。该信道不排队、不等待 `ok`、不受顺序流信道的背压和取消状态影响，直接写入同一底层串口或 socket。普通 G 码（包括 `M5`）属带内指令，严禁作为抢占字节穿插直写，避免在数据流中交织破坏 G-code 行语法或因下位机缓冲区排队而延期执行。网络连接（TCP）底层套接字必须显式配置 `TCP_NODELAY=true`，禁用 Nagle 算法以杜绝单字节拼包延迟；串口抢占写入仅提交至 OS 发送缓冲，禁止调用无超时阻塞的同步 `flush()`（避免 Windows `FlushFileBuffers` 在流控异常时挂死）。

约束：连接对象由 host 内的单一所有者持有，顺序流 worker 与 `laser_safe_action_tool` 共用同一底层连接写句柄，不允许因为 worker 独占端口而使急停打不开端口（旧 Python 版 worker 子进程独占串口即是这个缺陷，不带入）。急停、关激光、软复位的调用路径上不得经过数据库或任何可能阻塞的业务锁。
- 紧急停机（Emergency Stop）：直接通过抢占式信道向底层发送单字节 `0x18`（Ctrl-X 软复位），下位机 ISR 瞬间切断步进中断与激光 PWM 输出并清空缓冲区，随后停止顺序流。
- 任务取消（Cancel）：先通过抢占式信道发送 `!`（进给保持减速停机），停止顺序流，轮询 `?` 确认进入 `<Hold:0>` 状态且激光功率归零后，再发 `0x18` 清空队列（轮询硬超时 500ms，超时或控制器已处于 Alarm/Error 时不再等待，直接发 `0x18`，禁止无限轮询）。`M5` 属于普通 G-code 带内指令，不作为取消协议或抢占式信道备选。
- 固件参数硬门禁：连接握手阶段必须通过 `$$` 读取并确认固件参数 `$32=1`（激光模式）；若 `$32=0`（主轴模式）则拒绝就绪，拒绝完整文件发送与任何开激光动作，错误文案说明原因并指引用户在控制器上设置 `$32=1`。软件默认不自动改写控制器设置（改写 EEPROM 属真实设备行为变更，按 §23 需用户单独授权后才可提供"一键纠正"）。目的：消除进给保持（`!`）静止聚焦时的定点引燃火灾隐患。
- 急停/取消之后的状态机：控制器收到 `0x18` 后进入 `Alarm` 状态，工件坐标可能失效。job 进入终态（`cancelled`/`failed`）后，设备状态记为 `alarm`，后续任何发送必须先经 `laser_safe_action_tool` 的 `unlock`（`$X`）或 `home`（`$H`）并重新确认，软件不自动解锁、不自动重发。
- 网络失联兜底：网络模式下 host 以 `?` 做心跳（建议 250ms 一次），连续 500ms 无响应即判定失联，停止顺序流，job 标 `failed`，并向用户明示"控制器已脱离软件控制，急停无法送达"，此时依赖固件与硬件急停。

网络传输下抢占式信道走同一 Telnet 连接（Grbl_ESP32 的 Telnet 支持实时字节），HTTP 仅用于只读查询。验收：假串口/假 GRBL 测试中，顺序流积压 ≥10,000 行且 worker 处于等待 `ok` 时，急停字节（`0x18`）须在 100ms 内到达底层写端（串口与已开启 `TCP_NODELAY` 的 socket 各测一次）；取消流程中假设备依次收到 `!`、`?`、`0x18`，且在 `!` 之后不再收到任何顺序流行。

### 4.6 材料与校准

查询、增删改、导入导出（JSON 格式保留）；内置常用材料预设（椴木、亚克力、皮革、纸板，与 Python `DEFAULT_MATERIAL_PARAMS` 同值）在首次建库时播种为普通材料行，用户库文件缺失不是空库（§22，2026-09-14）；按材料、厚度、模式推荐；校准矩阵生成、预览、确认发送、结果选择；实测参数来源、置信度、更新时间；危险与不明材料拦截；参数改变后须重新预览确认。

### 4.7 可选功能

ONVIF 摄像头（发现、拍照、分析、云台）、Home Assistant、打开应用/网页、快捷键、命令执行、LaserGRBL GUI 自动化。允许条件编译、sidecar 或"能力不可用"响应；不得伪装成功。RTSP 取帧依赖本机 ffmpeg，缺失时返回能力不可用。

## 5. 平台能力矩阵

| 平台 | 必须支持 | 限制 |
|---|---|---|
| Windows Rust 服务 | 全部核心、MCP、Web/API、串口/网络、材料、摄像头、HA、本机应用、快捷键、LaserGRBL GUI；命令执行仅本地开发者模式提供 | 高风险能力受门禁；命令工具默认不注册 |
| Windows Tauri 桌面 | 完整 UI、托盘、服务生命周期、更新框架 | 更新服务器与签名另行授权 |
| Windows 浏览器 | 创作、画板、材料、预览、状态、任务、经确认的远程操作 | 默认本机；不直连设备 |
| Raspberry Pi | 无头服务、核心、MCP、Web/API、串口/网络、材料 | 无 GUI 自动化；CI 交叉编译验收 |
| Android | 移动客户端，局域网连 host，预览、状态、材料、任务、受控远程操作 | 不直连串口、不跑 GUI、不执行任意命令；本机无 SDK，验收 `partial` |
| iOS | 与 Android 对等 | 需 macOS/Xcode，验收 `partial` |
| 小智/MOSS | 直连 Rust MCP，调用当前已注册工具；开发者模式下验证实际注册集合契约 | 提示词和工具说明按实际能力适配；默认不暴露命令工具 |

## 6. 桌面与移动形态

- Tauri 2 桌面：内嵌前端；启动、停止、状态；托盘；更新框架（版本检查、包校验、失败回滚设计）；安装包与便携形式。发布操作另行授权。
- 安装目录（`InstallRoot`）：安装包必须允许用户在安装向导中选择整个程序安装目录，安装器将 host 可执行文件、Tauri 资源、前端静态资源和卸载/更新元数据整体写入该目录。用户未自定义时使用安装器默认目录：Windows 默认 `%LOCALAPPDATA%\Programs\MOSS`，其他平台使用对应的标准应用安装目录。`InstallRoot` 只承载程序文件，不承载数据库、锁文件、上传、预览或用户生成数据。
- 便携包不经过安装器，`InstallRoot` 定义为解压后的程序目录；便携模式的 `RuntimeRoot` 默认是 `<InstallRoot>/.runtime/`。
- Android/iOS：手动输入或 mDNS 发现 host；配对码认证；默认查看状态、创建预览任务、材料和历史；当前 Android 原生文字页只做 preview/status，不提供 confirm_send、cancel、feedback、regenerate、设备直连或本地 G-code。若后续开放受控远程确认或安全动作，必须复用服务器状态机与旧 17 路由，并作为独立能力和页面单独验收；断连或权限不足即停。iOS 本地构建受苹果官方生态硬性限制（必须依赖 macOS 与 Xcode），本地 Windows 环境下通过 GitHub Actions 云端 Mac Runner（官方托管的 `macos-14` 真实苹果机环境）进行跨平台 CI 自动化打包，构建输出未签名 iOS 应用包并作为 Artifacts 归档供下载，用户可在 Windows 下使用爱思助手、AltStore 或 Sideloadly 配合个人免费 Apple ID 免越狱安装至真机。

## 7. 数据与配置迁移

### 7.1 存储：全部结构化数据进 SQLite

| 数据 | 新位置 | 说明 |
|---|---|---|
| 材料库 | SQLite `materials` 表 | `LASER_MATERIAL_PARAMS_FILE` 变为导入源；首次建库先播种内置预设为普通行，再导入旧文件，旧文件缺失时库内即为预设，不是空库；旧文件里对内置格子的 `null`/`{}` 墓碑视为该格已删除，播种时跳过（A1 落法，db-schema.md §3.6.1）；`material_params_tool` 与 `/api/material-lab/export` 保持同一嵌套结构，顶层 `version` 恒为整数 1；导出结果与导入源按 JSON 解析后语义等价（§20.1 级别 1），不要求键序或逐字节一致（db-schema.md §3.6） |
| workflow | SQLite `workflows` 表，JSON 字段存原结构 | 读入时做路径重映射 |
| 串口/网络 job | SQLite `jobs` 表 | 后台 worker 通过数据库交换状态 |
| 文字任务、校准记录、AI 状态 | SQLite 对应表 | |
| G-code、预览图、上传、生成图片 | 仍是文件，目录布局由 `<RuntimeRoot>` 下的固定子目录定义 | 数据库只存相对路径与 SHA-256 |

安装根目录（`InstallRoot`）与运行根目录（`RuntimeRoot`）是两个独立概念。`InstallRoot` 由安装器的“安装位置”选择确定：支持用户自定义完整目录，未选择时采用平台默认目录；运行中的程序通过自身可执行文件位置或安装器记录解析它。`InstallRoot` 不得作为用户数据目录的隐式替代。

运行根目录（`RuntimeRoot`）必须是一个已解析的绝对目录，所有持久化运行时数据都从该目录派生：

1. `MOSS_RUNTIME_DIR` 非空时直接使用其目录；相对值按启动程序的配置根解析后再转为绝对路径，不得追加 `.runtime`。
2. 便携模式使用可执行文件同级的 `.runtime/`。
3. 安装模式默认使用 Windows `%APPDATA%\moss\`；其他平台使用对应用户级应用数据目录。
4. 优先级固定为：`MOSS_RUNTIME_DIR` > 便携模式 > 安装模式默认目录。首次启动创建目录后再 `canonicalize()`；不能对尚不存在的路径直接 canonicalize。

数据库统一位于 `<RuntimeRoot>/moss.sqlite`，锁文件位于 `<RuntimeRoot>/moss.lock`；上传、预览、生成物、WAL 文件和迁移临时文件也必须基于同一个 `RuntimeRoot` 做绝对路径和白名单校验。`trait RuntimeStore` 抽象读写，测试用内存库。接口返回运行时文件路径的规则（2026-09-14 决定，§22）：本机回环请求可返回相对于 `RuntimeRoot` 的相对路径，正斜杠分隔，不含盘符或根；非回环（局域网手机、其他主机）请求只返回文件名，不返回任何目录成分；任何请求都不得返回 `RuntimeRoot` 或 `InstallRoot` 的绝对路径。

### 7.2 迁移规则

- 旧数据只读；`moss-host migrate --from <旧根> [--dry-run]` 输出计划与问题清单；幂等，重复执行不产生重复。
- 路径重映射分两步：先按历史根替换前缀，再按目录布局映射把旧布局改到现布局。迁移预检（2026-09-12）实测：workflow 产物路径引用共 1581 处，同时指向当前源目录（1180 处）与更早的 `moss-xiaozhi-mcp-main`（401 处）两个历史根，且早期布局把 `.lasergrbl_text_tasks/` 放在仓库根下（现布局为 `.runtime/lasergrbl_text_tasks/`，同一根内两种布局并存，两代根都适用）。因此 `migrate --from` 可多次传入历史根（或自动识别 payload 中出现的已知根前缀）。仅做单根前缀替换会产生 1037 处 stale（约 65%），两步映射后实测仅剩 3 处缺失。仍找不到文件的记录标 `stale`，`feedback`/`regenerate` 对 stale 记录返回明确错误；stale 清单写入迁移报告，迁移验收以映射后缺失 ≤3 且逐条解释为基线。
- 无法映射的数据隔离并报告，不静默丢弃。迁移后按数量、关键字段、文件哈希核对。

材料冲突规则（已确认 2026-09-11）。旧材料库结构是 材料名 → 别名 → 厚度 → 模式（engrave 下再分 raster/outline，cut 单层）→ 参数格，每格带 `source`、`updated_at`、`notes`。冲突按"参数格"判定，不按材料名整体判定：

| 情况 | 处理 |
|---|---|
| 数据库里没有这个材料 | 直接导入 |
| 材料存在，但这个厚度或模式的格子不存在 | 直接导入该格，不动其他格 |
| 同一格两边都有且参数完全一样 | 跳过，不算冲突 |
| 同一格两边都有且参数不同 | 列入冲突清单，逐条显示两边的功率、速度、次数、点距、来源、更新时间和备注，等用户确认 |
| 用户确认覆盖 | 用旧 JSON 的值覆盖数据库该格，原值写入 `material_history` 表可追溯 |
| 用户不确认 | 保留数据库现值；用户随时可在材料页或 `material_params_tool` 里手动修改 |
| 别名 | 两边取并集，不产生冲突 |

`migrate --dry-run` 只输出冲突清单不改库；`migrate --on-conflict keep|overwrite` 供无人值守场景，默认 `keep`；交互模式逐条确认。冲突清单同时写入迁移报告。
- 不迁移：venv、缓存、字节码、失效临时任务、测试临时文件。

### 7.3 `.env` 与敏感数据

键集合与 `.env.example` 一致，dotenvy 读取。真实 token、IP、串口、密码、私有路径不进仓库、测试夹具、文档、日志。密钥库属后续里程碑。

## 8. 安全需求

- 生成、预览、推荐、校准方案、反馈默认不访问设备；预览不等于发送许可；客户端不得直连控制器。
- 完整发送、运动、切割、开激光、危险命令需明确确认，内容含文件、材料、厚度、模式、功率、速度、次数、连接、目标设备。命令执行工具不因开发者开关而绕过本机权限、审计或进程隔离要求；默认禁用时不得出现在可用能力列表。
- 预览成功时冻结 SHA-256 与全部关键加工字段并持久化；确认发送时校验，不一致即拒绝；字段变化后快照作废。客户端的"已确认"不能替代服务器校验。
- 发送前只读探测；不可达、身份不符、异常、超时即停，不隐式重试。握手阶段强制校验固件 `$32=1` 激光模式，未开启时拒绝完整发送与开激光动作并给出指引，软件不自动改写控制器设置（§4.5、§23），消除进给保持静止时的引燃隐患。取消与急停语义分开：取消停止后续排队发送，急停走 §4.5 抢占式信道发送 `0x18` 软复位立即切断硬件输出；两者在固件 `$32=1` 硬约束下均确保最终激光处于关闭状态。
- PVC 与明确危险材料拒绝；"塑料""软卡"先澄清；不声称自动识别真实材料。工作区、功率上限、边距、次数上限作为基线。
- Web/API 默认只监听本机；非回环地址时数据接口要求 bearer，配对入口与静态资源按 §17 白名单放行；移动端先配对；危险操作需与任务绑定的确认；命令执行、GUI 自动化、私有文件访问不向移动端与普通浏览器开放；安全操作记录不含明文凭据或可逆 token 片段。

## 9. 疑似旧错误处理

安全门禁不得弱化；发现旧实现与测试、文档矛盾时建立差异记录；普通差异给出"兼容"与"修复"两种影响，未确认前不静默改变用户可见规则；明显崩溃或漏洞可先阻断并报告。

契约冻结不等于实现照抄。本期冻结的是 Python 实际注册集合与 17 个路由的对外契约（§4.2、§4.3）与安全门禁语义（§8）；内部实现允许并鼓励按 Rust 与同进程模型重做。凡旧 Python 实现中仅因多进程、文件轮询、动态类型才存在的做法，一律不带入 Rust 版，已识别的包括：以 kill 子进程实现取消、通过文件或数据库轮询传递取消信号、worker 子进程独占串口导致急停无法共用连接、业务层直接操作 `serde_json::Value`。这类差异记为 "实现差异"，不需要走用户可见规则的确认流程，但须在 docs/MIGRATION_STATUS.md 留一行说明。

## 10. 非功能需求

- 分发：Windows 后端 `moss-host.exe` 作为单文件发布，使用 `x86_64-pc-windows-msvc` + `crt-static`、`rusqlite` `bundled` 和 `rust-embed` 静态嵌入 `frontend/dist`，不得依赖额外分发的 VC Runtime、SQLite、OpenCV、FreeType 或 OpenSSL DLL。Windows 系统 API DLL 不计入本项；Tauri 图形桌面的 WebView2 Runtime 另列为桌面前置条件，不与后端 exe 的单文件验收混同。该组合必须在任何业务 crate 编写前通过 §11 的 packaging spike；Pi 提供 CI 产物；Android 可构建；iOS 可构建工程。
- 性能：同硬件同输入不明显慢于 Python；对 2000×2000 抖动建立可重复基准。
- 稳定性：异常退出后不自动恢复未重新确认的危险任务；启动时把残留的 running/sending 记录标为 failed 并注明中断原因（db-schema.md §1）；SQLite 采用单写连接并发模型，且 G-code 逐行发送热路径 worker 绝对禁止访问数据库（取消走内存通道，进度由独立后台任务异步节流持久化，杜绝发送热路径因锁竞争造成串口欠载与步进卡顿，见 db-schema.md §1 第 7 条、§3.4）；状态可追踪；结构化日志与协议分离；无设备、缺可选依赖时可启动并降级。
- 质量：核心计算与 I/O 分离；平台差异用条件编译或适配器；公共模型有版本和序列化测试；Rust、前端、移动工程各自 fmt/lint/test。

---

# 第二部分：技术实施

## 11. 阶段 0：环境准备

| 步骤 | 命令 | 说明 |
|---|---|---|
| 1 | `winget install Rustlang.Rustup` | 用户态，不提权 |
| 2 | `winget install Microsoft.VisualStudio.2022.BuildTools --override "--add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --passive"` | 弹一次 UAC；约 3-4GB |
| 3 | `rustup default stable`，验证 `cargo build` 能链接 | 目标 `x86_64-pc-windows-msvc` |
| 4 | `cargo install tauri-cli --locked`；确认 WebView2 Runtime | Tauri |
| 5 | Android：OpenJDK 17、SDK 34、NDK；`rustup target add aarch64-linux-android` | 不装则 Android 标 `partial` |
| 6 | `rustup target add aarch64-unknown-linux-gnu` | 树莓派链接在 GitHub Actions 完成 |
| 7 | Windows packaging spike：`crt-static` + `rusqlite` `bundled` + 确定性约 40 MiB 前端 fixture 嵌入；Release 构建、依赖扫描、干净 Windows 启动 | 在任何业务 crate 前完成；记录 exe 大小、`dumpbin /DEPENDENTS`、SQLite 读写和内嵌资源访问结果；阶段 7 产出真实 `frontend/dist` 后按同一检查复跑；失败即暂停业务迁移 |
| 8 | rmcp spike，半天 | 见 §14 |
| 9 | 失败处理 | Build Tools 装不上不得默认切 GNU 目标：`rusqlite` `bundled` 需要可用 C 编译器（MSVC 使用 `cl.exe`），rustls 的 `ring` 在 `windows-gnu` 下有已知编译问题，且 `crt-static` 是 MSVC 目标特性，GNU 产物同样过不了 §10 单文件验收。Build Tools 视为不可回退前置，优先修复安装；GNU 仅在显式加装 MinGW-w64 并先通过 packaging spike（bundled SQLite、rustls、静态链接）后评估。不得把未验证的单文件能力宣称为完成 |

## 12. 架构与目录

```
小智/MOSS ──wss──┐
浏览器 ──http────┤──► moss-host.exe（独立 host，复用 laser-host）
Android/iOS ─────┘
Tauri 桌面 ─────────► laser-host（同进程，默认不再拉起子进程）
```

```
moss-xiaozhi-mcp-main(rust)/
├─ Cargo.toml
├─ crates/
│  ├─ laser-contracts/   serde 模型 + schemars 导出 JSON Schema 供前端生成 TS
│  ├─ laser-core/        图像、字体、几何、光栅、轮廓、G-code、安全规则、时间估算、pycompat
│  ├─ laser-runtime/     LaserSettings、SQLite RuntimeStore、迁移器
│  ├─ laser-host/        可嵌入的 host 生命周期、HTTP/MCP 启停与优雅关闭
│  ├─ laser-device/      serial / telnet / http grbl、job worker、onvif、rtsp(ffmpeg)、ha、win_gui
│  ├─ laser-tools/       16 个模块，与 tools/*.py 一一对应
│  ├─ laser-mcp/         WebSocket 客户端、JSON-RPC、工具注册表
│  └─ laser-api/         axum 路由、静态嵌入、认证
├─ apps/
│  ├─ moss-host/         bin：serve / mcp / web / lab / gcode / migrate
│  ├─ desktop/           Tauri 2 桌面
│  └─ mobile/            Tauri 2 Android/iOS
├─ frontend/             React 18.3.1 + TS + Vite 6 + Excalidraw 0.18.1
├─ migration/            黄金样本生成脚本（Python，只读调旧代码）
├─ tests/golden/         G-code、API 快照、mcp_tools.json、xiaozhi 帧
├─ tests/visual-baseline/
└─ docs/MIGRATION_STATUS.md
```

依赖方向单向：host → api/mcp → tools → device → runtime → core。core 不依赖 tokio。

模块解耦与存储边界：
- `laser-api` 严禁直接依赖 `rusqlite`，严禁直接操作原生数据库连接或编写裸 SQL。所有工作流、任务与业务状态的读写，必须通过 `laser-runtime` 暴露的 `Arc<dyn RuntimeStore>` 抽象接口进行，确保 API 层可以注入 `MemoryStore` 进行毫秒级脱机无盘单元测试。
- `laser-core` 计算核心必须提供面向任务的粗粒度引擎门面（Facade / Pipeline，如 `TextTaskEngine`、`ImageRasterPipeline`），以“输入规范 ➔ 输出 G-code & 预览图”为黑盒契约边界；内部的字形解析、贝塞尔曲线自适应细分、轮廓层级提取和路径优化排序等微观算子封装在 core 内部，严禁上层 API 或 Tools 直接抓取底层零散算子拼装业务。

类型边界：`serde_json::Value` 只允许出现在 `laser-runtime` 的读写入口（payload 反序列化与宽松映射）、`laser-mcp`/`laser-api` 的协议边界（JSON-RPC 参数解析、HTTP 请求体）以及迁移器中。`laser-tools`、`laser-device`、`laser-core` 之间传递的必须是 `laser-contracts` 中的强类型 struct；出现 `as_str()`/`as_f64()` 式的取值猜测即视为架构违规。针对审计详情（`audit_log.detail`）或材料未知扩展属性（`material_params.extra`）等动态载荷，由 `laser-contracts` 定义统一的强类型枚举/载体（如 `AuditDetail`）或受控的类型安全原始 JSON 包装，禁止业务 crate 直接进行弱类型探测。CI 加一条 grep 或 clippy 级检查，禁止这三个 crate 直接使用 `serde_json::Value` 的动态解包方法。

## 13. 依赖选型

| 用途 | crate | 备注 |
|---|---|---|
| 运行时 | tokio | 串口与图像计算放 `spawn_blocking` |
| 序列化 | serde、serde_json（`preserve_order`） | 键序按 Python dict 顺序输出，便于 diff 阅读；键序不是验收条件（§20.1） |
| 存储 | rusqlite（`bundled`） | 需 MSVC；schema 迁移统一由 `laser-runtime` 自管 `schema_version`，不引入 refinery。每个版本在一个事务中执行，全部语句成功后才写入 `schema_version`；迁移失败回滚并阻止业务读写；迁移器支持 dry-run、重复执行幂等与中断恢复，执行规则见 db-schema.md §3.1、§7 |
| 配置 | dotenvy、clap | |
| 日志 | tracing | stderr 与文件 |
| HTTP 服务 | axum、tower-http | |
| 静态嵌入 | rust-embed | `frontend/dist`；约 40MB 仅为估算，实际大小以 packaging spike 记录为准 |
| WebSocket | tokio-tungstenite（rustls） | |
| HTTP 客户端 | reqwest（rustls，禁默认 features） | ESP3D、HA、小智 vision、ONVIF SOAP |
| 串口 | serialport | Linux 需 libudev |
| 图像 | image、imageproc（仅高斯模糊） | 轮廓自实现 |
| 字体 | ttf-parser（轮廓、TTC）、ab_glyph（位图） | |
| 哈希/ID/时间 | sha2、uuid（`simple` 格式）、chrono | |
| Windows | windows crate（`#[cfg(windows)]`） | GUI 自动化 |
| MAC 地址 | mac_address | 摄像头请求头 |
| Schema | schemars | 前端 TS 类型生成 |
| 前端 | react 18.3.1、@excalidraw/excalidraw 0.18.1、vite 6、typescript、playwright | 首期锁 React 18.3.1，以保持现有视觉与行为基线并减少重构变量；不把 React 19 视为硬性不兼容——Excalidraw 0.18.1 的 peerDependencies 本身接受 React 17/18/19；升级 React 19 需单独通过构建、Excalidraw 加载、字体、编辑、导出与 E2E 验证 |

不选：opencv/freetype 绑定（破坏单文件）、PyO3（仍需 Python）、纯 Rust RTSP 解码（无成熟 H.264 软解）、内嵌 CJK 字体（20MB 且改变默认输出）。

## 14. MCP 传输

方案 A（首选）：rmcp 3.x + 自定义 Transport 包装 tungstenite。通过条件：`tools/list` 输出与 `tests/golden/mcp_tools.json` 在名称、参数结构、类型、必填、默认、枚举/范围等机器字段上兼容（§20.1 级别 3）。描述字段不要求逐字一致，按本节字段规则做语义和行为核验。若黄金样本仍包含描述字段，比较器须将其单独处理，不得把描述文字差异判为机器契约失败。
方案 B（回退）：手写 JSON-RPC 2.0，只实现 `initialize`、`tools/list`、`tools/call`、`ping`，schema 用 `serde_json::json!` 手写。

共同要求：阶段 1 用真实 token 采集 `initialize` 与 `tools/list` 帧到 `tests/golden/xiaozhi_frames/`（token 打码）；断线时取消进行中的 `tools/call`，未确认的发送任务不自动恢复；工具返回 `indent=2, ensure_ascii=False` 的 JSON 文本。

## 15. 前端实施

- `frontend/` 结构：`pages/`（主页、材料库、画板）、`components/`、`api/`（生成的 TS 类型 + fetch 客户端）、`state/`。
- 视觉基线：阶段 1 用 Playwright 在 1440×900 与 390×844 截取旧页面全部主要状态；重构后同视口比对，像素差异阈值 0.5%。
- 资源：Excalidraw 使用 `@excalidraw/excalidraw/dist/prod/fonts` 的字体文件，复制到 `frontend/public/excalidraw/fonts/`，并在加载 Excalidraw 前设置 `window.EXCALIDRAW_ASSET_PATH = "/excalidraw/"`；旧 `vendor/excalidraw/fonts` 只作视觉基线，不代替新包字体。
- 离线验收：Playwright 阻断外网后，画板仍可加载、编辑、导出；字体请求只能命中本地资源，不得访问 CDN。
- 联调：先对接 `python -m moss_mcp.web_server`，后端就绪后切换。

## 16. Tauri 桌面与移动

- `apps/desktop`：WebView 内容为 `frontend/dist`；默认在 Tauri 进程内启动并持有 `laser-host`，通过统一 shutdown token 做托盘退出、窗口关闭和异常退出清理；不再默认拉起独立 `moss-host` 子进程。Windows 桌面仍需单独确认 WebView2 Runtime。
- 退出时的激光安全兜底（同进程与子进程两种形态都适用）：shutdown token 触发后的第一步、且在关闭端口、停止线程之前，`laser-host` 对每个仍打开的设备连接通过 §4.5 抢占式信道直接写入单字节 `0x18`（软复位），下位机微秒级切断 PWM 激光与步进脉冲；随后在 100ms 内非阻塞读取设备返回的欢迎标头（若超时则重发一次 `0x18`），整步操作通常在 150ms~300ms 内快速完成，不依赖普通 G 码响应，设置总超时 2s 作为最终兜底；超时后强制注销并记一条安全日志；正在进行的 job 标为 `failed` 并注明 "host 退出中断"。触发点覆盖：Tauri 窗口关闭事件（`CloseRequested`，先执行兜底再允许关闭）、托盘退出、`Ctrl+C`/`SIGTERM`、Windows 上的 `SetConsoleCtrlHandler`（`CTRL_CLOSE_EVENT`/`CTRL_SHUTDOWN_EVENT`）以及 Rust panic hook。为保证在非异步上下文（控制台事件线程、panic hook）中能可靠执行，设备连接必须暴露脱离 tokio runtime 调度的同步裸写句柄（RawHandle / raw socket），以无锁系统调用直接写出 `0x18`。诚实边界：进程被任务管理器强杀或系统掉电时软件兜底不会执行，此时依赖 GRBL 固件自身的看门狗/串口断开行为与硬件急停按钮，文档与用户界面须写明这一点，不得宣称软件能覆盖所有情况。
- `apps/moss-host`：保持为调用 `laser-host` 的薄二进制，供无头服务、浏览器、MCP 和移动端连接使用。
- 若平台限制迫使桌面采用子进程：子进程及其后代必须加入 Windows Job Object（关闭时整树清理）；退出顺序必须是停止接收新任务、取消未确认任务、确保激光安全停机、优雅关闭、超时后清理进程树，等待端口释放后才允许下次启动。
- `apps/mobile`：同一前端，feature `mobile` 去掉串口、GUI、命令执行页面；连接走 §17 配对。
- capabilities 分开：桌面开放 shell/tray，移动只开放 http。
- 验收：桌面本机；Android/iOS 标 `partial` 并记录所缺环境。

## 17. 局域网配对（最小版本）

- host 首次启动生成 `.runtime/auth/host_key`。
- 桌面端或终端显示 6 位一次性配对码，5 分钟有效；配对码只在内存中保存，成功使用或过期后立即销毁。
- `POST /v1/auth/pair` 是非回环场景唯一免 Bearer 的数据接口；静态资源与页面路由（`/`、`/material-lab`、`/draw`、`/assets/*`、`/vendor/*`）也免认证，除此之外的 `/api/*`、`/v1/*`、`/preview` 均要求 bearer。回环地址仍按本地免认证规则运行。
- 配对码按来源 IP 限流；未识别客户端另按配对尝试标识计数：连续错误 5 次锁定 15 分钟；错误响应不区分配对码不存在、错误或过期，锁定期返回明确的重试等待时间。配对成功后立即失效旧码。
- token 使用操作系统 CSPRNG 生成 32 字节随机值，线上的 bearer 是其 Base64 表示；服务端解码后只保存原始 32 字节的 SHA-256 哈希、`pairing_id`、设备名、创建/最后使用时间和撤销状态。长期 bearer 默认有效直到本地管理入口撤销或 host 身份重置，不自动续期、不通过远程接口扩大有效期；撤销后返回 401，不能静默重放危险请求。
- 非回环部署默认视为受信任局域网明文 HTTP 模式，禁止直接映射到公网；跨网访问必须由 Nginx/Caddy 等前置代理做 TLS 终结，并在部署记录中标明。
- 非回环场景新前端增加输入配对码状态：数据请求收到 401 即进入该状态，提交 `/v1/auth/pair` 成功后，浏览器仅把 bearer 存当前会话，移动端存平台安全存储，随后重放原请求。
- 移动端不新增独立的危险操作 `/v1` 路由：认证后复用旧 17 路由（§8 的移动端禁止项不变），`/v1` 仅限能力发现、状态事件、材料实验室首屏只读数据与配对；门禁保持单一实现路径。
- 危险操作只需携带 `workflow_id` 与 `confirmed=true`；服务器从 SQLite 加载该 workflow 冻结的 `confirmation_snapshot`，重新计算当前 G-code 文件 SHA-256 并校验快照字段、文件完整性和连接目标，不接受客户端自报 SHA 作为授权依据。

## 18. 图像与字体算法对应

| Python | Rust | 验收 |
|---|---|---|
| Otsu、灰度、反色、缩放（INTER_AREA） | 自实现 + image；box filter | 容差 |
| Floyd-Steinberg / Atkinson / Sierra-lite / 阈值 | 直译 `raster.py` | 同一输入位图下输出位图逐像素一致（§20.1 精确项） |
| `findContours(RETR_TREE/LIST/EXTERNAL)` | 自实现 Suzuki-Abe 含层级 | IoU ≥98% |
| `approxPolyDP`、`TC89_KCOS` | Douglas-Peucker 替代 | IoU ≥98% |
| Pillow 文字渲染 | ab_glyph + 直译排版 | 尺寸 ≤0.05mm，行数 ≤3% |
| fontTools glyph、TTC | ttf-parser | 字形轮廓点序列逐点一致，font units 含 on/off 标志（§20.1 精确项） |
| RTSP | `ffmpeg -rtsp_transport tcp -i <url> -frames:v 1 -f image2 -` | 缺 ffmpeg 返回不可用 |

Python 语义对齐专项：`py_round` 银行家舍入、浮点格式化（`1.0` 不输出 `1`）、`uuid4().hex`、ISO 时间带 `+00:00`、Windows 路径剥离 `\\?\`、`strip_inline_comment` 规则。字体查找顺序与 Python 相同。

## 19. 阶段与验证

| 阶段 | 内容 | 证据 | 依赖 |
|---|---|---|---|
| 0 | 工具链、Windows packaging spike、rmcp spike、workspace 骨架 | `cargo build` 通过；先用与最终 `rust-embed` 路径一致的确定性约 40 MiB 前端 fixture 验证单文件后端（依赖扫描、干净 Windows 启动、SQLite、内嵌资源访问），结论写入阶段记录；阶段 7 产出真实 `frontend/dist` 后必须按同一检查再跑一次；rmcp 结论写入 §14 | — |
| 1 | 黄金样本、实际 `mcp_tools.json`、小智帧、视觉基线、`__MATERIAL_LAB_DATA__` 字段提取（冻结 `GET /v1/material-lab/bootstrap` 字段表，api-contracts.md §2.3）；Python 测试真实结果 | `tests/golden/`、`tests/visual-baseline/` | 0 |
| 2 | laser-core | 逐模块按 §20.1 比对；`cargo test` | 1 |
| 3 | laser-runtime（SQLite）+ migrate | dry-run 报告；旧数据副本上幂等测试；单写连接并发与启动恢复测试；重映射后缺失 ≤3 基线 | 2 |
| 4 | laser-device + job worker | 假串口、TCP 假 GRBL：发送、探测失败即停、取消；取消信号走内存通道，worker 发送循环期间 `RuntimeStore` 访问计数恒为 0（进度由独立持久化任务写，单独计数；人为持锁 200ms 时串口输出无停顿）；顺序流积压 ≥10,000 行时 `0x18` 100ms 内到达（串口与 `TCP_NODELAY` socket 各一次）；取消流程假设备依次收到 `!`、`?`、`0x18` 且之后无顺序流行；`$32=0` 的假设备拒绝完整发送 | 3 |
| 5 | laser-tools + laser-mcp | 契约比对；真实端点握手 | 4 |
| 6 | laser-api | 17 路由快照比对；旧前端零改动跑通；`GET /v1/material-lab/bootstrap` 契约比对 | 5 |
| 7 | frontend（自阶段 1 起并行） | 视觉回归；Playwright 主流程；Excalidraw 离线字体与无 CDN 验证 | 1 |
| 8 | Tauri 桌面 | 构建、托盘、同进程 host 生命周期；WebView2 前置条件；必要时子进程树清理回归；窗口关闭、托盘退出、Ctrl+C、控制台关闭事件、人为 panic 五条路径下假设备均收到 `0x18`，兜底步骤常规 300ms 内、上限 2s 完成 | 6、7 |
| 9 | 树莓派 CI 交叉构建 | Actions 产物、无头单测 | 6 |
| 10 | Android/iOS 工程 | 本机 `partial` | 7、8 |
| 11 | 全量验收、迁移报告、用户审阅 | 报告含未验证项 | 全部 |
| 12 | 用户确认后删除旧 Python | 精确清单再次确认 | 11 |

每阶段更新 `docs/MIGRATION_STATUS.md`；语义不确定处停下核对。

## 20. 验收与完成标准

- 基线证据：源项目功能清单、工具契约快照、API 场景、安全拒绝场景、数据样本；Python 测试真实结果。
- Rust：单测覆盖核心、状态机、迁移、安全边界；集成测试用假串口、假 GRBL、模拟 HTTP/Telnet/WebSocket、临时目录、模拟摄像头；不以真实硬件为自动测试前提。
- 前端与桌面：主流程 E2E；视觉回归；Excalidraw 真实渲染、编辑、导出；Tauri 构建、托盘、生命周期。
- Windows 打包：`moss-host.exe` 通过 `crt-static`、bundled SQLite 和真实前端嵌入 spike；依赖扫描和干净 Windows 启动均有真实结果；Tauri WebView2 Runtime 作为独立前置条件记录。
- 离线资源：Excalidraw 字体自托管，断网时无 CDN 字体请求且画板功能完整。
- 桌面退出：同进程 host 能在窗口关闭、托盘退出、Ctrl+C、控制台关闭和可被 panic hook 捕获的异常路径释放任务、设备连接、端口和后台线程，并且在释放连接之前假设备已收到 `0x18`，整步不超过 2s；panic hook 与控制台关闭路径经同步裸句柄写入；强杀、进程被操作系统直接终止和掉电无法保证发出停机命令，必须依赖控制器保护与实体急停，且下次启动只把残留任务标记为失败、绝不自动重发。若采用子进程，Job Object 整树清理测试通过。
- 发送热路径：worker 发送循环从第一行到最后一行期间 `RuntimeStore` 访问计数恒为 0；开始与终态写入发生在循环之外；进度写入由独立持久化任务完成并单独计数，其被阻塞时 worker 吐字节奏不变（测试中人为持锁 200ms，断言串口输出无停顿）。急停在积压场景下 100ms 内到达底层写端。
- 类型边界：`laser-tools`、`laser-device`、`laser-core` 无 `serde_json::Value` 直接依赖的 CI 检查通过。
- 跨平台：Windows 完整验收；Pi CI 产物；Android/iOS `partial`。
- 迁移：只在副本上测；数量、字段、状态、文件引用、异常记录报告；幂等；敏感值不进 diff、日志、快照。
- 真实设备：串口、网络、发送、运动、开激光、摄像头、GUI、HA、命令执行的实机验证均需另行确认，未做时标未进行实机验收。

### 20.1 等价级别

不以逐字节文本比对作为通过条件。原因：旧数据同一字段 int/float/str 混用（db-schema.md §8），Python 与 Rust 的浮点打印、键序、空白都会产生无加工意义的文本差异；文本比对会把这些当成失败，掩盖真正的语义差异。黄金样本保存 Python 的原始输出，不做预处理；掩码与规范化只在比对时进行，比对器在 Rust 测试内实现，规则写入 `tests/golden/README.md`。每项验收标注适用级别。

| 级别 | 定义 | 适用 | 比对方法 |
|---|---|---|---|
| 1 语义等价 | 读它的是机器：解析后结构与数值相等，文本形态不管 | G-code/NC；材料库 JSON 导入导出（导出含内置预设；`version` 按数值比较；Python 侧对内置格子删除后留下的 `null`/`{}` 墓碑节点在比对时视为该格不存在，db-schema.md §3.6.2）；workflow/task/job payload；17 路由响应体 | G-code：逐行解析为（指令，参数字典），X/Y/I/J 按 3 位小数比较，S/F/P 按数值比较，注释与空白忽略，掩码时间戳与随机 ID。JSON：解析后 deep-equal，数值按数值比较（`3` 与 `3.0` 相等，`"800"` 与 `800` 不相等），键序与空白忽略，掩码字段见 api-contracts.md §6。用户可见的错误文案字符串逐字相等 |
| 2 容差等价 | 经过字体渲染、OpenCV 缩放或轮廓追踪的路径，结果受实现细节影响 | §4.1 表中边界、IoU、扫描线数量、文字尺寸与行数 | 按 §4.1 各项阈值；超阈值时输出差异样本（叠加图、数值表）供用户决定收紧或接受，不静默放宽 |
| 3 契约等价 | 读它的是另一方程序的 schema 校验 | `tools/list`；`/v1/*` 响应结构 | 字段名、JSON 类型、必填集合、默认值、枚举/范围逐字段相等；忽略 `title`、键序、`$schema`；描述按 §4.2 规则单独核验；工具集合以源 Python 当前实际模式为准 |

精确项：以下两项在输入相同时是纯整数或定点运算，没有理由产生差异，保留精确比较：抖动输出位图逐像素一致；字形轮廓点序列逐点一致。发现差异按 §9 记差异记录并查原因，不放宽为容差。

逐字节 diff 只作为调试手段：规范化后的两份文本可以 diff 看差异在哪里，但不是通过条件。
完成条件：范围内功能全部有实现或经确认的平台限制；Windows 通过自动化与人工验收；Pi、Android、iOS 达到各自要求且缺失项标明；真实小智端点只验证已授权的 `initialize` 与 `tools/list`，代表性工具调用改由模拟客户端、临时目录和假设备完成，且只覆盖无副作用或已被安全替身拦截的路径；提示词和工具说明通过语义与行为核验；前端通过视口核验；G-code 按 §20.1 等价、安全门禁不弱化；迁移通过；所有检查有真实结果；无门禁弱化、凭据泄露、直连绕过；用户审阅后接受。删除旧内容是完成后的独立步骤。

## 21. 风险与回滚

| 风险 | 应对 |
|---|---|
| rmcp schema 无法对齐 | 方案 B，只影响 laser-mcp |
| 轮廓 IoU 不达标 | 展示差异样本，用户决定收紧或接受 |
| SQLite 迁移与 25 工具、17 路由的读写改动量大 | `RuntimeStore` trait 隔离；阶段 3 独立验收 |
| 前后端并行联调分歧 | 接口冻结；前端始终可对 Python 版回归 |
| Android/iOS 环境缺失 | `partial`，不阻断 Windows |
| Build Tools 装不上 | 不可回退前置，优先修复安装；GNU 目标不是现成回退（本机缺 gcc/cmake，bundled SQLite 与 ring 编不过，crt-static 不适用），仅在显式加装 MinGW-w64 并通过 spike 后评估 |
| `crt-static`、bundled SQLite、前端嵌入组合无法生成可运行的单文件后端 | 阶段 0 立即阻断；记录具体 linker/依赖；在解决前不进入业务迁移 |
| Tauri 退出后 host 子进程或后代残留 | 默认采用同进程 `laser-host`；必要时使用 Job Object，并覆盖优雅关闭与超时清理 |
| WebView2 Runtime 缺失 | 作为 Tauri 桌面前置条件单独标记，不冒充后端单文件失败或通过 |
| 发送热路径抢数据库锁导致串口欠载、步进卡顿 | 取消/进度走内存通道，worker 循环零数据库访问，进度由独立任务落库，阶段 4 用访问计数与持锁扰动测试断言 |
| 急停排在积压 G-code 之后 | §4.5 抢占式信道只发单字节实时命令，急停即 `0x18`；阶段 4 用积压场景测延迟 |
| `$32=0` 时进给保持不关激光，静止聚焦引燃 | §8 激光模式门禁，探测阶段拒绝发送 |
| 退出/崩溃时激光停留在开启状态 | §16 退出兜底直接 `0x18`，panic/控制台路径走同步裸句柄；强杀与掉电场景明示依赖固件与硬件急停 |
| 网络模式控制器失联，急停送不到 | §4.5 `?` 心跳 500ms 失联判定，job 标 failed 并明示已脱离软件控制 |
| 历史 payload 收紧为强类型后大面积反序列化失败 | 迁移 dry-run 报告逐条列出失败字段；失败记录标 `stale` 可查看不可发送；宽松映射规则按 db-schema.md §8 逐字段列出并有单测 |

回滚：阶段 12 前全部为新增文件，删除 `Cargo.toml`、`crates/`、`apps/desktop`、`apps/mobile`、`frontend/`、`migration/`、`tests/golden/`、`tests/visual-baseline/`、`target/`、`docs/MIGRATION_STATUS.md` 即恢复。源目录始终只读。

---

# 第三部分：决定记录与授权边界

## 22. 已确认决定（2026-09-11）

| 议题 | 决定 |
|---|---|
| 工具与 API | 冻结 Python 当前实际注册集合：默认 20 个工具（含 `command_execution_tool`）；`homeasstant_tool`、`lasergrbl_gui_tool` 不注册，摄像头按 `ENABLED_IP_CAMERA` 注册；17 个 `/api` 不变 |
| 前端 | 本期做 React/TS 重构，与后端并行，截图基线保证视觉 |
| Android/iOS | 不移出本里程碑，本机验收标 `partial` |
| 可选工具 | 全部移植；RTSP 用 ffmpeg 子进程，缺失返回不可用 |
| 图像验收 | 纯 Rust，容差等价，不绑定 C 库 |
| 存储 | 全部结构化数据改 SQLite，文件类产物仍是文件，材料 JSON 导入导出保留 |
| workflow 存储 | 整条 JSON 存 payload 列，另抽索引列（db-schema.md §2 方案 A） |
| 材料实验室风格 | 合并后统一为主页暗色风格 |
| 材料导入冲突 | 按参数格判定；有差异则提示，确认后覆盖，不确认保留库内值并可手改；别名取并集 |
| 命令执行工具（2026-09-14） | 默认彻底禁用；仅本地电脑显式开发者调试开关启用；启用时仍需审计和本机权限控制 |
| 手机连接 | 6 位一次性配对码；5 次错误锁定 15 分钟；32 字节 CSPRNG bearer，仅存哈希；撤销可用 |
| 工具链 | 授权 winget 安装 rustup 与 VS Build Tools，一次 UAC |
| 真实 token | 允许连小智端点做 `initialize` 与 `tools/list`，不调用工具 |
| 旧 Python | 验收通过并再次确认清单后删除 |
| 旧文档 | 本文取代 `docs/rust-migration/` 草案与两份分离文档，旧稿已删除 |
| 验收等价标准（2026-09-12） | 取消逐字节一致，改为 §20.1 三级等价加两项精确项；黄金样本存原始输出，规范化只在比对时做 |
| Windows 单文件边界（2026-09-12） | 单文件验收对象为后端 `moss-host.exe`；Tauri WebView2 Runtime 单独记录 |
| 桌面 host 生命周期（2026-09-12） | 优先同进程 `laser-host`；保留 `moss-host` 薄二进制供独立服务使用；子进程方案必须有 Job Object 整树清理 |
| 数据库迁移（2026-09-12） | 删除 refinery 选项，统一由 `laser-runtime` 自管 `schema_version` |
| React 基线（2026-09-12） | 首期锁 React 18.3.1 以保持基线；React 19 作为独立升级验证（构建、Excalidraw 加载、字体、编辑、导出与 E2E），不写成永久不兼容 |
| 迁移路径重映射（2026-09-12） | 两步替换：多历史根前缀 + 目录布局映射（早期 `.lasergrbl_text_tasks/` 在仓库根下，同一根内两种布局并存）；预检基线 1581 处引用、映射后缺失 3 处 |
| /material-lab 页面契约（2026-09-12） | 页面路由由前端路由承接，不再用服务端模板注入；初始数据来源见 2026-09-14 决定：固定为 `GET /v1/material-lab/bootstrap` |
| 材料导出验收（2026-09-14 修订） | JSON 解析后语义等价，不要求对象键序或逐字节一致；数值按数值比较（updated_at 经 REAL 往返文本形态不可还原） |
| Windows GNU 回退（2026-09-12） | Build Tools 为不可回退前置；GNU 仅在显式加装 MinGW-w64 并通过 packaging spike 后评估 |
| 非回环认证边界（2026-09-14） | `POST /v1/auth/pair` 与静态资源/页面路由免 Bearer；其余数据接口需 bearer；配对限流、token 撤销和受信任局域网明文 HTTP 边界按 §17 执行；移动端复用 17 路由，不新增危险操作 `/v1` |
| SQLite 并发与启动恢复（2026-09-12） | 单写连接（单个 `Mutex<Connection>`，WAL）；启动时残留 running/sending 标 failed，不自动恢复 |
| 发送热路径与取消信号（2026-09-14） | 取消走 `tokio::sync::watch` 内存通道，worker 逐行只读内存；worker 循环绝对不访问数据库，开始/终态写在循环外，进度由独立持久化任务节流落库；不再 "每行查库"，不以 kill 进程实现取消 |
| 带外急停通道（2026-09-14） | `laser-device` 连接拆为顺序流信道与抢占式信道；抢占式信道只发 GRBL 单字节实时命令（`0x18` `!` `~` `?` `0x85`），普通 G-code 含 `M5` 严禁经此信道；急停即 `0x18`，取消为 `!` → 停流 → `?` 到 Hold → `0x18`；急停后设备记 `alarm`，需 `$X`/`$H` 并重新确认；连接单一所有者，worker 与安全动作共用；socket 必开 `TCP_NODELAY`，禁止阻塞 flush |
| 激光模式门禁 `$32=1`（2026-09-14，新增门禁，用户已确认） | 探测阶段读取 `$32`，非 1 时拒绝完整发送与开激光动作并给出指引；软件不自动改写控制器设置 |
| payload 类型边界（2026-09-14） | `serde_json::Value` 仅限 runtime 读写入口、协议边界与迁移器；tools/device/core 只用 `laser-contracts` 强类型，CI 检查 |
| /material-lab 首屏接口（2026-09-14） | 直接新增 `GET /v1/material-lab/bootstrap`，不再二选一；阶段 1 只冻结字段表 |
| 退出激光兜底（2026-09-14） | shutdown 第一步经抢占式信道直接发 `0x18`，100ms 内未见复位欢迎行再发一次，常规 300ms 内完成，总上限 2s；覆盖窗口关闭、托盘、Ctrl+C、控制台关闭、panic，后两者走同步裸句柄；强杀与掉电明示不覆盖 |
| 契约冻结不等于实现照抄（2026-09-14） | 对外契约与门禁冻结；旧实现中因多进程、文件轮询、动态类型才存在的做法不带入 Rust 版（§9） |
| 危险发送校验（2026-09-14） | 客户端仅传 `workflow_id` 与 `confirmed=true`；服务端从 SQLite 快照重算并校验 G-code SHA-256、关键字段和文件完整性 |
| 旧任务迁移（2026-09-14） | 旧 `pending/running/queued` job 一律迁移为 `failed`，写入安全终止说明；Worker 对状态、SHA、workflow 快照执行三重硬门禁 |
| 材料库缺失与内置预设（2026-09-14，选项 A / 落法 A1） | 继承 Python 行为：内置常用材料预设（`DEFAULT_MATERIAL_PARAMS`，4 种材料，engrave 拆为 raster/outline 同值两格）是材料库的一部分，文件缺失不得解释为空库。落法 A1：首次建库把预设播种为普通 `material_params` 行，之后与用户数据无区别，删除即删行，不保留 `null`/`{}` 墓碑；迁移旧文件时遇到内置格子的墓碑则跳过该格的播种。Python 删除内置格子留下墓碑的导出形态与 Rust 不一致，比对时墓碑视为该格不存在（§20.1） |
| 材料库 `version`（2026-09-14，选项 A） | Rust 对外统一输出整数 `1`。宽松归一化只用于迁移器和加载本地旧数据：`1`、`1.0`、`"1"`（去首尾空白）归一为 1；缺失按 1；无法安全转换（非整数值、其他整数、非数字字符串）判为数据错误，迁移时该文件整体隔离并写入报告，不导入任何材料。`manage(import)` / `material_params_tool(import)` 沿用 Python 的严格判断：只接受 JSON 数值 `1` 或 `1.0`，`"1"` 与其他值一律返回原文案 `导入材料库格式不支持：需要 version=1 的 JSON 对象` |
| 运行时路径脱敏（2026-09-14，选项 C） | 回环请求可返回相对 `RuntimeRoot` 的相对路径；非回环请求只返回文件名（如 `lasergrbl_materials.json`），不返回目录成分；任何请求不返回 `RuntimeRoot`/`InstallRoot` 绝对路径。覆盖 `GET /v1/material-lab/bootstrap` 的 `source_path`/`backup_path`、`manage(import)` 的 `backup_path` 及其他返回运行时文件路径的字段；黄金样本比对时这两个字段掩码，形态另用断言检查 |
| materials Schema 冻结（2026-09-14，选项 A） | 冻结 `aliases`、`thicknesses`、模式节点、参数格字段和空值规则，规范文本在 db-schema.md §3.6.2，`laser-contracts` 按其建强类型；变更需走 §24 |
| MCP 工具注册集合（2026-09-15，D1 已冻结） | 源 Python 当前测试与 `server.py` 实际行为显示默认 20 个工具，包含 `command_execution_tool`；`homeasstant_tool`、`lasergrbl_gui_tool` 不注册，摄像头仅在 `ENABLED_IP_CAMERA=true` 时注册。本期不臆造未存在的开发者开关；Rust 黄金样本以该实际集合为准 |
| 校准发送门禁（2026-09-14） | `jobs.workflow_id` 可空；校准 job 增加可空 `calibration_id`。发送前必须满足 workflow 快照门禁，或校准会话存在且其 G-code SHA 与当前文件重算值匹配，二者至少一个成立；无任何匹配凭据不得发送 |
| RuntimeRoot 文件目录（2026-09-14） | 运行时固定使用 `<RuntimeRoot>/uploads`、`<RuntimeRoot>/previews`、`<RuntimeRoot>/generated_assets`。仓库根 `out/` 仅作为旧数据迁移输入扫描，不作为 Rust 运行时写入目录；旧产物仅在用户明确执行迁移时导入 |
| 取消协议（2026-09-14） | 只保留 `!` → 停止顺序流 → `?` 轮询 Hold → `0x18`；删除 `M5` 备选。网络心跳可能穿插 `?`，验收按子序列判断 |
| 材料数值字段（2026-09-14） | `manage(save)` 的功率、速度、阈值、次数、像素尺寸等数值字段在入库边界统一转为对应数字类型；无法转换返回 400；导出统一输出数字。字符串数字不保留为字符串 |
| 残留任务启动处理（2026-09-16，D6 已确认，选项 A） | 启动恢复时，将数据库内历史残留的 pending 与 running/sending 任务一并标记为 failed，注明“服务重启中断”；绝不自动重发，彻底杜绝崩溃重启后激光意外出光风险 |
| 多厚度材料保存（2026-09-16，D9 已确认） | 采用原子批量保存方案：`/api/material-lab/manage` 支持一次性提交材料及其多个厚度（`thicknesses` 列表），后端在单事务中执行，失败整体回滚；禁止前端在浏览器中通过 for 循环发起 N 次 HTTP 请求，彻底消除部分成功/部分失败的脏数据隐患 |
| 实验服务归属（2026-09-16，D4 已确认） | `apps/excalidraw_lab/server.py`（8777 端口）暴露的 5 条路由被主服务同名路由全面覆盖，属于早期画板验证的副本，不单独迁移或启动独立端口 |
| 错误文案比对排除清单（2026-09-16，D11 已确认） | `/api/ui/serial-ports` 缺少 pyserial 依赖等 Python 环境特有报错在 Rust 原生探测下无法自然产生，列入黄金样本比对排除清单（验证契约结构与字段，文本免逐字匹配） |
| 状态词表归一化映射（2026-09-17，D10 已确认） | 并列规范三套词表（workflows.status、jobs.status、runtime_job.status）；前端状态机入口统一执行 `queued → pending`、`canceled → cancelled` 别名归一化；发送覆盖层严格按 4 步映射（步骤 0 等待、1 准备、2 发送中、3 完成，终止态停轮询） |
| 原生移动端归属（2026-09-17，D7 已确认） | 旧 Android 原生文字预览工程不再维护，由同一前端工程构建的 Tauri 移动端统一承接，旧 Android 原生代码纳入阶段 12 清理删除清单 |
| iOS 跨平台云端构建（2026-09-17，D12 已确认） | 鉴于本地 Windows 环境受苹果生态限制无 Xcode，iOS 应用采用 GitHub Actions 官方托管的 `macos-14`（真实 Apple Silicon 苹果机）Runner 实现云端跨平台矩阵编译；生成未签名应用包并通过 CI Artifacts 供下载，用户可在 Windows 下使用爱思助手、AltStore 或 Sideloadly 免越狱安装至 iPhone |

## 23. 执行授权边界

本文获批后允许：安装 §11 工具链；创建 §12 新文件；只读调用旧 Python 生成样本；用真实 token 做握手；按 §4.2 实现默认禁用的命令执行开发者开关。
仍需单独确认：任何真实硬件操作；把命令执行能力扩大到非本地或非开发者模式；更新服务器与签名；删除旧内容；任何 git 提交或推送；弱化安全、扩大远程权限、改变真实设备行为的方案。

## 24. 变更控制

需求含义、平台范围、安全规则、迁移范围或完成标准变化时先改本文第一部分并重新确认；仅依赖、命名或等价实现变化时改第二部分；无法满足的需求标 `partial` 或 `blocked`，不降低标准宣称完成。
