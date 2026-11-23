# MOSS 融合实施 TODO

实施进度的**唯一记录处**。设计看 [融合技术书](moss-desktop-fusion-technical-spec.md)、[内置语音技术书](moss-native-voice-spec.md)；NV10～NV19 步骤看 [实施细则](moss-native-voice-implementation.md)。规则、验证与结果状态见 [`AGENTS.md`](../../AGENTS.md)。

## 0. 开工前

1. 看 §1 状态表，找到要做的工作包，再到对应章节找第一个未勾选步骤及其前置条件。
2. `git status --short` 区分本任务改动和用户原有改动，不覆盖、不回滚。
3. 中途续做时先按该步骤的“检查”逐项核对，已满足的跳过。
4. 设备运动/出光/急停实测、固件刷写或 OTA 地址修改、部署服务、写真实密钥、提交推送：先获得用户同意，本文件写了也不能直接做。
5. 完成一步就勾选，并在 §9 追加一行。

标记：`[ ]` 未开始　`[~]` 进行中　`[x]` 完成　`[!]` 阻塞（§9 写原因）　`[-]` 不做（写原因）。
子步骤写“软件完成”时，`[x]` 只覆盖实现和自动检查，不代表实机、真人、真实账号或安装已验收。

## 1. 当前状态（2026-11-15）

**2026-11-15 用户决定清空待办**：实际已完成的项一律勾选；下列不再排期并从本清单删除——外部自建链路的前置验证与联调（P0-01～04、P0-09、P2a-01～04/06、P2b、P3）、可选项（VO-2、VO-3、P5）、全部实机验收（原 §2a HW-01～21 与 §10 FINAL-01～05，以及 S1-09、UI-4 桌面验收、P1-13、NV8b-1、各 NV 的 -5 实机子项）、PKG-06 签名、PKG-07 干净机验收、NV18-3 原生依赖许可闭环与 NV18-4 安装验收。自建链路代码（P4）保留可用（**2026-11-17 用户决定已连代码删除**，见下表与 §9）。以后要做实机验证，按 [MOSS 实机验证顺序与操作手册](moss-hardware-acceptance-plan.md) 另起任务；设备运动、出光、急停实测、刷写等仍按 `AGENTS.md` 安全边界逐次取得同意。

| 工作包 | 状态 |
| --- | --- |
| S1 来源校验、S2 顶栏状态、UI-2 / UI-3 / UI-4、EXP 导出、LINK 手机连接 | 软件完成 |
| PKG 安装包 | PKG-01～05 完成；Android release 已接签名，Windows 代码签名不做 |
| P0 前置验证 | P0-05～08 完成（固件与 OTA、部署位置已定），其余删除 |
| P2a / P2b / P3 外部自建链路 | 只保留已完成的 P2a-05 提示词，其余删除 |
| P1 调用方上下文与审批 | 完成（P1-05 不做） |
| P4 语音中心 | 完成；2026-11-17 删除自建服务端链路（P4-02 进程管理、P4-03 退出联动、P4-04 服务端日志、P4-06 模型向导及 P4-01 / P4-05 中的自建部分），语音链路只剩官方小智与 MOSS 内置服务 |
| NV0～NV19 内置语音 | 软件完成 |
| VO-1 语义判停 | 已实现（n86、n93）；VO-2、VO-3 不做 |

2026-10-16 以后的功能变更未逐条补记到本清单，以 [CHANGELOG](../../CHANGELOG.md) 与内置语音技术书修订记录为准。最近的交付构建见 [构建回执](../developer/build-receipts.md)。

---

## 2. 通用检查

按 [`AGENTS.md`](../../AGENTS.md)“验证”执行：每个代码步骤跑受影响测试，阶段收尾跑三端 release 构建；改契约、表、工具、提示词时按融合技术书 §13 同步文档。

---

## 3. S1 CORS 收紧与来源校验（独立于融合，默认始终生效）

目标：去掉 `Access-Control-Allow-Origin: *`，不允许任意网页跨站调用 MOSS。设计见技术书 §6.5、D11。
前置条件：用户已确认开始写代码。
属于**破坏性变更**，CHANGELOG 必须标注。

- [x] **S1-01 读现状**
  - 做：阅读 `crates/laser-api/src/lib.rs` 中 `ApiService::serve`、`handle_http_from`、`http_options`、`http_json`、`http_json_with_headers`、`http_bytes`、`http_bytes_with_disposition`、`http_sse` 以及两处测试 `http_adapter_answers_real_loopback_request`、`http_adapter_handles_cors_options_preflight`；搜索所有写 `Access-Control-` 的位置。
  - 检查：列出所有输出 CORS 头的函数清单（写进 §9 日志）；确认响应构造函数目前拿不到请求的 `Origin`。
- [x] **S1-02 定方案**
  - 做：选定注入方式——推荐“响应构造函数不再写任何 CORS 头，由 `handle_http_from` 在拿到响应后按请求 `Origin` 统一注入”，避免改动几十个调用点。
  - 检查：方案写入 §9 日志；确认 `InProcessBackend` 走的 `handle_http` 不带 `Origin`，行为不变。
- [x] **S1-03 允许来源判定函数**
  - 做：新增纯函数 `is_allowed_origin(origin, host_header, loopback, extra) -> bool`。规则：`http://tauri.localhost`、`tauri://localhost`；与 `Host` 同源；回环来源时允许 `http://127.0.0.1:5177`、`http://localhost:5177`、`http://127.0.0.1:5178`、`http://localhost:5178`；加上 `MOSS_ALLOWED_ORIGINS`。精确匹配 scheme+host+port，不做前缀匹配。
  - 检查：单元测试覆盖——允许项逐一为真；`http://tauri.localhost.evil.com`、`http://evil.com`、`null`、空字符串、大小写变体、端口不同、非回环来源下的 5177 为假。
- [x] **S1-04 读取 `MOSS_ALLOWED_ORIGINS`**
  - 做：在设置/环境读取处解析逗号分隔列表，去空白；含 `*` 时启动失败并给出明确错误。
  - 检查：单元测试——空值、单个、多个、含空格、含 `*` 报错。
- [x] **S1-05 来源校验（拦截）**
  - 做：在 `handle_http_from` 中、进入路由之前：请求带 `Origin` 且不允许时，`GET`/`HEAD` 以外一律返回 403 `origin_not_allowed`（不带 CORS 头）；`OPTIONS` 对不允许来源返回 403。
  - 检查：测试——不允许来源的 `POST /api/workflow`（`Content-Type: application/json` 与 `text/plain` 各一）返回 403 且**业务未执行**（用 fake/内存存储断言无新记录）；不带 `Origin` 的 POST 行为不变。
- [x] **S1-06 响应头注入**
  - 做：允许来源时回显 `Access-Control-Allow-Origin: <origin>` + `Vary: Origin`；`Allow-Methods` 保持；`Allow-Headers` 改为 `Content-Type, Authorization, Accept, X-Moss-Desktop-Session`。不允许或无 `Origin` 时不输出任何 `Access-Control-Allow-*`。
  - 检查：JSON、文件下载、静态资源、SSE、`OPTIONS` 五类响应各一条测试；全仓搜索确认不再出现 `Allow-Origin: *`。
- [x] **S1-07 更新旧测试**
  - 做：改写 `http_adapter_answers_real_loopback_request`、`http_adapter_handles_cors_options_preflight` 中对 `*` 的断言，改为断言允许来源回显、不允许来源无头/403。
  - 检查：没有删除断言或放宽断言；`cargo test --workspace` 全绿。
- [x] **S1-08 前端与 E2E 核对**
  - 做：确认前端请求后端的方式（Tauri 内嵌页经 `get_backend_origin`、同源页、Vite 开发页）都在允许范围；运行 E2E。
  - 检查：`npm test`、`npm run test:e2e` 通过；若 E2E 使用其他端口或来源，把它加入允许规则并记录原因。
- [x] **S1-10 文档与 CHANGELOG**
  - 做：`CHANGELOG.md` 新增条目（标注破坏性变更；写清现象与影响、根因、修复、验证、关联、默认值、升级动作：自定义前端来源需配置 `MOSS_ALLOWED_ORIGINS`）；更新 `docs/developer/api-contracts.md` §1 与 §5、`docs/developer/safety.md` §2 局域网访问、`docs/user/quick-start.md` 排障。
  - 检查：文档中所有 `*` 描述已改；文档不含真实地址。
- [x] **S1-12 DNS 重绑定防护**（2026-10-06 用户要求提前修复，原计划随 P1 会话凭据解决）
  - 做：回环连接校验 `Host`（IP 字面量 / `localhost` / `MOSS_ALLOWED_ORIGINS` 主机名），不通过时任何方法 403 `host_not_allowed`。
  - 检查：`loopback_host_rules_block_dns_rebinding_names`、`dns_rebinding_request_is_rejected_before_routing`；本机 `cargo test --workspace`。
- [x] **S1-11 三端构建与收尾**
  - 做：§2 全部检查。
  - 检查：验收 A-29 通过；报告实际产物路径；勾选本阶段并更新 §1。

---

## 3a. S2 顶栏真实状态（独立修复，技术书 §9.5.3）

前置条件：用户已确认开始写代码。与融合无关，可在 S1 前后任意时间做。

- [x] **S2-01 读现状**
  - 做：读 `crates/laser-api/src/lib.rs` 中 `/api/system/info` 分支（`mcp_configured`、`client_name`、`recent_events`、`messages_count`、`tools_count`、`last_request`）、`laser_contracts::global_mcp_telemetry`、`laser_mcp::run_bridge` 中更新遥测的位置、`frontend/src/components/Navbar.jsx` 小智胶囊。
  - 检查：在 §9 日志列出每个编造值的代码位置。
- [x] **S2-02 遥测记录握手状态**
  - 做：遥测单例增加“已握手/已断开”状态，`run_bridge` 在握手完成与断线时更新。
  - 检查：单测模拟握手与断线，状态正确切换。
- [x] **S2-03 去掉编造值并新增 `mcp.connected`**
  - 做：`client_info`、`recent_events`、`last_request` 无真实数据时返回空；`messages_count` 取真实值；`tools_count` 取 `tools_list_json(developer_mode)` 实际长度。
  - 检查：A-72——已配置未连接时无任何编造值；开发者开关开/关时 `tools_count` 分别为 20/19；没有放宽现有断言。
- [x] **S2-04 顶栏四种状态**
  - 做：`Navbar` 小智胶囊按 未配置 / 未连接 / 等待设备 / 已连接 显示（语音中心未上线前链路固定“官方”，没有“等待设备”）。
  - 检查：A-73；`npm test`。
- [x] **S2-05 文档与 CHANGELOG、三端构建**（2026-10-10 同步：软件、文档及后续测试和三端构建已有完成记录；真实接入点断开 / 恢复验收单独保留在 HW-05，不计为已实测）
  - 做：更新 api-contracts 的 `/api/system/info` 字段；CHANGELOG 写明“此前小智状态显示不可信”的现象、根因、修复、验证。
  - 检查：§2 全部检查。

## 3b. UI-2 前端整体改版（技术书 §9.5.5；已按确认版实施）

用户已确认最终原型版本。历史原型（曜石设计 + 流浪地球太空舱背景）可用 `git show 1e77350^:frontend/prototypes/forge-v2.html` 取回。当前设计以现行代码与文档为准。

- [x] **UI-01 设计定稿**（2026-10-06 用户确认：按技术书 §9.5.5 与原型实施，背景为等离子流体。历史原型取回方式见本节开头）：确认记录和实现已同步§9及技术书；后续新设计需另行明确范围，不重复把已确认事项记为待定。
- [x] **UI-02 令牌与基础组件**（2026-10-13 完成：299 处纯静态 `style={{…}}` 迁为 217 个类，按组件分段追加在 `styles.css` 末尾；其中 90 个与已有更高优先级选择器冲突的类重复书写以保持原内联效果。剩余 29 处依赖运行时值（进度宽度、按状态着色、传入样式变量）按设计保留内联。验证：新旧两版在同一浏览器、1440×900 下逐元素比对计算样式，12 个界面状态中迁移相关差异为 0，余下差异均为呼吸灯动画与导航/标签过渡时序；`LogPanel` 未被任何页面引用、`CloseConfirmModal`/导出设置仅 Tauri 显示，这些未渲染部分依赖机械转换与优先级分析，未逐元素实测）：`styles.css` 令牌（色彩、圆角、阴影、字体）、按钮、输入、分段、卡片聚光、表格（加粗表头 / 斑马纹 / 悬停）、胶囊、弹窗；把 `Navbar.jsx` 等组件的内联样式迁移为类名。检查：旧样式无残留冲突；hover / disabled / focus 清晰。
- [x] **UI-03 外壳与背景（软件）**：`SideNav.jsx`左侧导航、`Navbar.jsx`标题/硬件/小智四态/同一`0x18`常驻急停、`CyberBackground.jsx`的WebGL等离子流体及Geist/OFL本地字体已实现；字体固定来源与实际加载见§9。背景采用静态层离屏预渲染、WebGL半分辨率、40fps限制、低帧率降级与WebGL不可用回退；reduced-motion和窄屏规则已有软件检查。前端/FakeGrbl/Windows/Android已有阶段A回执，后续代码须以本次新构建为准。目标电脑实际显卡帧率/CPU及加工中表现仍留HW-06，不能用软件构建代替。
- [x] **UI-04 雕刻工作台改版**（三栏已完成：①通信链路 ②加工内容 ③材质与参数 / ④加工监视舱 / 加工反馈 + 控制台；待按 H1–H26 逐项真机核对）：三栏布局，按功能对照清单 H1–H26 逐项核对。检查：A-80 对应项。
- [x] **UI-05 创作画板改版**（左侧大画布 + 右侧 材料与尺寸 / 设备 / 预览与发送 三卡已完成；待按 D1–D21 真机核对）：D1–D21 逐项核对；重复发送防呆不变。检查：A-80 对应项。
- [x] **UI-06 材料实验室改版**（材料库改为表格 + 厚度胶囊，三栏版面已完成；待按 L1–L15 真机核对）：材料库表格化，L1–L15 逐项核对；`BroadcastChannel` 同步不变。检查：A-80 对应项。
- [x] **UI-07 弹窗改版**（弹窗统一曜石样式；待逐个核对 S1–S9、A1–A3、C1、O1）：偏好设置、配对、关闭窗口、材料编辑、确认发送、发送覆盖层（`STAGE_MAP` 不变）。检查：S1–S9、A1–A3、C1、O1。
- [x] **UI-08 验收与构建**（视觉基线已生成于 `frontend/tests/visual-baseline/`；用户本机测试与构建通过；需实机的 HW-01、HW-02 见[实机验证手册](moss-hardware-acceptance-plan.md)，2026-11-15 不再排期）：更新 `frontend/tests/visual-baseline/`；E2E 不改断言通过；三端构建；CHANGELOG。检查：A-79、A-80、§2 全部检查。

---

## 3b2. UI-3 语音中心按链路分组与分组保存（技术书 §9.5.1 r15）

用户决定（2026-10-14）：语音中心内容太多、保存对象不清楚。当前链路用不到的设置折叠在后（需要时展开），每组手动点保存。

- [x] **UI-3 改版**（2026-10-14）：顶部选链路 + 一行状态，下分服务与设备 / 模型 / 连接 / 功能扩展 / 记录与诊断五个标签；`ModeSplit` 把其他链路的部分折叠在后，组件不重建、不丢填写；`VoiceGroup` 折叠摘要 + `SaveBar`“保存{对象}”、没改动不可点、未保存标记（分组与标签）、结果显示在按钮旁；原“服务端与接入点设置”拆为四组只提交本组字段，本地模型与语音体验分开保存，云端三项各自一组；音乐 / 视觉 / Agent 卡补齐样式，Agent 放入默认折叠的“实验功能”。检查：`npm test` 113、`npm run build`、`tests/voice-center.browser.mjs` 与其余 4 个语音浏览器测试、E2E 32/32；视觉基线 `voice-*.png` 重新生成。真实桌面窗口目视与保存见 HW-15。

## 3b3. UI-4 隐私与家居任务入口（技术书 n25）

- [x] **UI-4 页面收敛**（2026-10-18）：语音中心增加“隐私与家居”，主窗口四个入口只负责打开已打包的私密页面对应任务；固件入口归“服务与设备”。私密页默认只显示四个任务，按需渲染表单并在切换任务时保留未保存草稿；信息服务只显示当前选择的服务，记忆高级项及家居备份/只读同步收进次级入口。没有为普通主窗口增加敏感读取或控制权限。

## 3c. PKG 安装包与卸载（技术书 §9.6；规范 docs/developer/packaging-install.md）

前置条件：用户确认开始做分发；建议在 UI-2 前端改版之后、对外分发之前。需要一台干净的 Windows 虚拟机做验收。

- [x] **PKG-01 打包配置（软件）**：`tauri.conf.json`配置NSIS、both安装范围、中文语言、MOSS开始菜单和embedBootstrapper，普通`cargo tauri build`已有实际EXE/NSIS回执，见§9。本次新代码仍须最新重建。正式发布者/签名信息待用户提供；安装实测原归PKG-07，2026-11-15 不再排期。
- [x] **PKG-02 安装版配置读取（软件）**（2026-10-06 用户确认安装版运行目录`.env`生效）：`config.rs::load_env_files`先启动目录再运行目录且不覆盖已有变量，`RuntimeRoot::default_base()`与migrate默认目标已实现。相关4项单测包含在workspace回归，release moss-host已重新构建，见§9；本次未安装程序，干净环境快捷方式启动仍归PKG-07/PK-04。
- [x] **PKG-03 安装/卸载钩子**（已写 `installer/hooks.nsh`；待 PK-06/07/08 实测）：`installer/hooks.nsh`——安装、升级、卸载前按进程名检测 `moss-desktop.exe`、`moss-host.exe`、`moss-voice-local.exe`（helper 于 2026-10-13 补；原计划的 `moss.lock` 检测未采用），运行中只提示退出不强杀；卸载复选框“同时删除我的数据”默认不勾，勾选才删固定路径 `%APPDATA%\moss`（`APPDATA` 为空跳过）；不碰外部服务端目录。检查：PK-06、PK-07、PK-08。
- [x] **PKG-04 安装器图片**（已生成 header / sidebar；待安装界面人工确认）：横幅与侧栏图按新前端视觉制作。检查：安装界面观感人工确认。
- [x] **PKG-05 交付流程（软件文档完成）**（2026-10-11 用户授权按顺序完成非实机待办）：AGENTS 增加本轮普通 NSIS 非空、时间/大小/SHA-256 检查，并与 NV18 专用包分别记录；README 链接安装与卸载入口，文档索引对齐已实施现状。签名及干净机验收仍归 PKG-06/07。

---

## 3d. EXP 导出目录设置（技术书 §9.7）

前置条件：无（独立小功能，可随时插入）。只改桌面壳、前端与文档；Android 与浏览器行为保持不变。

- [x] **EXP-01 读现状**（与技术书 §9.7.1 一致；壳层经 `HostApplication.runtime_root` 定位 `settings.json`）：核对 `save_export_file`、`sanitize_export_name`、`unique_export_path`、`saveFile.js` 与全部调用方是否与技术书 §9.7.1 一致；确认运行目录 `settings.json` 在壳层的定位方式。检查：差异写入 §9 日志并回修技术书。
- [x] **EXP-02 壳层命令**（`src/export.rs`；已登记 build.rs 与 capability，测试覆盖登记一致性）：加入 `tauri-plugin-dialog`（仅桌面）；新增 `get_export_settings`、`pick_export_dir`、`reset_export_dir`、`set_export_ask_each_time`、`open_export_dir`，改造 `save_export_file`；全部登记到 `capabilities/default.json` 与 ACL。检查：新 exe 启动不出现“命令未登记”失败页。
- [x] **EXP-03 存储与校验**：`settings.json` 的 `export.dir`、`export.ask_each_time` 读写（保留其他键、临时文件 + 重命名）；目录存在 / 可写探测 / 路径逃逸检查做成纯函数并单测；`/api/settings` 不新增该字段。检查：A-86。
- [x] **EXP-04 前端**：`SettingsModal` “导出”分区（路径、选择文件夹、恢复默认、打开文件夹、每次询问开关）；`saveFile.js` 处理 `export_dir_unavailable` 与取消；成功提示加“打开所在文件夹”；非 Tauri、Android 隐藏分区。检查：前端单测覆盖各分支。
- [x] **EXP-05 文档与记录**：CHANGELOG、前端行为契约、快速开始、安装包规范（卸载不删导出目录）。
- [x] **EXP-06 验收与构建**（2026-10-06 用户本机构建并通过 A-82..A-85、A-88 人工测试）：`cargo test --workspace`、`npm test`、`npm run test:e2e`、`npm run build`、`cargo tauri build`、Android APK；本机人工执行 A-82..A-85、A-88。检查：结果记入 §9，未做项标 `unverified`。

---

## 4. P0 前置验证（不写 MOSS 代码）

前置条件：无（可与 S1 并行）。产出写入 `docs/integration/self-hosted-model-bench.md`（只写占位地址，**不写密钥**）。

- [x] **P0-05 锁定上游源码版本（不代表已部署）**
  - xiaozhi-esp32-server：`87c6df77b0220ddbd8fc804f984f83f69f3225e5`，MIT；mcp-endpoint-server：`0f9302f327100fb976c7bdea6e4ebbb8ebdb5787`，MIT，2026-10-11 官方仓库公开提交/许可核对。
  - 检查：固定版本与许可链接写入 [bench 文档](self-hosted-model-bench.md)；未检出/安装/启动服务，运行兼容仍留 P2a。xiaozhi-client 属 P0-09/P5 可选范围，未决定时不作为本项硬前置。
- [x] **P0-06 设备固件与 OTA 地址**（已定：立创实战派 ESP32-S3 用官方固件，OTA 地址经 USB 一键配网写入，技术书 n52、n96）
  - 做：查明实战派 ESP32-S3 当前小智固件版本，以及修改 OTA 接口地址的方式（配网页面或重新编译）。
  - 检查：写出操作步骤；**实际刷写/修改需用户授权**。
- [x] **P0-07 上游插件与接入点资料**（2026-10-05 源码阅读完成，结论见技术书 §2“上游源码核实”；运行验证留给 P2a）
  - 做：阅读上游 `plugins_func` 插件写法（入参、返回、能否异步播报）、`Intent: function_call` 配置、MCP 接入点部署方式；并为语音中心确认：源码启动命令与入口脚本、用户覆盖配置文件名及 ASR/LLM/TTS 字段名、服务端监听端口、是否有设备在线状态接口。
  - 检查：P2a、P2b、P4 所需信息齐全，写入 bench 文档。
- [x] **P0-08 部署位置**（已定：语音服务内置在 MOSS 桌面端，见内置语音技术书）
  - 做：与用户确定 xiaozhi-server 运行在哪（推荐与 MOSS 同一台电脑、源码部署）。
  - 检查：若用 Docker，记录防火墙限制方案（技术书 §9.1）。

---

## 5. P2a 接入点零代码验证（只读与预览）

前置条件：P0-01..P0-08 完成。**本阶段禁止任何真实发送和 R3 动作**（严格模式未上线，模型可自行确认）。建议激光机断电或不连接。

- [x] **P2a-05 自建系统提示词（文档完成，未部署）**
  - 做：编写自建服务端系统提示（路由表、“开始加工需到电脑确认”、“停”只映射 `cancel`/`emergency_stop`）；存 `docs/integration/xiaozhi-prompts/history/<日期>-self-hosted.md`，在 `latest.md` 注明官方/自建两套的适用范围。
  - 检查：2026-10-11 自建文本已保存至 `history/2026-10-11-self-hosted.md`，latest 区分官方/自建适用范围；预算和链接检查后记录 §9。真实工具发现、播报与延迟仍未验，不写入外部配置。

---

## 6. P1 调用方上下文、网关与审批（全部用 fake 验证）

前置条件：S1 完成；P2a 完成或已明确放弃接入点。设计见技术书 §5–§7。

- [x] **P1-00 纯签名改造（单独一步，不改任何判定）**（2026-10-06 用户本机 `cargo test --workspace` 通过；上下文在 `route_http` 鉴权后构造，因为局域网 `pairing_id` 要鉴权后才知道；只读的 `handle()` 不含执行动作，未加参数）
  - 做：在 `laser-contracts` 定义 `CallerContext`；`serve` 按来源与头部构造上下文；`handle_http_from` / `handle` / workflow、text-task、material-lab、safe-action、framing 处理函数 / `launch_send_job_for` / `handle_safe_action` 增加 `ctx` 参数；新增 `handle_http_with_ctx`，`InProcessBackend` 传 `McpBridge`；HTTP 入口删除请求体中的身份类字段。
  - 检查：`cargo test --workspace` 全绿且**测试未修改断言**；diff 中只有签名与参数传递，没有新判断逻辑；A-36。
- [x] **P1-01 强类型**（2026-10-06 用户本机 `cargo test --workspace` 通过；按技术书 r9 §10 P1-1，`GatewayCallRequest` 移至 P2b，本步未做）
  - 做：`laser-contracts` 中 `GatewayCallRequest`、`ApprovalRecord`、`ApprovalStatus`、`AuditDetail` 与 JSON Schema。
  - 检查：`type_boundary` 测试通过；序列化往返测试。
- [x] **P1-02 schema v4**（2026-10-06 用户本机 `cargo test --workspace` 通过；`gateway_requests` 按技术书 r9 移至 P2b，本步未建）
  - 做：`laser-runtime/src/db.rs` 追加 `if current < 4` 分支：`approvals`（含 `connection` 列）、`gateway_requests`；`audit_log` 写入方法。
  - 检查：A-41——内存库迁移、重复执行幂等、中断重试；用 v3 迁移逻辑打开 v4 库正常。
- [x] **P1-03 审批服务**（2026-10-06 用户本机 `cargo test --workspace` 通过；接入执行入口见 P1-04，桌面接口见 P1-07）
  - 做：创建（解析显式目标，否则 `target_required`；保存连接参数）、digest 计算、批准、拒绝、过期、单次消费（事务内 `approved→executed`）、重启时 `pending/approved→expired`。
  - 检查：A-22、A-23、A-32、A-35 单元/集成测试。
- [x] **P1-04 严格模式**（2026-10-06 用户本机 `cargo test --workspace` 通过；桌面会话在 P1-07 接入前，开启严格模式会把桌面端自己的发送也当作回环拒绝，暂不在实际使用中开启）
  - 做：实现 `require_r3_authority(ctx, kind, digest)`，接入所有 R3 执行入口；`MOSS_FUSION_STRICT` 开关；网关开启而严格模式关闭时启动失败。
  - 检查：A-24、A-25、A-26；R2 停止类不受影响（A-27）；严格模式关闭时 A-40 全量回归。
- [-] **P1-05 网关路由**（r5：移至可选 P2b-00；正式链路走接入点，不需要网关）
  - 检查：A-20（模型经 MCP 桥传 `confirmed=true` 仍只得到 `approval_required`）、A-28 改在 P1-06 中验证。
- [x] **P1-06 工具分级**（2026-10-06 用户本机 `cargo test --workspace` 通过；分级函数放在 `laser-contracts::risk`，便于以后 laser-api 中的网关复用）
  - 做：`laser-mcp` 中 R0–R4 分级函数；严格模式下 MCP 桥（接入点）调用的 R3 返回 `approval_required`；以后 P2b 网关复用同一函数。
  - 检查：每个工具/action 都有分级（测试遍历黄金清单，缺一个就失败）；A-20、A-25、A-28。
- [x] **P1-07 桌面会话与审批接口**（2026-10-06 用户本机自动测试、构建与严格模式实测通过：桌面端发送通过严格模式检查，浏览器发送被拒，`/v1/approvals` 无会话返回 `desktop_session_required`；批准时立即以审批身份重放原请求，原请求参数存入 schema v5 `approvals.request`）
  - 做：Tauri 命令 `get_desktop_session`；`/v1/approvals` 列表、批准、拒绝；Android 编译下命令返回不可用；`authSession.js` 在 Tauri 环境取会话只存内存，`api/client.js` 的 `apiFetch` 统一加 `X-Moss-Desktop-Session`，403 时重取会话并只重试一次（技术书 §9.5.4）。
  - 检查：无会话头/错误会话头调用审批接口被拒；A-21、A-75、A-76。
- [x] **P1-08 审计**（2026-10-06 用户本机测试与构建通过）
  - 做：审批各状态、删除 `confirmed`、R4 拒绝、严格模式拦截写 `audit_log`；写入失败时 R3 拒绝、R2 照常。
  - 检查：A-34。
- [x] **P1-09 前端**（2026-10-06 用户本机测试与构建通过；真实流程见[实机验证手册](moss-hardware-acceptance-plan.md) HW-07（2026-11-15 不再排期）；E2E 未加入——现有 `test_app_buttons.mjs` 依赖真实后端且无法在测试中开启严格模式与桌面会话，改用预览桩脚本验证）
  - 做：全局审批卡（技术书 §9.5.2）——挂在 `App` 根组件；每 4 秒轮询 `/v1/approvals?status=pending`；显示刀路预览、材料、功率/速度/遍数、目标设备（不显示 IP）、来源、摘要、120 秒倒计时；勾选现场确认后才能批准；`approval_mismatch` / `approval_expired` 在卡内显示原因；批准后接现有 `SendOverlay`（`STAGE_MAP` 不变），急停始终可见；顶栏“审批”按钮带红色数量角标。历史原型 TC-01..TC-04 可用 `git show 1e77350^:frontend/prototypes/voice-center.html` 查阅。
  - 检查：`npm test`、E2E 覆盖批准、拒绝、过期、摘要不一致；A-70、A-71、A-74。
- [x] **P1-10 故障场景**（2026-10-06 修复启动恢复后用户本机测试通过）
  - 做：补齐 A-31、A-33 集成测试（FakeGrbl）。
  - 检查：发送中崩溃不重放；审批 `outcome` 为结果未知。
- [x] **P1-11 文档与 CHANGELOG**（2026-10-06 文档-only：architecture §2/§3/§4a/§6、feature-glossary、api-contracts §3.6/§3.12/§3.13/§5、db-schema、safety、frontend-behavior §8、quick-start“严格模式”；网关、语音中心、提示词相关条目属 P2b / P4，届时再写）
  - 做：api-contracts、db-schema、safety、architecture、feature-glossary、frontend-behavior；CHANGELOG 记录默认值、契约、安全门禁、跨端影响。
  - 检查：技术书 §13 清单逐项勾。
- [x] **P1-12 三端构建与收尾**（2026-10-06 用户本机 `cargo test --workspace`、`npm test`、`npm run build`、E2E、`cargo tauri build`、Android 构建完成；验收与测试对应如下，A-10..A-13 属可选网关 P2b，本阶段不适用）
  - A-20：`strict_mode_actions_are_audited`、`strict_bridge_forwards_r3_for_approval_and_r2_unchanged`
  - A-21：`desktop_approval_executes_bridge_request_with_saved_target`、`strict_mode_turns_bridge_send_into_approval_and_executes_with_saved_target`
  - A-22：`changed_content_after_approval_is_a_mismatch`、`desktop_approval_rejects_changed_content`
  - A-23：`approve_then_consume_once`、`expired_approvals_cannot_be_approved_or_executed`、`strict_mode_safe_action_approval_is_single_use`
  - A-24 / A-26：`strict_mode_blocks_loopback_r3_without_touching_device`
  - A-25：`strict_mode_turns_bridge_send_into_approval_and_executes_with_saved_target`、`strict_bridge_forwards_r3_for_approval_and_r2_unchanged`
  - A-27：`strict_mode_never_blocks_stop_actions`（经接入点；网关在 P2b）
  - A-28：`strict_bridge_refuses_r4_tools`
  - A-31 / A-32 / A-33：`pending_request_expires_while_nobody_answers`、`restart_expires_open_approvals_and_workflow_can_be_reapproved`、`interrupted_send_is_marked_unknown_on_restart`、`restart_during_approved_send_marks_result_unknown_and_never_replays`
  - A-34：`audit_failure_blocks_r3_but_not_risk_reduction`、`audit_failure_refuses_r3_and_keeps_r2`
  - A-35：`blank_serial_port_without_default_requires_target`；A-36：`caller_context_is_built_by_entry_never_from_body`
  - A-40：`strict_mode_off_keeps_existing_behavior`、`non_strict_bridge_behavior_is_unchanged` 与全量回归、E2E
  - A-41：`schema_v4_*`、`schema_v5_*`、`newer_schema_is_opened_without_changes`
  - A-70 / A-71 / A-74：`tests/approvalState.test.mjs` + 预览桩脚本；真实流程见[实机验证手册](moss-hardware-acceptance-plan.md) HW-07（不再排期）
  - A-75 / A-76：`tests/authSession.test.mjs` 桌面会话用例、`approval_routes_require_desktop_session`
  - A-50：Android APK 构建与 E2E（用户本机）
  - 检查：§2 全部检查；A-10..A-28、A-35、A-36、A-40、A-41 通过；A-50 Android 构建。

---

## 7. P4 语音中心与内置语音


> 2026-11-17 用户决定删除语音中心的自建服务端链路（融合技术书 r50、内置语音技术书 n102）：下列 P4-02、P4-03、P4-04 的服务端日志、P4-06 与 P4-01 / P4-05 / P4-08 中的自建部分的代码已删除，勾选与说明保留为历史；旧设置 `voice.mode = "self_hosted"` 启动时迁移为内置服务。

**P4 语音中心**（前置：P1 已完成、P2a 已验证接入点及外部服务端配置；若 P2a 实测不兼容且用户选择 P2b 替代链路，则先完成相应 P2b 验证；**P2b 不是默认硬前置**。设计见技术书 §9.4）

- [x] **P4-01 设置项与模式备份**（代码完成：`crates/moss-voice/src/settings.rs`；官方接入点也改在语音中心编辑；待用户本机 `cargo test --workspace`）
  - 做：`settings.json` 新增 `voice.server_dir`、`voice.python`、`voice.args`、链路模式；切换模式前备份官方 `mcp_endpoint`。
  - 检查：读写单测；旧版 `settings.json` 无这些字段时正常加载；文档不写真实路径。
- [x] **P4-02 进程管理**（核心完成：`crates/moss-voice/src/supervisor.rs`；Tauri 接线在 P4-07；待用户本机测试——Windows 的 Job Object 与 cmd 假子进程只能在本机验证）
  - 做：在 `apps/desktop/src-tauri`（或桌面专用模块）实现启动、停止（正常退出 5 秒后强制）、重启；Windows Job Object；同一时刻只一个实例；端口探测识别外部实例。
  - 检查：A-62、A-63（用假子进程，不启动真实服务端）；Android 构建不编译此模块。
- [x] **P4-03 退出联动**（代码完成：`exit_application({stopServer})` 先急停再停止或放开服务端；托盘退出在服务端运行时弹对话框；关闭弹窗新增“同时停止语音服务端”；`Supervisor::detach` 解除 Job Object；待本机测试 A-64）
  - 做：托盘退出、窗口关闭退出时提示是否停止服务端；`ShutdownCoordinator` 先急停后处理服务端。
  - 检查：A-64；急停顺序相关的现有测试不变。
- [x] **P4-04 崩溃与日志**（代码完成：`redact.rs`、`logfile.rs`、监控线程；待用户本机测试）
  - 做：记录退出码；不自动重启；日志写 `logs/xiaozhi-server.log`，5 MB × 3 轮转，脱敏。
  - 检查：A-65、A-66。
- [x] **P4-05 链路模式切换**（代码完成：`moss-voice/src/center.rs` `set_mode` / `mode_changes`，`laser-host` `restart_mcp_bridge`，`ApiService::set_strict_mode`；用假宿主测 A-60 / A-61；待本机测试）
  - 做：官方/自建互斥；先断旧桥再接新桥；自建模式开启严格模式并把 `mcp_endpoint` 指向自建接入点（网关仅在启用 P2b 时开启），失败整体回滚；切换前确认框列出改动项。
  - 检查：A-60、A-61。
- [x] **P4-06 模型配置向导**（2026-10-13：经用户授权取得固定版本上游源码，A-67 的上游 `config_loader` 离线解析与合并校验已补，三种写入前文件各 29 项通过、全模块部署拒绝写入、反向对照能报错，见 [bench 文档 §2.1](self-hosted-model-bench.md)；真实服务端运行仍归 HW-11。代码完成：`models.rs` 测试连接、`config_yaml.rs` 按行读-改-写 + `.bak` + 重新解析核对、`center.rs` 先测试后保存；按行编辑逻辑已在云端用 PyYAML 离线校验；上游 `config_loader` 合并校验待有服务端源码时做）
  - 做：大模型、语音识别、语音合成**三个独立分区**，各自填地址、密钥、模型（语音合成另有音色、语速）；测试连接（内存密钥、15 秒超时、不跟随重定向）；保存按技术书 §9.4.4 字段表写入 `data/.config.yaml`（`LLM.MossLLM`：`url`/`api_key`/`model_name`；`ASR.MossASR`：完整 `/audio/transcriptions` 的 `base_url`；`TTS.MossTTS`：完整 `/audio/speech` 的 `api_url`、`model`、`voice`；`selected_module` 指向三者）；YAML 读-改-写保留其他内容并备份 `.bak`；留空的分区不改；检测到 `manager-api.url` 时只测试不写。
  - 检查：A-67——在 MOSS 的 SQLite、`settings.json`、日志中搜索测试用的假密钥，结果必须为空；写入后的 `data/.config.yaml` 可被上游配置加载器解析（用上游 `config_loader` 的合并逻辑做一次离线校验）。
- [x] **P4-07 接口与鉴权**（代码完成：以 Tauri 命令实现 `apps/desktop/src-tauri/src/voice.rs`，只有桌面窗口可调用；HTTP `/v1/voice/*` 全部 404（laser-api 新测试）；移动端命令直接返回 `voice_desktop_only`）
  - 做：`/v1/voice/*` 或等价 Tauri 命令；仅桌面会话。
  - 检查：A-68。
- [x] **P4-08 前端页面**（2026-10-14 补齐检查项：`tests/voice-center.browser.mjs` 在 Vite 回环 + 有状态假 Tauri 桥上覆盖链路切换确认框、切回时询问停止服务端、服务端启动 / 停止 / 意外退出；后端进程管理的假子进程（Windows `cmd`）由 `moss-voice` supervisor 测试覆盖，`cargo test -p moss-voice` 73 通过。仍未与真实 xiaozhi-server 联调，见 HW-11。2026-10-13 复核前记录：`pages/VoicePage.jsx`、`voiceState.js`、`voiceApi.js`；`npm test` 80 项通过；E2E 跑的是 `moss-host`，没有语音中心）
  - 做：`/voice` 页面（技术书 §9.5.1）：链路单选（切换确认框）、OTA 一致性提示、四张状态卡（服务端带启停重启）、三栏模型配置（先测试后保存、显示最终地址、密钥不回显）、审批记录、脱敏日志；导航第四个标签；`SettingsModal` 移除 MCP 地址并改为跳转说明；Android 隐藏入口。参照原型 TC-05..TC-08。
  - 检查：`npm test`、E2E 覆盖模式切换与启停（假子进程）；A-69、A-77、A-78；顶栏“等待设备”状态在自建模式下生效（A-73）。
- [x] **P4-09 文档与 CHANGELOG**（quick-start §8“语音中心”、frontend-behavior §9、api-contracts §3.15 / §5、architecture §4b、feature-glossary、CHANGELOG）
  - 做：quick-start 语音中心章节；feature-glossary、frontend-behavior、api-contracts；CHANGELOG。
  - 检查：技术书 §13 清单。
- [x] **P4-10 打包评估**（结论见 §9 进度日志 2026-10-07：首版不打包 Python 环境）
  - 做：评估是否把 Python 环境打进安装包；只写结论到 §9。
  - 检查：结论已记录，未实施。
- [x] **P4-11 三端构建与收尾**（2026-10-07 用户确认：`cargo test --workspace`、`npm test`、`npm run build`、E2E、`cargo tauri build`、Android 构建全部通过；与真实自建 xiaozhi-server 的联调未做，改由 NV 内置语音服务覆盖）
  - 检查：§2 全部检查；A-50、A-60..A-69 通过。

**LINK 手机连接电脑 MOSS**（技术书 §9.8；独立功能，由另一会话实施，2026-10-07 补记到本清单）

- [x] **LINK-1..LINK-5**：`web_host_from`（`LASER_WEB_HOST` > `lan_access` > `127.0.0.1`）、参考数据只读守卫与 `GET /api/system/lan-info`、前端远程模式（`remoteHost.js`、`PhoneLinkSection`、启动失败页）、Android 明文网络配置、CHANGELOG 与 quick-start §7 均已在代码中（见 CHANGELOG“手机 App 连接电脑 MOSS”）。检查：A-89..A-96 以该条 CHANGELOG 的验证记录为准；本清单未单独复核。

**NV 内置语音服务**（技术书 [moss-native-voice-spec.md](moss-native-voice-spec.md) n19；决定 V1–V4 已于 2026-10-07 确认：密钥存 `app_secrets`；范围含第三方 MCP、记忆、插件（天气 / 新闻 / 农历 / 搜索 / RAGFlow / 音乐 / Home Assistant）、视觉、Claude Code·Codex、声纹、固件下发；端口 8000 / 8003；保留外部服务端链路）

- [x] **NV0 前置**（2026-10-07 完成：依赖核实见技术书 §11.1；V5 已确认，本地模型放独立 GPL 辅助程序；本机实测见 §11.3——编译通过、Opus 默认纯 Rust 后端、SenseVoice 一句约 0.2 秒、Kokoro 首段 0.4–1.2 秒、辅助程序约 20 MB；`THIRD_PARTY_NOTICES.md`、`tools/nv0-probe` 已入库）
- [x] **NV1 协议与 OTA**（2026-10-07 代码完成，见技术书 §13.1；本机 `cargo test --workspace`、`npm test` 已通过；待真实设备验收 HW-08）：`moss-xiaozhi-proto`、`moss-voice-server` 骨架、设备允许流程、token、回声模式。检查：真实设备 OTA 指向 MOSS 后连上并听到回声；未允许设备被拒。
- [x] **NV2 音频**（2026-10-07 代码完成，见技术书 §13.2；Silero 随 NV6 接入，NV2 用能量 VAD；待真实设备验收 HW-08）：Opus、播放节奏泵、VAD、设备打断。检查：无断音跳字；200 ms 静音判停；打断立即停播。
- [x] **NV3 云端对话**（2026-10-07 代码完成，见技术书 §13.3；用假模型与假 HTTP 服务测试；桌面端启用随 NV5；待真实模型与设备验收）：OpenAI 兼容 ASR / LLM（流式 + 工具调用）/ TTS、切句聚合、历史截断、唤醒 / 退出 / 空闲。检查：完整对话走通并记录首句延迟（事件“首句延迟”已提供）。
- [x] **NV4 工具与审批**（2026-10-07 代码完成；当前规则见技术书 §5.2、§13.6，取代 §13.4 的始终严格规则；待真实设备验收）：统一工具路由、`CallerContext::Voice`、设备 MCP 中继；R3 跟随全局严格模式，开启时审批卡 + 语音提示，关闭时沿用现有 confirmed 契约与提示词口头确认；R4 始终拒绝。检查：按当前两档规则复测，不能继续以“始终严格”为预期。
- [x] **NV5 语音中心与密钥**（2026-10-07 代码完成，见技术书 §13.5；待真实模型与设备验收）：链路三选一、内置服务启停与状态、设备允许列表、提供商配置、密钥存 `app_secrets`。检查：密钥不出现在 `settings.json`、日志与接口响应（自动测试已覆盖）。
- [x] **NV6 本地模型**（2026-10-07 代码完成，见技术书 §13.7；真实模型端到端测试本机通过；安装包加入辅助程序留到 NV18；待真实设备）：辅助程序 `moss-voice-local.exe`（GPL-3.0）、Silero VAD（接入 NV2 的 `Vad` 接口）、sherpa-onnx ASR / TTS、模型下载与 SHA-256 校验。2026-10-08 起判停、识别、合成各有可选模型（技术书 §13.10）。检查：断网可用（识别、合成、判停全在本机；大模型仍需云端或本机 Ollama）。
- [x] **NV7 流式云厂商**（2026-10-08 代码完成，见技术书 §13.8；用假密钥验证了真实地址的 TLS 与鉴权；待真实豆包账号与设备验收首句延迟）：豆包流式 ASR / TTS。
- [x] **NV8 接入点增强**（2026-10-08 代码完成，见内置语音技术书 §13.9；待真实小智接入点验收多接入点、心跳与断网恢复）：多接入点、断线重连、工具开关（列表与调用都校验）、调用统计、接入点身份。
- [x] **NV8a 语音体验优化**（软件停止通道已按用户中间结果规则完成，本轮回归/三端/helper证据见§9；实机见HW-09）：
  - [x] **NV8a-1 停止类指令本地快速通道（软件）**：会话`select!`监听`AsrStream::next_interim`，整句停止词进入单次`FastStop`，不等待最终文本或声纹；`interim_whole_stop_executes_without_waiting_for_final_and_once`及本轮workspace/三端构建通过。临时“停止”随后补成“停止之后怎么办”仍可能提前停机（用户接受的取舍），不宣称消除误停；完整否定句若未经临时单句命中仍交普通识别。使用可信语音身份与原有`laser_safe_action_tool emergency_stop`通路；真实0x18/噪声仍归HW-09。
  - [x] **NV8a-2 常用只读指令本地匹配（软件完成）**：固定句式直接调用 R0 工具并用本地模板回答；不确定时交给大模型；默认开、可关闭；失败不编造。证据：技术书 §13.11、§9 联合验证记录。
  - [x] **NV8a-3 识别热词**：X-ASR beam search、云端请求字段、行业/自定义词、SenseVoice 不支持提示和 greedy 选项已有软件验证；真实热词识别率及相对 greedy 速度尚待记录，归 HW-09。
  - [x] **NV8a-4 本地合成边合成边播**：协议已升 3，分段返回和旧版握手拒绝已有自动验证及 helper 构建；真实模型长句首段耗时、设备无断音尚待 HW-09。分段返回不等于模型内部逐采样推理。
  - [x] **NV8a-5 各环节耗时记录（软件完成）**：最近 20 轮内存耗时、模型和字段白名单复制已接线/自动验证，不保存识别/回答正文；真实数字采集另归 HW-09。
- [x] **NV8b 自然打断（设备端回声消除）**（软件实现、7 项专项测试及发布构建见 §13.12/§9；设备 AEC 与真人噪声未验，整体 partial）：
  - [x] **NV8b-2 插话检测（软件完成）**：realtime、默认 400 ms、300 ms 开播保护、问候/告别保护、保留开头和 auto 模式不变已有假设备测试；真人效果归 HW-10。
  - [x] **NV8b-3 历史只记已开始播放的句子（软件完成）**：以已发 `sentence_start` 为准，未开始尾句不进入历史；这不证明用户实际听完该句。
  - [x] **NV8b-4 设置与事件（软件完成）**：默认关、时长 300～600 ms、设置往返与界面测试已有记录；真实插话事件与噪声误触发归 HW-10。
- [x] **NV9 第三方 MCP**（2026-10-10 四段集成已落地；发布构建见最新日志，真实服务未验证）：
  - [x] 桌面配置与凭据保存：主窗口及来源校验、配置版本、独立凭据引用、空白保留／移除字段删除、原配置保护。
  - [x] 连接维护：stdio / Streamable HTTP、独立有界队列、心跳／清单刷新、1–60 秒退避、故障不重放、全局预算和冲突提示。
  - [x] 持久审批与审计：external_tool、真实会话及当前目标／schema／凭据／参数校验、单次消费、配置变更及撤销失效、结果未知和审计失败路径。
  - [x] 语音聚合和统计页面：只进入内置语音、三档策略、参数审批卡、内存调用统计、持久审计；1280/960 像素假桥页面操作通过。
- [x] **NV10 记忆**（[六项实施细则](moss-native-voice-implementation.md#nv10)；软件功能与发布构建已完成，实际 GUI/真实语音模型/设备留 FINAL）：
  - [x] **NV10-1** schema v7、记忆表/CRUD、revision 条件更新、迁移与并发测试通过。
  - [x] **NV10-2** 已接入按设备摘要注入、会话断开后的有界后台总结，fake LLM 和过滤测试通过；真实模型质量另验。
  - [x] **NV10-3** 开关 generation、设备撤销 epoch、编辑/清空乐观锁，fake 竞态和隔离测试通过，旧结果不会覆盖清空。
  - [x] **NV10-4** 桌面记忆卡片、开关/列表/编辑/清空、主窗口专用 Tauri capability 均已开发；Rust 工作区 485/485、前端单测/构建、FakeGrbl E2E 32/32、Windows release/NSIS 与 Android release APK 构建通过。实际 GUI 按钮操作需 FINAL 验收，本项在此之前仍 partial。
- [x] **NV11 信息插件**（[六项实施细则](moss-native-voice-implementation.md#nv11)；五种插件软件已接线，未完成项和真实环境留 FINAL）：
  - [x] **NV11-1（供应商软件验收）** 六类供应商固定依据/合成fixture、本机真实TLS请求响应、鉴权/额度/头与正文超时/跳转/有长度和chunked上限/准确1MiB/缺字段/空结果/缓存失效去重已补；HTTP3秒连接/10秒总时限/1MiB、转义后外层2000字符、禁跳转及自动重试；原急停raw保留。定向33 PASS、最终workspace710/7 ignored及本次三端通过，见§9；真实提供方另留NV11-5。
  - [x] **NV11-2** 和风当前天气（配置地址/坐标、API Key/Bearer）和本机农历（1900～2100、显式日期、时区、春节/闰月测试）已接线；真实天气提供方待 FINAL 验证。
  - [x] **NV11-3（供应商软件验收）** RSS/NewsNow、Tavily、Metaso和RAGFlow适配及固定官方SDK/类型依据、六家fixture/TLS矩阵已验；Metaso普通搜索无需专题ID，独立凭据、不暴露文件元数据、HTTP200应用错误分类；NewsNow数字/字符串时间、RSS完整XML/实体、RAG非空来源与响应数据集白名单复核。天气/新闻5分钟缓存、去重及配置/凭据隔离通过，最终回归/三端见§9本次回执；真实源质量另留NV11-5/FINAL。
  - [x] **NV11-4** 桌面配置卡/default off/独立Keep-Set-Clear与供应商选择已接线，实际React+假native验证旧配置、Metaso密钥隔离、失败草稿保留及窄屏；管理仅packaged voice-private（不是main主窗口直接管理）。最新测试/构建见本次§9回执；跨settings文件/多secret存储故障的原子性与真实WebView2操作仍待验收，假桥不证明DB故障回滚。2026-10-13：保存改为密钥事务后写设置、失败还原密钥（`commit_secrets_then_settings`，单测覆盖设置写入失败回滚）；真实 WebView2 操作仍待验。
- [x] **NV12 视觉**（[实施细则](moss-native-voice-implementation.md#nv12)；软件与双端产物已验证，真实设备/模型留 FINAL）：
  - [x] **NV12-1** 独立视觉模型、非流式多模态请求/响应、独立配置及提供方本机 fake 4/4 通过；真实服务留 FINAL。
  - [x] **NV12-2** 8003 multipart、设备/client/token 只读鉴权与代次、图片体积/像素/MIME 门禁已实现；假设备 HTTP 5/5 通过，更全面的畸形上传、实际固件 fixture 待补。
  - [x] **NV12-3** MCP initialize 能力与真实端口、全局2/每设备1并发、撤销再允许不恢复旧请求均通过软件测试；超时/取消竞态与图片不落盘快照仍待更完整的覆盖。
  - [x] **NV12-4** 默认关闭的桌面模型、SQLite 密钥、合成图片连接测试及本地主窗口 ACL/React 卡已接线；Rust 511/511、前端测试/构建、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 通过；Android aarch64 编译完成，APK 内库与本轮新 `.so` 的 SHA-256 一致（Gradle 复用旧包时间戳）。
- [x] **NV13 Claude Code / Codex**（[实施细则](moss-native-voice-implementation.md#nv13)；2026-10-15 起按用户决定可用：用户在语音中心开启，每次运行在桌面单独批准，且只在严格模式运行）：
  - [x] **NV13-1** 精确版本/help 参数检查、可执行文件与已有工作目录的 canonical/symlink/junction/敏感根校验、固定 argv+stdin 已用 fake 测试；本轮经用户授权真实探测本机 `codex-cli 0.157.1` 及 sandbox/help，详细结果见 NV13 设计 §5；真实沙箱负向测试**发现越界读取和本机网络风险**。
  - [x] **NV13-2** AgentRun 类别和专用执行路由、voice permit、指纹重算、桌面单次批准、原子消费与审计已写入，假执行器端到端通过；默认不暴露 Agent 工具。
  - [x] **NV13-3** Windows Job Object 独立进程夹具：挂起创建→分配 Job→恢复，kill-on-close、最多 8 个进程、300 秒上限、取消/撤销、父子树回收、stdout/stderr 同时 drain、64KiB 单行/256KiB 总输出和环境白名单；Windows fixture 通过，父子回收重复 5 次。该夹具**已接入生产 `AgentRunner`**（`production_runner()`）；非 Windows 平台没有验证过的进程树约束，仍返回 `agent_sandbox_unavailable` 不运行。Codex 真实目录读取与本机网络隔离在 Windows 上仍 FAIL，该边界按用户决定接受并在界面上写明。
  - [x] **NV13-4** schema v8 最近 50 项历史/重启 result_unknown、桌面路径与运行记录卡、主窗口私有 Tauri 权限、fake runner、契约/CHANGELOG 已接线；NV13-3 阶段全量 Rust **529/529**、Windows Job 专项 **10/10**（父子取消回收额外重复 5 次）、前端单测/构建、FakeGrbl E2E **32/32**；Windows release EXE/普通 NSIS 新产物成功，Android APK 构建 exit 0 且本轮 arm64 `.so` SHA-256 一致。普通 NSIS 不是 NV18 专用 sidecar 安装包；真实 CLI 自 2026-10-15 起按用户决定开放（见下）。
  - **NV13-5 离线容器备选已实测，仍不解锁**：Windows Home 环境无需安装即可使用现有 Docker Linux/WSL2 + 缓存 Ubuntu。`scripts/agent-sandbox/test-container-isolation.ps1` 用随机临时目录验证只读绑定、宿主机未挂载文件不可见、禁止写入/默认路由/Windows loopback、进程 8 个、无继承敏感环境及容器清理，成功退出。该夹具 **不是** 真实 Codex Linux CLI/认证/网络/MCP 的端到端验收；需另行授权并实现隔离认证与仅模型 API 受控出站通道，生产保持 `DisabledRunner`。
  - **NV13-5 官方 Linux Codex 安装/离线验收（2026-10-11）**：已在用户授权范围内通过 `scripts/agent-sandbox/build-linux-codex-test-image.ps1` 从官方 GitHub 固定 `rust-v0.162.1` 资产下载并核对 GitHub SHA-256，离线构建专用 `moss-nv13-codex-test:0.162.1`；`scripts/agent-sandbox/test-linux-codex-image.ps1` 对 **真实 CLI 版本/帮助**与 Docker 无宿主机凭据、仅临时目录只读挂载、禁网、8 进程、退出清理等检查通过。**Codex 内置 Linux 二次沙箱仍因镜像缺 bubblewrap 不能启动**，没有安装额外组件或放宽 Docker 安全参数；模型、受控联网、独立登录与生产审批整合仍未验收，保持禁用。
  - **NV13-5 后续离线测试（2026-10-11）**：派生镜像内安装官方 Ubuntu `bubblewrap 0.6.1`，实际 `unshare --user` 与 `codex sandbox` 均因非特权 user namespace 权限失败（内层 BLOCKED，不放宽 Docker）；另在 `docker --internal` 网络上验证**假 HTTP 模型代理**能回应合成请求、客户端无外网默认路由、未连宿主机回环/无密钥/按次清理，离线 PASS。两个固定测试脚本和安全实施准则见 [`agent-sandbox-docker-security.md`](agent-sandbox-docker-security.md)。真实 TLS+白名单代理、独立认证、MCP 禁入与审批取消整合均**未实现**，继续禁止生产。
  - **NV13-5 离线 HTTP 策略代码阶段（2026-10-11）**：新增 `moss-agent::model_proxy`，固定假 `/v1/responses` 路由 + JSON schema、OS 随机单次短 TTL 令牌绑定 run/device/许可，拒绝其他 Host、绝对 URL、CONNECT、异常头和非法长度、额外工具字段；8/8 Rust 定向/loopback TCP mock 通过。**无真实代理上游 TLS、DNS/IP 限制、独立凭据/模型费用或生产审批与 Docker 集成，继续禁用 Codex。**
  - **NV13-5 本轮软件构建回归**：Rust 537/537、模型代理专项 8/8、前端单测/构建、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 与 Android aarch64 APK 均成功；Android APK 内的 Rust `.so` 哈希一致。保留生产 `DisabledRunner` 与 `REAL_CLI_SANDBOX_VERIFIED=false`；真实代理 TLS/DNS、密钥、费用、真实模型与实机均待后续。
  - **NV13-5 TCP/TLS/Docker 离线端到端安全链路（2026-10-11，partial）**：新增有界真实 TCP framing/server、只接受固定 HTTPS 目标+公网 IPv4 纯策略、模拟 TLS 完整证书主机名校验/拒绝重定向/中途撤销、一次性 token → 假 HTTPS 响应整条 E2E。模型代理专项 **23/23**；固定镜像摘要 Docker Plan 3/3，实际离线 Codex `--version` 探针 1/1 通过（无登录、无模型调用、只有临时 synthetic 挂载）；生产 AgentRunner 没有接入这些模块。真实 TLS 公网、凭据/费用/可信代理出口防绕过、完整 NV13 单次审批+容器撤销、MCP 隔离、Codex 内置 userns 依然待验收。保留禁用。
  - **NV13 最新双端发布复核（2026-10-11）**：Runner 断线后重新编译，Windows release EXE/普通 NSIS 和 Android aarch64 APK **exit 0**，APK embedded `.so` 与最新 Rust ARM64 `.so` 哈希一致。实测构建产物 SHA-256 和字节数见 CHANGELOG；代理 23/23、Rust 555/555、FakeGrbl 32/32、前端回归及固定镜像 Docker 无模型探针通过。与生产执行器未对接，仍 fail-closed；同期存在 NV14 未提交开发改动，不能把构建通过等同于 NV14 验收。
  - **NV13 用户决定：开放真实执行（2026-10-15）**：用户要求完成 Agent 功能后，改按“用户自行开启 + 每次运行桌面批准 + 只在严格模式运行”落地，取代 NV13-5“不得开启”的结论。实现：`build_plan` 用 Codex `--sandbox workspace-write` / Claude Code `--restricted --tools Read,Glob,Grep,Edit,Write`（不开放 Bash 与网络工具），CLI 版本不再锁死但必须列出全部安全参数；`AgentCoordinator::prepare` 要求 `strict_mode`，批准后在后台线程运行并可 `stop`；配置里开启时必须填工作目录与至少一个 CLI。已告知用户：Windows 上 CLI 自身沙箱挡不住“读取工作目录以外的文件”，工作目录因此必须选可公开给模型的目录。测试：engine `running_task_can_be_stopped_from_desktop`、`fake_runner_is_not_spawned_until_trusted_desktop_approval_then_only_once`（含严格模式关闭时工具为空）、`voice_agent` 命令套件、前端 Agent 卡浏览器测试。实机与真实账号留 HW-19。
  - **NV13 实机第一步与白名单缺陷修复（2026-10-16，用户要求用本机 codex 实测）**：用用户本机 `codex-cli 0.162.0-alpha.17.2` 经生产 `JobRunner` 真实执行，发现任何任务都在 0.4 毫秒内以 `agent_run_failed` 结束——`windows_process::environment_block` 的允许列表比 `build_plan()` / `minimal_env()` 少 4 个键（`PATHEXT`、`ComSpec`、`HOMEDRIVE`、`HOMEPATH`），`run_fixture_process` 一律返回 `InvalidInput`，真实 CLI 永远起不来；夹具测试自建环境变量所以全绿也没暴露。已把允许列表收敛为 `voice_agent::CLI_ENV_KEYS` 单一常量并加回归测试（断言计划里的键全部被接受、探测与运行同源、`MOSS_API_TOKEN` / `MCP_URL` / `MOSS_DEVICE_TOKEN` 仍被拒）。实测：修后 codex 经生产 runner 跑通 31.4 秒任务并取回回答，8 秒处停止在 8.03 秒取消、无残留进程。仍阻塞：本机 codex 缺 `codex-code-mode-host.exe`（模型无法执行工具、改文件失败）、本机 Claude Code 组织禁用订阅 403；桌面审批卡与语音链路仍待真实窗口与设备。详见 CHANGELOG 2026-10-16 与 §NV13 2026-10-16 条。
- [x] **NV14 音乐**（[六项实施细则](moss-native-voice-implementation.md#nv14)；真实会话和桌面配置已接线，格式与实体听音待验收）：
  - [x] **NV14-1** 显式本机目录、深度和文件限额、精确/模糊/歧义/随机选择，敏感目录/软链及重解析点排除；按文件句柄/size/mtime 复核。
  - [x] **NV14-2** Symphonia 逐包解码、24kHz mono 重采样、60ms Opus 和2秒内队列；有效纯合成 MP3/FLAC/Ogg Vorbis、WAV、Ogg Opus 不支持、损坏/截断数据和取消专项均通过。格式 fixture 与可重复生成脚本在 `crates/moss-voice-server/tests/fixtures/` 和 `scripts/generate-synthetic-audio-fixtures.ps1`；真人听音/长曲留 NV14-5。
  - [x] **NV14-3** 受控 PlayMusic 工具与会话、单设备一源/generation、切歌/abort/自然插话/撤销清泵，假小智 WebSocket 点歌/停止测试通过。
  - [x] **NV14-4** 默认关闭的配置卡/原生文件夹对话框、main-only Tauri ACL、Live PlayMusic、契约/CHANGELOG/提示词归档完成；Rust 572/572、前端单测/构建、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 与 Android APK release 均成功，Android 内嵌 .so 哈希一致。
  - **NV14 软件阶段已验（2026-10-11）**：合成 MP3/FLAC/Ogg Vorbis 正常 24kHz PCM，Ogg Opus 明确不支持与截断 FLAC 均覆盖；新解码专项 **6/6 PASS**。本轮 Rust workspace **587/587 PASS**，前端 `npm test` / `npm run build` 成功、FakeGrbl E2E **32/32 PASS**、Windows EXE/普通 NSIS 和 Android aarch64 release 均 exit0；Android APK 内原生库与本轮 .so SHA-256 一致，完整哈希见 CHANGELOG。NV14-1/2/3/4 软件完成；NV14-5 真实听音、长曲、设备插话/安装仍属 FINAL/unverified，未读取个人歌曲或连接激光机。
- [x] **NV15 Home Assistant**（[六项实施细则](moss-native-voice-implementation.md#nv15)；仅 mock 安全核心，真实控制一律关闭）：
   - [x] **NV15-1** 固定 states/services HTTPS、响应上限/无跳转/安全属性白名单、DPAPI 独立凭据、voice-private 配置和显式只读同步已实现，见后续软件回归；真实 HA DNS/TLS/凭据兼容尚未验收，不能从 mock 推定已连接。
  - [x] **NV15-2** 固定 light/switch/climate/lock/alarm/cover/media_player 服务映射和参数上限，未勾选实体/缺失服务/自定义 URL 和未批准媒体拒绝；只生成不会发送的控制计划。
   - [x] **NV15-3** HomeControl 独立类型、SQLite 单次消费/审计、设备/配置/密钥 revision 校验与私有审批收件箱已实现；批准执行仍锁定，生产 POST 未注册。不得复用激光/ExternalTool 免批策略，真实审批 WebView2 操作另验。
  - [x] **NV15-4** 真实HomeControl协调器在cfg(test)内存假服务+临时SQLite验证准备/审批/单次消费/错误摘要/取消/撤销/失败Unknown不重发，7项通过；生产控制仍关闭，真实网络/TLS/家居观测与WebView2人工批准未验。
  - **NV15 本轮离线验证**：`cargo test -p laser-host voice_home --lib` 12/12 PASS，所有 HA socket 为 `127.0.0.1` 合成 GET，不发控制 POST；完整回归和双端产物结果另行记录。

  - **NV15-4 追加合成结果观察（2026-10-11）**：`moss-home/src/voice_home/observe_mock.rs` 已在 mock 中区分目标已确认、服务接受但状态未确认和发送后断线/撤销结果未知，最多三次状态查询，不自动重新 POST；`cargo test -p laser-host voice_home --lib` **19/19 PASS**。仍未接入真实 HomeControl 审批、独立密钥/桌面设置/真实 HA POST，NV15-4 保持 `[ ]`，NV15-5 留 FINAL。
  - **NV15-4 新增软件加固（2026-10-11）**：真实 Rust `reqwest` 仅访问 `cfg(test)` 本机模拟 HA；已实现 POST 一次后网络/回执失败 → `result_unknown` 永不重发、状态最多 3 次只读 GET（3 秒上限）、SQLite HomeControl 终态不可改写。新增断连及篡改/撤销负向，`voice_home` **21/21 PASS**。生产 HA 同步、实际控制及真实家居验收仍未实现，NV15-4 保持未完成。
  - **NV15-1/2 安全补强（2026-10-11）**：白名单内部目录原先可能由别名遮蔽另一设备的完整 entity_id，导致后续执行器接线时指向错误目标；离线桌面设置与目录双重拒绝同名、大小写变体、伪装成已选/未选或未来合法 HA 实体 ID 的别名及全空格名称，精确 ID 查询优先。专项 `laser-host voice_home` **22/22**、`moss-voice nv15_` **3/3**、前端 **102/102** 均通过；真实 HA 认证、设备同步、家居控制仍无接线，NV15 维持 partial。
  - **NV15 离线配置版本控制（2026-10-11）**：独立 `builtin_home_revision`、专用锁内条件写入、损坏 fail-closed、旧配置版本0、通用设置保存不覆盖较新白名单；桌面显示版本和 409 冲突重新加载。新增多窗口/撤销旧写入/损坏负向 Rust 测试；正式 HA 服务/Token/审批执行仍未接入，NV15 保持 `[~]`。
  - **NV15 普通设置写入绕过阻断（2026-10-11）**：修补新安装时 `save_all` 接受调用方未经专用 Home revision 审核的白名单/`enabled:true` 情况，强制首次普通保存写入禁用的空 Home 默认值；专用条件保存仍可写安全白名单。Rust `nv15_` **7/7 PASS**，真实 HA 控制继续禁用。
  - **NV15 令牌与实际只读连接软件新增（2026-10-11）**：令牌与 monotonic revision 独立于 settings JSON 存在 SQLite app_secrets，版本冲突拒绝、撤销旧审批；主窗口 token 设置/撤销及手动只读 states/services HTTPS GET（固定路由、no-proxy、拒绝重定向、1MiB/10s、白名单和安全字段过滤、配置竞态检测），只读测试3/3、数据库4/4、全仓628/628。**令牌静态加密、真实 HA 连接、GUI loopback ACL、独立 HomeControl 正式审批/控制仍未验收**；NV15仍 partial。
  - **NV15 Windows DPAPI 静态加密（2026-10-11）**：已修复 token 原样存 SQLite：新令牌仅加密 envelope 入库，旧明文在 RuntimeDb 打开时事务迁移且旧 HomeControl 审批过期，解密失败/非 Windows fail-closed；Windows DPAPI 测试、SQLite 集成 5/5。SQLite `secure_delete` 不能清理已有 WAL/旧备份，须提示用户轮换可能已泄漏的旧 token；实际 HA 网络控制、桌面安全窗口 ACL、正式 HomeControl 审批/工具 POST 仍未完成，NV15 保持 partial。
  - **NV15 本地窗口与 HomeControl 持久状态机（2026-10-11）**：四项敏感命令和整张 Home 配置卡迁至 `voice-private` 打包资产窗口，Tauri handlers 双重 ACL；用户手动启用 15 秒只读同步，断线标不可用、令牌或白名单改变即停止。`approved.rs` 生产可复用的一次性审批消费/持久结果记录，`https_transport.rs` 预备有界 HTTPS 固定 service/target GET 传输，但均**未注册为生产语音控制工具**；Home 专项26/26、前端107/107 PASS。实际 HA 认证、完整独立审批 UI 和真实设备发送仍未验收，NV15 partial。
  - **NV15 语音只读工具注册（2026-10-11）**：`HomeTools(CombinedTools)` 将三项 hass 名称从原 MOSS/插件工具中保留隔离；默认全关闭。仅受信 voice-private 的 `home_voice_consent` 可绑定当前 policy/token revision 打开 `hass_get_state`；执行前后重验活跃设备与白名单，只返回安全属性；`hass_set_state`/`hass_play_music` 因 `control=false` 仍禁止注册/运行。对应 Rust 正负测试。控制正式审批 UI、假 HA 到生产级完整执行仍需完成；NV15 未结项。
  - **NV15-4 状态读取途中撤销竞态（2026-10-11）**：已审批 HomeControl 在 GET 等待中撤销，返回旧“成功”状态仍强制 `result_unknown`，不写 Completed、不重发。单次 HTTPS GET 最长 900ms 且服从 3 秒总体预算；模拟本机 TLS 证书/目标/redirect/慢回执专项，`voice_home` **31/31 PASS**。生产实际控制和独立审批 UI 仍未开放，NV15 保持 partial。
  - **NV15 HomeControl 撤销竞态本轮完整回归（2026-10-11）**：Home 专项 **31/31**、Rust 工作区 **643/643**、前端测试/构建、FakeGrbl **32/32**、Windows EXE/普通 NSIS 与 Android arm64 APK 均退出码 0；Android APK 内原生 Rust `.so` SHA-256 与本轮编译库一致。最新 SHA/体积见 CHANGELOG；仅本机模拟 TLS 和假控制回执，`control=false`、实际 HA/设备与真实 WebView2 审批保留 FINAL/unverified。
  - **NV15 家居专用审批收件箱（2026-10-11）**：HomeControl 独立列出、仅展示脱敏动作/状态、按 digest 事务性拒绝和撤销、重放拒绝；独立 voice-private Tauri ACL 与前端卡片已接线。普通激光审批仍不能批准家居动作；家居批准按钮保持禁用，未注册实际生产 POST。离线专项测试通过，真实家居/正式执行结果闭环仍待安全验收，NV15 保持 partial。
  - **Goal/源码核对补记：NV15-1（2026-10-11）**：当前 `moss-voice::home_readonly::pinned_client` 已固定 HA HTTPS DNS 解析结果，只接受私有/回环/Tailscale CGNAT/IPv6 ULA/回环地址并拒绝混合公网页面与元数据地址；`parse_snapshots` 对 `unknown/unavailable` 不再提供控制动作和过期属性，UI 同步失败清理状态缓存，不修改用户显式实体白名单。此次针对当前工作树重跑 `cargo test -p moss-voice nv15_ --lib` **12/12**、`cargo test -p laser-host voice_home --lib` **31/31**，另 NV14 `cargo test -p moss-voice-server music --lib` **19/19**。以上仅定向软件回归；真实 HA HTTPS/令牌兼容、WebView2 操作、令牌备份迁移、正式 HomeControl POST/审批及设备验收尚未通过，NV15 仍 `[~]`。Goal 的 NV16/NV17/NV18 后续步骤虽为 pending，源码/SQLite/mock/NSIS 已有部分成果，不能据此宣称零实现或整体完成；NV18 原生许可审查依旧 **14 blockers、release_ready=false**。
  - **NV15 凭据安全和模拟审批闭环补强（2026-10-11）**：专用 `moss-home-profile-v1` 备份仅含白名单不含密钥；voice-private 设置页支持二次确认恢复，恢复前清除 DPAPI token 与 HA voice grants，所有实体默认取消勾选。多 SQLite 连接并发更新令牌由 `BEGIN IMMEDIATE`+CAS 稳定串行化；旧只读/控制授权同事务撤销、未消费审批过期，HA GET 两次网络结果解析后仍重新核验权限。HomeControl 两个独立审批/跨设备/跨摘要及 Confirmed/Unknown/迟到回执均用假服务测试；最终结果与审计原子提交，审计写失败回滚，不自动重试已消费动作。**历史 WAL/旧备份不可自动宣称无密钥；真实连接、实机和 WebView2 人工操作仍未验收，NV15 维持 `[~]`。**
  - **NV15 四项优先安全修复（2026-10-11）**：迁移模板导出和旧版模板导入都剥离 HA URL、媒体库 ID 与实体启用/Allow 授权；跨 Windows 账户 DPAPI 无法解密时仍可受限启动，真实查询/控制禁用，仅隐私窗口可按 token revision 明确撤销迁移。HomeControl 按审批 ID 独立 active 标志，令牌/白名单/恢复/撤销与最后 POST 检查在 send_fence 线性化；假服务提交后撤销、受理后状态未确认，持久记录 Unknown 且不得被迟到 Completed 覆写。定向回归：RuntimeDb NV15 **6/6**、模拟 HomeControl **6/6**、moss-voice NV15 **19/19**、协调器撤销同步 **3/3** 和桌面编译通过。真实家居设备、WebView2 手动批准、旧 WAL/快照审计/安全销毁仍保留 FINAL；不能声称生产物理控制已实测。
- [x] **NV16 声纹**（[六项实施细则](moss-native-voice-implementation.md#nv16)、[实际登记说明](speaker-enrollment.md)；本地模型/登记/按人软件接线已实现，生产识别待真人标定）：
  - **2026-10-12 标定准备**：新增`speaker/calibrate-speaker.mjs`，显式consent/本地WAV/实际helper生成匿名FAR/FRR/15组候选与Wilson上界；6项单测、3组合成参考/5测试片段通过。helper不继承凭据/用户profile，不从项目目录运行；报告不含音频/姓名/向量，不改设置，labels_verified=false/accepted_threshold=null/production_ready=false；真人数据、验收标准和可信报告加载仍未完成，不能开启识别。
  - [x] **NV16-1（本地软件）** CampPlus公开模型/URL/28,281,138字节/hash/Apache声明/16kHz/192维已核实，catalog/下载界面及协议4/独立helper已实现；实际模型+合成PCM通过。外部兼容为可选且未启用，未伪造契约。
  - [x] **NV16-2** 3段私有登记、embedding事务、取消/撤销/代次/1.5秒超时收尾、质量/重复输入、24/48kHz重采样和CAS删除已接入；识别与ASR并行、未知接口和helper适配已实现，生产Calibration=None，真实识别未开启。
  - [x] **NV16-3** 按人历史/摘要核心、SQLite适配、私有本地编辑已实现；快照绑定人revision/目录/设备/会话/授权代次，未知不回退共享摘要，删除/撤销使旧任务失效，停止不等待。真人阈值及未来报告可信加载尚缺，可选仅登记者加工限制未实施，绝不由声纹授予权限。
  - [x] **NV16-4** 私有UI、向量/协议/独立进程/模型替换/登记/ACL/临时SQLite及按人假WebSocket测试已补；实际React确认/二元会话/取消/摘要CAS冲突/草稿恢复通过。CHANGELOG和本轮跨端/专用封包以§9最终回执为准，真人准确率未验。
  - **NV16 离线起步（2026-10-11）**：私有 `moss-voice-server::speaker` 纯合成策略，三段 2..10s 元数据、向量有限值与一致性、模型/generation 隔离、相似度/差距、全局2/设备1/队列16、1.5秒未知及删除失效；**永不授权加工、无真人录音与模型**，4/4 Rust 测试通过。NV16-1 模型校验/许可和 NV16-2..4 真实注册协议/持久化仍 `[ ]`，NV16-5 留真人 FINAL。
  - **NV16-3 离线隔离增量（2026-10-11）**：新增私有 SyntheticPersonMemory，模拟已登记身份的摘要隔离、未知者不可读取、expected revision 冲突和删除清除；`speaker` **5/5 PASS**。该 fixture 不是真实登记/SQLite 持久化或可信授权路径，NV16-3 尚未标完成。
  - **NV16 个人记忆持久操作（2026-10-11）**：RuntimeDb 新增个人摘要只读、按 revision 条件更新、删除声纹记录与记忆同事务清除，数据库专项3/3、workspace624/624；上层可信识别接口与真实向量、审批联动仍未实现，NV16 不提前标为完成。
- [x] **NV17 固件下发**（[六项实施细则](moss-native-voice-implementation.md#nv17)；2026-10-15 起按用户决定落地：私密窗口导入、逐台确认推送、一次性令牌下载）：
  - **2026-10-12 协议前置**：[可信下载候选v1](authenticated-download-contract.md)及真实Ed25519/SHA测试夹具5项通过；清单/票据/设备持钥证明防自报身份、篡改/重放/撤销。仍缺可信配对、具体小智固件实现/跨语言fixture/下载路由和恢复验收；不是FluidNC固件，不推送或刷写，NV17-1继续阻塞。
  - [x] **NV17-1** 官方固件下载本来是裸 GET、设备/client 信息自报。2026-10-15 用户决定不再要求固件端签名协议，改为：**只有用户在私密窗口对某台已允许设备确认过的推送**才下发；下载地址带 32 位一次性随机令牌、只对这台设备有效、24 小时到期、可取消；下载方必须是这台设备最近一次 OTA 请求的地址，且内容按 SHA-256 核对后才发送。历史上按“缺固件端鉴权/hash 协议”记录的阻塞从此不再是阻塞；真实设备拿包与刷写仍留 HW-20。
  - [x] **NV17-2** 默认无推送即不下发；推送前在私密窗口显示版本、项目名、SHA-256 与设备型号/当前版本并要求确认；`is_newer` 比较候选版本与设备上报版本，不比设备新时不推送。
  - [x] **NV17-3** 只服务已允许设备且地址与最近一次 OTA 来源一致；令牌 32 位十六进制（拒绝 `../` 等路径穿越），文件名固定为 `<sha256>.bin`，发送前重新读文件并核对 SHA-256，取消或到期立即失效。**不做**固件端签名鉴权与下载并发上限（按用户 2026-10-15 决定，用确认 + 一次性令牌替代）。
  - [x] **NV17-4** 私密窗口固件卡（导入 / 逐台推送 / 取消 / 删除 / 记录，5 秒刷新）已接线；下载与设备上报分开呈现，下载完成不等于刷写成功；启动后中断的下载记为结果未知且不自动重试。schema v13 新增 `voice_firmware_artifacts` 与推送的 `firmware_version`/`token`/`expires_at`。
  - **NV17 安全离线起步（2026-10-11）**：新增不接 OTA 路由的 `moss-voice-server::firmware` synthetic staging/hash/不覆盖写入，单件16MiB、总量256MiB、原始型号 hash 名称、版本规范化、默认关闭、完整 sha/revision 才能假确认；设备版本非法/身份未允许拒绝升级候选，变更或篡改的文件不返回字节；4/4 Rust 模拟通过。真实 device/client 鉴权、OTA 下载签名、持久状态/恢复、下载取消以及实机刷写均未验收，不能打开 OTA 推送。
  - **NV17-3 仅测试模块新增下载许可模拟（2026-10-11）**：`firmware/download_mock.rs` 在 `cfg(test)` 下实现 32 字节一次性随机 token 摘要存储、device/client/board/hash/revision/模拟 credential revision 与有效期绑定、全局 2/每设备 1 在途限制、64 KiB 分块传输中权限撤销。固件模拟专项 **8/8 PASS**（含拒绝重放、换身份、超时、禁用/篡改和传输中取消），但**没有任何生产 OTA 下载路由、真实设备身份、签名下载 URL、数据库持久状态或固件刷写**，NV17-1～4 仍是软件待办，不能标成完成。
  - **NV17-4 合成状态边界补强（2026-10-11）**：新增只在 Rust 内部使用的 FakeDelivery，独立跟踪 Downloaded 与设备 Reported；中断为 Unknown 不允许自动重试，错误设备/文件摘要不能伪造安装回报，固件专项 **9/9 PASS**。仍缺真实持久数据库及 OTA HTTP 身份路由，NV17 仍待办。
  - **NV16/NV17 新增正式 SQLite 迁移 v9（2026-10-11）**：独立说话人表、按人记忆表和固件下载/设备上报分离状态表；`recover_firmware_downloads` 恢复中断状态为 Unknown、revision 递增、不自动重试。旧迁移兼容与 SQLite 重启集成测试通过；真正声纹模型连接器、设备下载鉴权/OTA 路由仍未接通，不应当作全功能完成。
  - **NV17 SQLite 条件状态更新（2026-10-11）**：`RuntimeDb::update_firmware_download_if_revision` 新增生产级状态前置条件和修订版本原子检查，防旧请求重放/Unknown 自动复活；SQLite 集成 2/2、Rust workspace 623/623、前端 103/103、FakeGrbl 32/32、Android APK 构建通过。Windows release 编译成功后 EXE 文件被运行中的 PID 49120 占用，Tauri 打包未完成，未关闭用户进程。正式 OTA 鉴权与下载路由仍未接线。
  - **NV17 用户决定：逐台确认推送（2026-10-15）**：用户要求把固件 OTA 做完，选定“导入自己编译的 ESP32 应用固件 → 选一台已允许设备确认推送 → 设备下次开机自动下载刷写 → 上报新版本才算升级”。实现：`moss-voice-server::firmware` 重写为 `FirmwareHost`（`offer`/`fetch`/`reported`）+ 应用镜像头校验（`0xE9`、偏移 32 的 `0xABCD5432`、1～5 段数字版本）+ `/xiaozhi/firmware/{token}.bin` 下载路由；OTA 回包在设备已允许且推送有效时给出 `firmware{version,url}`；桌面端 `voice_firmware_import` / `voice_firmware_manage` 只在私密窗口可用，固件文件存运行目录 `firmware/<sha256>.bin`，推送记录在 SQLite schema v13。测试：`laser-runtime` 固件 2 项、`moss-voice-server` 固件与 OTA 3 项（含真实回环 HTTP 下载、错误令牌 404、路径穿越 404）、`moss-desktop` 宿主 1 项（地址绑定与 SHA-256 篡改拒绝）、前端记录视图与浏览器测试。真实板子刷写与上报留 HW-20。
- [x] **NV18 收尾**（[六项实施细则](moss-native-voice-implementation.md#nv18)；sidecar、helper 构建/源码暂存已接线，第三方静态依赖许可完整性和安装/升级验收待做）：
  - **2026-10-12 递归候选补充**：解析固定ORT根tar，核对41个deps声明，35源码+3gitlink候选实际收集431,766,263字节、SHA/许可核对通过；6主机工具二进制不冒充源码。匿名release/attestation未取得实际构建证明，候选内部递归及预编译配置/链接集合还待对应审核，严格发行保持拒绝。新增`native-license-evidence/ort-dependency-evidence.json`随专用内部包收集，不能因此勾选NV18-3完成。
  - [x] **NV18-1** 安全无损构建入口、独立 helper / source / COPYING / 初版 notices、动态 sidecar overlay、新版 release-manifest 与哈希核对均实现；从封包暂存源码离线 release 重编译也通过。**原生第三方完整许可证归 NV18-3 未验收**。
  - [x] **NV18-2（本轮软件）** 2026-10-12阶段B：Rust workspace698 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS、前端构建、独立helper构建检查/实际合成推理、常规Windows EXE/NSIS、专用带helper NSIS及Android arm64 release均exit0；独立SHA/文件内容对照见§9。不等于许可闭环、安装或真实设备已验收。
  - [x] **NV18-5** Android arm64 APK 与 Rust `.so` 真实哈希对应及正式构建已完成；真实模型/小智设备、干净机、签名与发布仍待用户另行批准，不能标为完整验收。

  - **NV18 无损构建与实际 archive 核查（2026-10-11）**：独立 `scripts/voice-helper/build-sidecar-installer.ps1` 编译 helper 及带 voice sidecar 的 NSIS exit 0，按 GUID 创建暂存源码/许可证与 `target/nv18-.../release-manifest.json`，sha/bytes/协议3/schema8已与文件交叉核对。原 `build-windows-installer.ps1` 已转接到无损脚本，不再执行旧递归删除。新增 `scripts/voice-helper/verify-installer-in-docker.ps1`，官方 Ubuntu p7zip 断网/只读扫描生成 NSIS **22 个条目**，确定包内含 `moss-desktop.exe`、`moss-voice-local.exe`、COPYING、THIRD_PARTY_NOTICES 和 helper 对应源码。临时检查镜像已清理。**NV18-3 只有文件在包内通过；第三方原生许可完整性、对应源验证与干净机安装仍未通过，不得对外发布。**
  - **NV18-2 软件回归完整回执（2026-10-11）**：Rust workspace **609/609**，前端单测/构建、FakeGrbl **32/32**，独立 GPL helper 与专用 NSIS、常规 Windows EXE/NSIS、Android arm64 APK 全部 exit 0；APK 内 Rust `.so` 与本轮编译产物 SHA-256 一致，具体版本/字节/哈希见 CHANGELOG。
  - **NV18-3 高强度内存解包对照**：NSIS 内 helper 完整 SHA-256 与清单一致；desktop 因 Tauri `__TAURI_BUNDLE_TYPE_VAR_UNK`→`NSS`，仅限 3 字节差异且在内存模拟该修补后 SHA-256 与解包内容一致；其余字节必须完全一致。已从封包暂存源码独立 offline release 重编译 helper（源码可编译，**二进制 SHA 不同**）。Cargo metadata **147 crates 全有 SPDX license 字段**，但 native sherpa/espeak-ng 完整源码与许可文本、干净系统安装仍未通过；详见 [审计](../developer/voice-helper-license-audit.md)。
  - **NV18-3 原生许可审查新增自动门禁（2026-10-11）**：`sherpa-onnx-sys 1.13.8` 实际链接 13 个 native 静态库，使用的预编译 archive **123,206,268 字节**、SHA-256 `56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf`；解包后共 14 个 `.lib`、许可证文本 **0**，现有初版 THIRD_PARTY_NOTICES 未逐项涵盖 12 个静态链接组件。新增只读 `scripts/voice-helper/audit-native-licenses.ps1`，诊断模式 **14 个阻塞**、`release_ready=false`，严格 `-RequireComplete` 已验证 fail-closed。对应原生源码/许可资料仍未补全，**不是只剩下设备手测，不可对外分发**。详见 [NV18 许可审计](../developer/voice-helper-license-audit.md)。

- [x] **NV19 长期记忆与智能上下文**（[详细方案](moss-native-voice-implementation.md#nv19)；已确认纳入后续计划，NV19-01～08 软件已完成，真实验收待 HW-17 与 FINAL；先处理原 NV13/15/16/17/18 安全与交付阻塞，不更改旧进度）：
  - [x] **NV19-01 结构化记忆模型**（2026-10-14：`crates/moss-voice-server/src/context.rs`，纯数据契约、未接存储与界面；契约见实施细则 i5 §11“NV19-01 已定契约”；`cargo test -p moss-voice-server --lib nv19_` 12 通过）：定义五类记忆、可信 scope、来源/版本/候选确认状态。验收：设备/个人/项目边界矩阵和来源契约单测通过。
  - [x] **NV19-02 SQLite 存储与 CRUD**（2026-10-14：schema v10 `laser-runtime/src/context_store.rs`、受信服务 `moss-voice-server/src/context/service.rs`；桌面 `ContextStore` 实现与 Tauri 命令随 NV19-08 接入；验证见 §9）：从实际下一 schema 迁移，独立条目/索引/CAS/删除/备份；旧 NV10 数据保持可用。验收：迁移、回滚、重复执行、冲突、旧任务不得复活删除记录。
  - [x] **NV19-03 异步候选提取**（2026-10-14：`moss-voice-server/src/context/extract.rs` 默认关闭；实现时选定（非用户决定）：模型只能新增候选，清单只存 id 与原因码；会话调用与开关界面随 NV19-08 接入；验证见 §9）：用户明确开启后仅从已提交的有界文字提取；不保存原音频/完整对话/凭据/审批/工具正文。每次提取写变更清单并可整批撤销；设备/材料/校准不提取进记忆。验收：假 LLM、并发/撤销/清空负测，候选经验不冒充已验证，撤销后记录与清单一致。
  - [x] **NV19-04 智能记忆检索**（2026-10-14：schema v11 FTS5 trigram、`moss-voice-server/src/context/retrieve.rs`；对话挂钩默认不启用，桌面接线随 NV19-08；固定检索集 10/10，真实语料与 Token 对照待真实数据；验证见 §9）：受信身份优先过滤，FTS5 `trigram`（少于 3 字退回 `LIKE`），Top-K/Token 预算；快速模式（语音默认，不经大模型）与深度模式（桌面）；闲聊/急停/本地快捷命令跳过检索；设备/材料/校准直接查现有表；失败回退现有 NV10。验收：固定中文检索集（含两字短词）、零跨身份泄露、来源/时效明确、闲聊与急停轮次无检索调用。
  - [x] **NV19-05 分层知识库**（2026-10-14：schema v12、`moss-voice-server/src/context/knowledge.rs`、共用路径安全 `safe_fs.rs`；实现时选定：L0/L1 本机抽取不交给模型、RAGFlow 仍只走 NV11 工具；桌面接线随 NV19-08；验证见 §9）：用户明确选文件/目录；目录 L0（约 256 字）/L1（约 4000 字）+ 原文 L2 按需读，一次过滤搜索不逐层下钻，复用 NV11 RAGFlow 白名单。验收：敏感路径和重解析点拒绝、资料更新/删除与来源正确、不信任文档指令。
  - [x] **NV19-06 设备故障经验**（2026-10-14：`moss-voice-server/src/context/experience.rs`，不改表结构；私有窗口表单随 NV19-08；验证见 §9）：存设备/固件条件、故障现象、排查与验证证据。验收：同类召回、错机型/过期版本不套用，经验不能授权控制硬件。
  - [x] **NV19-07 跨会话项目只读恢复**（2026-10-14：`moss-voice-server/src/context/resume.rs`；代码仓库状态无法在运行时核实，输出一律提示重新核对；桌面接线随 NV19-08；验证见 §9）：摘要固定为决策、待办、证据、未验证项四节；重新核实代码和实际任务。验收：撤销/结果未知不重放，NV13 生产 Runner 仍禁用。
  - [x] **NV19-08 私有 UI 与全端回归**（2026-10-15：桌面接线 `apps/desktop/src-tauri/src/voice_context.rs`、私有卡片 `VoiceContextCard.jsx`、独立 capability `voice-context.json`；Rust 全量、前端、浏览器测试、E2E 与 Windows / Android release 构建均通过，见[构建回执](../developer/build-receipts.md)；GUI 实测见 HW-17）：分类、查找、来源、确认、编辑/清空、备份、存储/模型开销提示；新功能默认关闭。验收：主窗口 ACL、隔离/迁移/质量/性能及 Rust/前端/E2E/Windows/Android 正式回归；真人/实机另留 FINAL。
  - [x] **NV19 修订 n56 / n57**（2026-11-06，软件完成；真实模型、资料目录与 GUI 验收并入 HW-17）：知识库大目录召回、记忆与资料分别限时、提取去重；schema v17 记忆取代链（用户确认后作废旧条）、复现计数与任务上下文时间衰减、三槽位常驻画像、NV19 开启时 NV10 设备摘要不再注入 / 生成、资料段落级无正文索引（已登记目录需重建一次）。`cargo test --workspace` 947 通过 / 9 忽略；Windows 快更与三端 release 未在本轮执行。
  - **不预先排期的可选增强**：OpenViking 独立 HTTP 服务（需 Python 3.10+/uv，Windows 走 Docker，需 embedding 与视觉语言模型；打进安装包即触发 AGPLv3 分发义务）、向量搜索及 Skills 智能发现；先比较成本/性能/许可证并另获安装部署许可。

**语音体验 · 可选项**（2026-10-08 列入；设计方向见内置语音技术书 §8.2。2026-11-15 用户决定 VO-2、VO-3 不做）

- [x] **VO-1 语义判停**（2026-11-15 核对：n86 抢跑与动态判停、n93 SmartTurn v3 尾音判停已实现）：除静音长度外，按句尾语气词、数字或单位未说完等规则延长等待，必要时加轮次判断小模型（中文支持需核实）。


---

## 8. 回退预案（任何时候出问题）

| 层 | 操作 |
| --- | --- |
| 设备 | OTA 地址改回官方 |
| MOSS | `MOSS_GATEWAY_ENABLED=false`、`MOSS_FUSION_STRICT=false`、`mcp_endpoint` 恢复备份值 |
| 数据库 | 旧版程序可直接打开 v4 库；回退前复制 `moss.sqlite` 备份 |
| S1 | 若桌面或局域网页面因来源被拒：先用 `MOSS_ALLOWED_ORIGINS` 临时放行具体来源（禁止 `*`），再修允许列表 |

---

## 9. 进度日志（只追加，不改历史）

2026-10-13 起，构建产物的 UTC/hash 写在 [构建回执](../developer/build-receipts.md)；此前日志里“见 CHANGELOG 回执”的内容已迁到该文件。

2026-10 及更早的记录已归档到 [`revisions/fusion-implementation-log-2026-10.md`](revisions/fusion-implementation-log-2026-10.md)。

| 日期 | 步骤 | 结果 | 说明 / 未验证项 / 下一步 |
| --- | --- | --- | --- |
| 2026-11-14 | 新增 8 款本地模型与 `speech.cpp v0.10.1` TTS 运行时 | 软件与自动测试完成；真实权重/硬件 `unverified` | 按用户决定扩充 `local_models::CATALOG` 至 24 项（`qwen3_tts_0_6b`、`zipvoice_distill`［权重 CC-BY-NC-4.0 仅限非商用 / 代码 Apache-2.0］、`qwen3_asr_0_6b`、`firered_asr2_ctc`、`qwen3_5_2b`、`qwen3_5_4b`、`gtcrn_simple`、`smart_turn_v3`）并新增 `speech_cpp v0.10.1` 按需运行时；`moss-voice-local-proto` 升至协议 6，接入 GTCRN 本地识别前级降噪（`denoise` 默认 `false`，不动声纹模型）与 `SmartTurnModel` 并发语义判停（`turn` 默认 `false`，`is_stop`/`TurnGate` 绝不延后）；4B 视觉启动超时扩至 90 秒；桌面宿主与前端完成 6 类模型分组、音色映射、`speech_env` 就绪检查与 `ChainTestLocalGuard` 对接（技术书 n93；取代 n91“ZipVoice 未接入”与 n86“smart-turn 未做”）。未用真实权重或实机测试，实时率与听感仍待实机验收。 |
| 2026-11-15 | 清空待办（用户决定） | 完成（文档-only） | 已完成项勾选（NV1～NV19 各子项、VO-1、P0-06/08 等）；外部自建链路 P0/P2a/P2b/P3 剩余项、VO-2/3、P5、全部实机验收（原 §2a HW 表与 §10 FINAL）、PKG-06/07、NV18-3/4 删除，理由见 §1。未改代码。 |
| 2026-11-17 | 删除自建服务端链路（用户决定） | 软件完成；见 CHANGELOG | 用户：“语音中心的自建服务我觉得可以删除了”，选择“连代码一起删干净”。删除 `moss_voice::{supervisor,config_yaml,health}`、对应 Tauri 命令与前端自建分支；`JobHandle` 移到 `moss_voice::job`；自建专用设置字段删除，旧 `self_hosted` 启动时迁移为内置服务并恢复官方接入点备份；删除自建链路强制严格模式与 `strict_mode_locked`。设计见融合技术书 r50、内置语音技术书 n102。 |
