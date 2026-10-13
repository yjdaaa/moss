# MOSS 融合实施 TODO

实施进度的**唯一记录处**。设计看 [融合技术书](moss-desktop-fusion-technical-spec.md)、[内置语音技术书](moss-native-voice-spec.md)；NV10～NV19 步骤看 [实施细则](moss-native-voice-implementation.md)，P2b/P3/P5 看 [外部链路实施细则](moss-self-hosted-extension-implementation.md)。规则、验证与结果状态见 [`AGENTS.md`](../../AGENTS.md)。

## 0. 开工前

1. 看 §1 状态表，找到要做的工作包，再到对应章节找第一个未勾选步骤及其前置条件。
2. `git status --short` 区分本任务改动和用户原有改动，不覆盖、不回滚。
3. 中途续做时先按该步骤的“检查”逐项核对，已满足的跳过。
4. 设备运动/出光/急停实测、固件刷写或 OTA 地址修改、部署服务、写真实密钥、提交推送：先获得用户同意，本文件写了也不能直接做。
5. 完成一步就勾选，并在 §9 追加一行。

标记：`[ ]` 未开始　`[~]` 进行中　`[x]` 完成　`[!]` 阻塞（§9 写原因）　`[-]` 不做（写原因）。
子步骤写“软件完成”时，`[x]` 只覆盖实现和自动检查；实机、真人、真实账号、安装验收统一放 §10，未测前保持 `unverified`。

## 1. 当前状态（2026-10-12）

阶段顺序：S1、S2 → P0 → P2a → P1 → P3 → P4 → P2b（可选）→ P5；UI-2、PKG、EXP、LINK 独立。正式外部链路为 MCP 接入点。

| 工作包 | 软件 | 真实验收 | 阻塞 / 下一步 |
| --- | --- | --- | --- |
| S1 CORS 与来源校验 | 完成 | Android 实机来源待测（HW-03） | — |
| S2 顶栏真实状态 | 完成 | 待测（HW-05） | — |
| UI-2 前端改版 | 完成 | 待测（HW-01/02/06） | — |
| PKG 安装包 | 5/7 | 未做 | PKG-06 签名、PKG-07 干净 VM 验收 |
| EXP 导出目录 | 完成 | Android 待测（HW-04） | — |
| LINK 手机连接电脑 | 完成 | — | — |
| P0 前置验证 | 2/9 | — | 云端模型能力、延迟、固件、部署位置未验证 |
| P2a 接入点零代码验证 | 1/6 | — | 需授权部署服务端与接入点 |
| P1 调用方上下文与审批 | 12/13（P1-05 不做） | 待测（HW-07） | P1-13 首次实机出光需另行授权 |
| P2b 网关（可选） | 未开始 | — | 仅在需要审批播报或接入点不兼容时启用 |
| P3 自有云端模型 | 未开始 | — | 依赖 P0 |
| P4 语音中心 | 完成 | — | — |
| NV0～NV9 内置语音基础 | 代码完成 | 待真实设备（HW-08/09/10） | — |
| NV10 记忆 | 软件完成 | GUI、真实模型待 FINAL | — |
| NV11 信息插件 | 11-1/11-3 完成 | 真实账号待 FINAL | NV11-4 跨存储故障原子性未验 |
| NV12 视觉 | 软件完成 | 真实设备/模型待 FINAL | — |
| NV13 Claude Code / Codex | 框架完成，生产锁定 | — | 真实沙箱内层 user namespace 被拒（NV13-5） |
| NV14 音乐 | 软件完成 | 听音待 FINAL | — |
| NV15 Home Assistant | mock 完成，生产控制硬关闭 | 真实 HA 按用户决定暂缓 | 真实 TLS/服务兼容与逐实体授权 |
| NV16 声纹 | 登记与按人隔离已接入 | 未标定 | 需真人 FAR/FRR 标定，生产 Calibration=None |
| NV17 固件下发 | 只读记录完成，OTA 下载 URL 为空 | — | `[!]` 缺固件端鉴权/hash 协议 |
| NV18 收尾 | 构建与源码收集完成 | 未安装 | 原生依赖对应审核、ORT 传递许可闭环 |
| NV19 长期记忆 | 未开始 | — | 排在 NV13～18 阻塞之后 |
| VO-1～3、P5 | 未决定 | — | — |
| FINAL-01～05 | — | 未开始 | 软件全部完成后集中执行 |

最近一次全量结果与产物见 §9 末行及 [构建回执](../developer/build-receipts.md)。

---

## 2. 通用检查

按 [`AGENTS.md`](../../AGENTS.md)“验证”执行：每个代码步骤跑受影响测试，阶段收尾跑三端 release 构建；改契约、表、工具、提示词时按融合技术书 §13 同步文档。

---

## 2a. 待实机测试（需要设备或真机，留到后面统一测）

以下项目代码已完成、自动测试已过，但必须在真实设备上验证。**未测之前一律视为 `unverified`**，不得写成通过。测试会让激光机运动或出光的项目，必须确认材料放好、有人在旁、随时能按硬件急停，并先在对话中得到用户同意。

| ID | 来源 | 需要 | 操作 | 期望 | 状态 |
| --- | --- | --- | --- | --- | --- |
| HW-01 | UI-05 重复发送防呆 | 激光机通电、材料放好 | 创作画板生成预览并发送一次；**不重新生成**再点“确认发送” | 确认弹窗出现“我已移动 / 更换材料”勾选框，不勾选不能发送 | 待测 |
| HW-02 | UI-03 顶栏常驻急停 | 激光机通电、正在下发任务 | 雕刻工作台与创作画板各发送一次，发送中点顶栏红色“急停 0x18” | 任务停止，效果与页面内急停 / “取消”相同；无任务时弹窗提示用硬件急停 | 待测 |
| HW-03 | S1-09 Android 来源 | Android 手机 | 安装 ACL 修复后重新打包的 APK，打开各页面 | 能进主页、各页面数据正常加载（后端收到的来源为 `http://tauri.localhost`） | 待测 |
| HW-04 | 导出（Android） | Android 手机 | 创作画板导出 PNG / 场景、材料库导出 | 提示保存路径且文件存在；若落在应用私有目录，记录实际路径 | 待测 |
| HW-05 | S2 小智真实状态 | 可用的小智接入点 | 在偏好设置填写接入点并重启；断开网络再恢复 | 顶栏依次显示“未连接 → 已连接”，控制台“小智网关”页的客户端、消息数为真实值 | 待测 |
| HW-06 | UI-03 背景性能 | 目标 Windows 电脑 | 打开桌面端 5 分钟，看任务管理器 GPU / CPU；同时下发一次任务 | 界面流畅，背景不影响下发节奏 | 待测 |
| HW-08 | NV1 / NV2 内置语音回声 | 小智设备、同一局域网；改设备 OTA 地址需用户同意 | 语音中心启动内置服务，设备 OTA 改为卡片上的地址并重启；在电脑上允许后再重启；自动模式（唤醒词或单击 BOOT）说一句话，播放中再唤醒一次；按住说话模式实战派默认未开启，回声模式下无法开启，可跳过 | 未允许时设备提示“等待允许”；允许后能连上；自动模式停顿约 0.2 秒播回；回声无断音跳字（事件“补静音 0 帧”）；播放中唤醒立即停 | 待测 |
| HW-07 | P1-09 审批卡真实流程 | 激光机通电（或断电只看界面）、小智接入点或手机“连接电脑”模式 | 临时开启 `MOSS_FUSION_STRICT=true`；由小智或手机发起开始加工 | 桌面任意页面弹出审批卡，未勾选现场确认不可批准；批准后进入四步发送覆盖层且急停可用；拒绝 / 稍后处理 / 过期行为符合 §9.5.2；测试后关闭严格模式 | 待测 |
| HW-09 | NV8a 各环节耗时与体验优化 | 小智设备、内置服务对话模式；云端或本地模型；激光机通电（只测停止，不出光） | 用真人语音分别测几种模型组合（本地快组合、NV6 默认组合、云端 / 豆包），每种说 5 句以上；任务运行中（可用空跑）喊“停”；问“现在什么状态”；说含材料名与参数的句子；让它说一段长回答 | 语音中心“各环节耗时”有每轮数据，记录各组合的判停、识别、大模型首字、首段合成、首帧耗时；本地识别链路可用且设备局域网仍连通时，断开外网喊“停”仍发出停止请求且不经过大模型；云端识别组合不宣称离线可用；状态问题不经过大模型；热词开启后材料名识别正确率提高；长回答边合成边播、无断音（补静音 0 帧） | 待测 |
| HW-10 | NV8b 自然打断 | 小智设备（ES8311 + ES7210，MIC3 回采）；固件开启设备端回声消除（重新编译 / 刷写需用户同意）；内置服务对话模式 | 语音中心打开“说话时可直接插话”；让 MOSS 说一段长回答，中途直接说“等一下，换成亚克力”；安静环境下让它连续说 10 段长回答、不说话；打开风扇 / 气泵 / 排风再重复；MOSS 说话时直接说“停” | 插话后约 0.5 秒内停播，整句（含开头“等一下”）识别完整；不说话时 0 次误打断，记录噪声环境下的误打断次数并据此调插话时长；直接说“停”触发急停；事件中有每次插话的时长 | 待测 |

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
- [~] **S1-09 实测 Tauri 来源（Android 实机 Origin 未验证）**
  - 做：构建 Windows App 与 Android APK 后，在应用内确认后端收到的 `Origin` 实际值（可临时日志，验证后删除）。
  - 检查：桌面端与 Android 端页面功能正常；实际来源与 S1-03 列表一致（不一致则修正列表并补测试）。Android 实机安装需用户授权，未安装则标 `unverified`。
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

## 3b. UI-2 前端整体改版（技术书 §9.5.5；已按确认版实施，实机验收见§2a）

前置条件：用户确认最终原型版本。原型：`frontend/ui-preview-forge-v2.html`（曜石设计 + 流浪地球太空舱背景）。只改样式与布局，不改任何业务行为。

- [x] **UI-01 设计定稿**（2026-10-06 用户确认：按技术书 §9.5.5 与原型 `ui-preview-forge-v2.html` 实施，背景为等离子流体）：确认记录和实现已同步§9及技术书；后续新设计需另行明确范围，不重复把已确认事项记为待定。
- [x] **UI-02 令牌与基础组件**（剩余：组件内联样式逐步迁移为类名，作为后续重构，不阻塞）：`styles.css` 令牌（色彩、圆角、阴影、字体）、按钮、输入、分段、卡片聚光、表格（加粗表头 / 斑马纹 / 悬停）、胶囊、弹窗；把 `Navbar.jsx` 等组件的内联样式迁移为类名。检查：旧样式无残留冲突；hover / disabled / focus 清晰。
- [x] **UI-03 外壳与背景（软件）**：`SideNav.jsx`左侧导航、`Navbar.jsx`标题/硬件/小智四态/同一`0x18`常驻急停、`CyberBackground.jsx`的WebGL等离子流体及Geist/OFL本地字体已实现；字体固定来源与实际加载见§9。背景采用静态层离屏预渲染、WebGL半分辨率、40fps限制、低帧率降级与WebGL不可用回退；reduced-motion和窄屏规则已有软件检查。前端/FakeGrbl/Windows/Android已有阶段A回执，后续代码须以本次新构建为准。目标电脑实际显卡帧率/CPU及加工中表现仍留HW-06，不能用软件构建代替。
- [x] **UI-04 雕刻工作台改版**（三栏已完成：①通信链路 ②加工内容 ③材质与参数 / ④加工监视舱 / 加工反馈 + 控制台；待按 H1–H26 逐项真机核对）：三栏布局，按功能对照清单 H1–H26 逐项核对。检查：A-80 对应项。
- [x] **UI-05 创作画板改版**（左侧大画布 + 右侧 材料与尺寸 / 设备 / 预览与发送 三卡已完成；待按 D1–D21 真机核对）：D1–D21 逐项核对；重复发送防呆不变。检查：A-80 对应项。
- [x] **UI-06 材料实验室改版**（材料库改为表格 + 厚度胶囊，三栏版面已完成；待按 L1–L15 真机核对）：材料库表格化，L1–L15 逐项核对；`BroadcastChannel` 同步不变。检查：A-80 对应项。
- [x] **UI-07 弹窗改版**（弹窗统一曜石样式；待逐个核对 S1–S9、A1–A3、C1、O1）：偏好设置、配对、关闭窗口、材料编辑、确认发送、发送覆盖层（`STAGE_MAP` 不变）。检查：S1–S9、A1–A3、C1、O1。
- [x] **UI-08 验收与构建**（视觉基线已生成于 `frontend/tests/visual-baseline/`；用户本机测试与构建通过；需实机的 HW-01、HW-02 见 §2a）：更新 `frontend/tests/visual-baseline/`；E2E 不改断言通过；三端构建；CHANGELOG。检查：A-79、A-80、§2 全部检查。

---

## 3c. PKG 安装包与卸载（技术书 §9.6；规范 docs/developer/packaging-install.md）

前置条件：用户确认开始做分发；建议在 UI-2 前端改版之后、对外分发之前。需要一台干净的 Windows 虚拟机做验收。

- [x] **PKG-01 打包配置（软件）**：`tauri.conf.json`配置NSIS、both安装范围、中文语言、MOSS开始菜单和embedBootstrapper，普通`cargo tauri build`已有实际EXE/NSIS回执，见§9。本次新代码仍须最新重建。正式发布者/签名信息待用户提供，安装实测归PKG-07。
- [x] **PKG-02 安装版配置读取（软件）**（2026-10-06 用户确认安装版运行目录`.env`生效）：`config.rs::load_env_files`先启动目录再运行目录且不覆盖已有变量，`RuntimeRoot::default_base()`与migrate默认目标已实现。相关4项单测包含在workspace回归，release moss-host已重新构建，见§9；本次未安装程序，干净环境快捷方式启动仍归PKG-07/PK-04。
- [x] **PKG-03 安装/卸载钩子**（已写 `installer/hooks.nsh`；待 PK-06/07/08 实测）：`installer/hooks.nsh`——安装、升级、卸载前检测 `%APPDATA%\moss\moss.lock`，运行中只提示退出不强杀；卸载复选框“同时删除我的数据”默认不勾，勾选才删 `%APPDATA%\moss`，删除前校验路径以 `\moss` 结尾；不碰外部服务端目录。检查：PK-06、PK-07、PK-08。
- [x] **PKG-04 安装器图片**（已生成 header / sidebar；待安装界面人工确认）：横幅与侧栏图按新前端视觉制作。检查：安装界面观感人工确认。
- [x] **PKG-05 交付流程（软件文档完成）**（2026-10-11 用户授权按顺序完成非实机待办）：AGENTS 增加本轮普通 NSIS 非空、时间/大小/SHA-256 检查，并与 NV18 专用包分别记录；README 链接安装与卸载入口，文档索引对齐已实施现状。签名及干净机验收仍归 PKG-06/07。
- [ ] **PKG-06 签名**（正式分发前）：exe 与安装包代码签名；Android release APK 签名。检查：PK-10、PK-11。
- [ ] **PKG-07 验收**：干净 Windows 虚拟机执行 PK-01..PK-09（自定义目录含中文空格、两种安装范围、快捷方式、默认卸载保留数据、勾选删除数据、运行中保护、覆盖升级）。检查：结果记入 §9 日志，未做的标 `unverified`。

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

- [ ] **P0-01 云端大模型能力**
  - 做：用户提供（或自己在对话外测试）OpenAI 协议地址；确认 `/chat/completions` 支持 `tools` 函数调用、流式输出、中文。
  - 检查：一次带 1 个工具定义的请求能返回 `tool_calls`；记录模型名、首包延迟、整句延迟。
- [ ] **P0-02 云端语音识别（ASR）**
  - 做：确认云端是否提供 OpenAI 协议 `/audio/transcriptions`；支持的音频格式与采样率。
  - 检查：一段中文录音能识别；记录延迟。若没有该接口：在 xiaozhi-server 其他 ASR provider 中选替代并记录。
- [ ] **P0-03 云端语音合成（TTS）**
  - 做：确认是否提供 `/audio/speech`；可用的 `model`、`voice`、输出格式。
  - 检查：能合成一句中文并可播放；记录延迟。没有则选替代 provider 并记录。
- [ ] **P0-04 整轮延迟预算**
  - 做：ASR + LLM + TTS 延迟相加。
  - 检查：与用户确认可接受的上限（建议 <8 s）。
- [x] **P0-05 锁定上游源码版本（不代表已部署）**
  - xiaozhi-esp32-server：`87c6df77b0220ddbd8fc804f984f83f69f3225e5`，MIT；mcp-endpoint-server：`0f9302f327100fb976c7bdea6e4ebbb8ebdb5787`，MIT，2026-10-11 官方仓库公开提交/许可核对。
  - 检查：固定版本与许可链接写入 [bench 文档](self-hosted-model-bench.md)；未检出/安装/启动服务，运行兼容仍留 P2a。xiaozhi-client 属 P0-09/P5 可选范围，未决定时不作为本项硬前置。
- [ ] **P0-06 设备固件与 OTA 地址**
  - 做：查明实战派 ESP32-S3 当前小智固件版本，以及修改 OTA 接口地址的方式（配网页面或重新编译）。
  - 检查：写出操作步骤；**实际刷写/修改需用户授权**。
- [x] **P0-07 上游插件与接入点资料**（2026-10-05 源码阅读完成，结论见技术书 §2“上游源码核实”；运行验证留给 P2a）
  - 做：阅读上游 `plugins_func` 插件写法（入参、返回、能否异步播报）、`Intent: function_call` 配置、MCP 接入点部署方式；并为语音中心确认：源码启动命令与入口脚本、用户覆盖配置文件名及 ASR/LLM/TTS 字段名、服务端监听端口、是否有设备在线状态接口。
  - 检查：P2a、P2b、P4 所需信息齐全，写入 bench 文档。
- [ ] **P0-08 部署位置**
  - 做：与用户确定 xiaozhi-server 运行在哪（推荐与 MOSS 同一台电脑、源码部署）。
  - 检查：若用 Docker，记录防火墙限制方案（技术书 §9.1）。
- [ ] **P0-09 是否做 P5**
  - 做：与用户决定海门、xiaozhi-client 是否纳入。
  - 检查：结论写入 §9。

---

## 5. P2a 接入点零代码验证（只读与预览）

前置条件：P0-01..P0-08 完成。**本阶段禁止任何真实发送和 R3 动作**（严格模式未上线，模型可自行确认）。建议激光机断电或不连接。

- [ ] **P2a-01 部署服务端**（需用户授权）
  - 做：最简化部署——在 `main/xiaozhi-server` 下用 Python 3.10 安装依赖，复制 `config_from_api.yaml` 为 `data/.config.yaml` 并按单模块教程修改，`python app.py` 启动；`selected_module` 先用默认 provider 隔离变量；`Intent: function_call`。
  - 检查：启动日志打印 OTA 地址（端口 8003，`/xiaozhi/ota/`）与 WebSocket 地址（端口 8000，`/xiaozhi/v1/`）；浏览器访问 OTA 地址有返回。
- [ ] **P2a-02 设备连上自建服务端**（需用户授权）
  - 做：按 P0-06 修改 OTA 地址。
  - 检查：A-01 设备在线可对话；A-03 能播报。
- [ ] **P2a-03 部署接入点并接 MOSS**（需用户授权）
  - 做：部署 `xinnan-tech/mcp-endpoint-server`（Docker 或源码，默认端口 8004）；从其日志取“单模块部署 MCP 接入点”地址（Docker 部署需把容器内 IP 换成电脑局域网 IP）；把该地址同时填入 xiaozhi-server `data/.config.yaml` 顶层 `mcp_endpoint` 与 MOSS 设置的 `mcp_endpoint`（先备份 MOSS 原值，只记录在本机，不写进文档）。
  - 检查：访问接入点 `health` 地址返回 `tool_connections ≥ 1`（MOSS 桥）且设备对话时 `robot_connections ≥ 1`；`/api/system/info` 显示 MCP 已连接；服务端日志显示拿到 19 个工具。
- [ ] **P2a-04 语音测 R0/R1**
  - 做：语音测试“检查激光机连没连上”“现在状态”“帮我在 3 毫米椴木板上刻你好（只预览）”“推荐参数”。
  - 检查：A-14——预览生成成功，MOSS 中无任何 `sending` 记录；记录每个工具的耗时，确认都在接入点 30 秒超时内。
- [x] **P2a-05 自建系统提示词（文档完成，未部署）**
  - 做：编写自建服务端系统提示（路由表、“开始加工需到电脑确认”、“停”只映射 `cancel`/`emergency_stop`）；存 `docs/integration/xiaozhi-prompts/history/<日期>-self-hosted.md`，在 `latest.md` 注明官方/自建两套的适用范围。
  - 检查：2026-10-11 自建文本已保存至 `history/2026-10-11-self-hosted.md`，latest 区分官方/自建适用范围；预算和链接检查后记录 §9。真实工具发现、播报与延迟仍未验，不写入外部配置。
- [ ] **P2a-06 记录差异**
  - 做：记录工具描述是否被截断、函数调用格式问题、超时。
  - 检查：写入 §9；若接入点实测不兼容，标 `[-]` 写原因，把 P2b 网关 + 插件升为必做。

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
- [x] **P1-09 前端**（2026-10-06 用户本机测试与构建通过；真实流程见 §2a HW-07；E2E 未加入——现有 `test_app_buttons.mjs` 依赖真实后端且无法在测试中开启严格模式与桌面会话，改用预览桩脚本验证）
  - 做：全局审批卡（技术书 §9.5.2）——挂在 `App` 根组件；每 4 秒轮询 `/v1/approvals?status=pending`；显示刀路预览、材料、功率/速度/遍数、目标设备（不显示 IP）、来源、摘要、120 秒倒计时；勾选现场确认后才能批准；`approval_mismatch` / `approval_expired` 在卡内显示原因；批准后接现有 `SendOverlay`（`STAGE_MAP` 不变），急停始终可见；顶栏“审批”按钮带红色数量角标。参照原型 `frontend/ui-preview-voice-center.html` 的 TC-01..TC-04。
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
  - A-70 / A-71 / A-74：`tests/approvalState.test.mjs` + 预览桩脚本；真实流程见 §2a HW-07
  - A-75 / A-76：`tests/authSession.test.mjs` 桌面会话用例、`approval_routes_require_desktop_session`
  - A-50：Android APK 构建与 E2E（用户本机）
  - 检查：§2 全部检查；A-10..A-28、A-35、A-36、A-40、A-41 通过；A-50 Android 构建。
- [ ] **P1-13**（需用户另行授权）首次经接入点 + 桌面审批的实机出光：低功率、小图案、人在旁。检查：结果如实记录，未做则标 `unverified`。

---

## 7. P2b、P3、P4、P5

**P2b 网关与插件（可选增强）**（前置：P1 执行层门禁完成；仅需结果语音播报、会话级幂等/审计或 P2a 实测不兼容且用户决定替代链路时做；[六项技术细则 s1](moss-self-hosted-extension-implementation.md#p2b)，未授权部署）

- [ ] **P2b-00 网关/令牌/幂等**：新路由、Gateway 身份、当前 schema 下一版迁移、令牌哈希/轮换、幂等与执行结果未知；保留 R2、R3、R4 执行层门禁。检查：A-10..A-13、重复请求/冲突/崩溃/审计失败/限流 fake 测试。
- [ ] **P2b-01 上游插件**：锁定上游接口后，在用户授权的 xiaozhi-server 目录添加 `moss_tool`（`ToolType.SYSTEM_CTL`，异步）；动态工具目录、只读审批轮询与 `speak_txt` 播报。检查：假网关测试拒绝、过期、结果未知、会话切换不串播，不在代码或日志存令牌。
- [ ] **P2b-02 提示词**：仅补自建网关模式话术；模型 `confirmed` 无授权力，“停”只映射 `cancel`/`emergency_stop`。检查：历史版本保留，提示词不含私有地址和密钥。
- [ ] **P2b-03 真实服务联调（需授权）**：确认服务端/接入点版本、令牌和安全网络范围，先 fake 再真实服务端。检查：A-12、A-13 与播报结果；无真实验收标 `unverified`。
- [ ] **P2b-04 并存回归**：网关/接入点都必须经同一 R3 门禁，不能双发、不自动重试结果未知的物理动作。检查：A-25、`FakeGrbl`、三端发布构建与 CHANGELOG；真实加工另获授权。

**P3 切换到自有云端模型**（[六项技术细则 s1](moss-self-hosted-extension-implementation.md#p3)；前置：P0 模型接口、P2a 接入点实测与 P1；P2b 只在接入点不兼容且明确采用替代链路时成为前置。本组**仅外部 xiaozhi-server**，不改内置语音配置）

- [ ] **P3-01 ASR**：核实完整 `/audio/transcriptions`、中文与采样/格式，先测试后仅更新 `ASR.MossASR`，失败恢复原分区。检查：A-01..A-04、A-14，延迟/错误码与无密钥日志记录。
- [ ] **P3-02 TTS**：核实完整 `/audio/speech`、模型/音色/输出，先测试再更新 `TTS.MossTTS`。检查：中文播报、打断/延迟，失败不覆盖 ASR 或其他 YAML 键。
- [ ] **P3-03 LLM**：核实流式 `/chat/completions` 和 `tool_calls`，再更新 `LLM.MossLLM`；固定中文样本记录当前模型可见工具的选择/参数准确率，未达标按白名单缩小可见集。检查：R3 只能经桌面审批、R4 拒绝、不改变安全门禁。
- [ ] **P3-04 凭据/配置/回归**：密钥只进外部服务端 `data/.config.yaml`，按分区备份、原子写及失败回滚；`manager-api.url` 模式仅测试不改；切换官方/内置模式不受影响。检查：fake 与真实契约分开记录，真实账号/设备未测为 `unverified`。
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
- [x] **P4-06 模型配置向导**（代码完成：`models.rs` 测试连接、`config_yaml.rs` 按行读-改-写 + `.bak` + 重新解析核对、`center.rs` 先测试后保存；按行编辑逻辑已在云端用 PyYAML 离线校验；上游 `config_loader` 合并校验待有服务端源码时做）
  - 做：大模型、语音识别、语音合成**三个独立分区**，各自填地址、密钥、模型（语音合成另有音色、语速）；测试连接（内存密钥、15 秒超时、不跟随重定向）；保存按技术书 §9.4.4 字段表写入 `data/.config.yaml`（`LLM.MossLLM`：`url`/`api_key`/`model_name`；`ASR.MossASR`：完整 `/audio/transcriptions` 的 `base_url`；`TTS.MossTTS`：完整 `/audio/speech` 的 `api_url`、`model`、`voice`；`selected_module` 指向三者）；YAML 读-改-写保留其他内容并备份 `.bak`；留空的分区不改；检测到 `manager-api.url` 时只测试不写。
  - 检查：A-67——在 MOSS 的 SQLite、`settings.json`、日志中搜索测试用的假密钥，结果必须为空；写入后的 `data/.config.yaml` 可被上游配置加载器解析（用上游 `config_loader` 的合并逻辑做一次离线校验）。
- [x] **P4-07 接口与鉴权**（代码完成：以 Tauri 命令实现 `apps/desktop/src-tauri/src/voice.rs`，只有桌面窗口可调用；HTTP `/v1/voice/*` 全部 404（laser-api 新测试）；移动端命令直接返回 `voice_desktop_only`）
  - 做：`/v1/voice/*` 或等价 Tauri 命令；仅桌面会话。
  - 检查：A-68。
- [x] **P4-08 前端页面**（代码完成：`pages/VoicePage.jsx`、`voiceState.js`、`voiceApi.js`；`npm test` 80 项通过；预览桩截图 6 张入 `tests/visual-baseline/voice-*.png`；E2E 未覆盖——E2E 跑的是 `moss-host`，没有语音中心）
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
- [~] **NV1 协议与 OTA**（2026-10-07 代码完成，见技术书 §13.1；本机 `cargo test --workspace`、`npm test` 已通过；待真实设备验收 HW-08）：`moss-xiaozhi-proto`、`moss-voice-server` 骨架、设备允许流程、token、回声模式。检查：真实设备 OTA 指向 MOSS 后连上并听到回声；未允许设备被拒。
- [~] **NV2 音频**（2026-10-07 代码完成，见技术书 §13.2；Silero 随 NV6 接入，NV2 用能量 VAD；待真实设备验收 HW-08）：Opus、播放节奏泵、VAD、设备打断。检查：无断音跳字；200 ms 静音判停；打断立即停播。
- [~] **NV3 云端对话**（2026-10-07 代码完成，见技术书 §13.3；用假模型与假 HTTP 服务测试；桌面端启用随 NV5；待真实模型与设备验收）：OpenAI 兼容 ASR / LLM（流式 + 工具调用）/ TTS、切句聚合、历史截断、唤醒 / 退出 / 空闲。检查：完整对话走通并记录首句延迟（事件“首句延迟”已提供）。
- [~] **NV4 工具与审批**（2026-10-07 代码完成；当前规则见技术书 §5.2、§13.6，取代 §13.4 的始终严格规则；待真实设备验收）：统一工具路由、`CallerContext::Voice`、设备 MCP 中继；R3 跟随全局严格模式，开启时审批卡 + 语音提示，关闭时沿用现有 confirmed 契约与提示词口头确认；R4 始终拒绝。检查：按当前两档规则复测，不能继续以“始终严格”为预期。
- [~] **NV5 语音中心与密钥**（2026-10-07 代码完成，见技术书 §13.5；待真实模型与设备验收）：链路三选一、内置服务启停与状态、设备允许列表、提供商配置、密钥存 `app_secrets`。检查：密钥不出现在 `settings.json`、日志与接口响应（自动测试已覆盖）。
- [~] **NV6 本地模型**（2026-10-07 代码完成，见技术书 §13.7；真实模型端到端测试本机通过；安装包加入辅助程序留到 NV18；待真实设备）：辅助程序 `moss-voice-local.exe`（GPL-3.0）、Silero VAD（接入 NV2 的 `Vad` 接口）、sherpa-onnx ASR / TTS、模型下载与 SHA-256 校验。2026-10-08 起判停、识别、合成各有可选模型（技术书 §13.10）。检查：断网可用（识别、合成、判停全在本机；大模型仍需云端或本机 Ollama）。
- [~] **NV7 流式云厂商**（2026-10-08 代码完成，见技术书 §13.8；用假密钥验证了真实地址的 TLS 与鉴权；待真实豆包账号与设备验收首句延迟）：豆包流式 ASR / TTS。
- [~] **NV8 接入点增强**（2026-10-08 代码完成，见内置语音技术书 §13.9；待真实小智接入点验收多接入点、心跳与断网恢复）：多接入点、断线重连、工具开关（列表与调用都校验）、调用统计、接入点身份。
- [~] **NV8a 语音体验优化**（软件停止通道已按用户中间结果规则完成，本轮回归/三端/helper证据见§9；实机见HW-09）：
  - [x] **NV8a-1 停止类指令本地快速通道（软件）**：会话`select!`监听`AsrStream::next_interim`，整句停止词进入单次`FastStop`，不等待最终文本或声纹；`interim_whole_stop_executes_without_waiting_for_final_and_once`及本轮workspace/三端构建通过。临时“停止”随后补成“停止之后怎么办”仍可能提前停机（用户接受的取舍），不宣称消除误停；完整否定句若未经临时单句命中仍交普通识别。使用可信语音身份与原有`laser_safe_action_tool emergency_stop`通路；真实0x18/噪声仍归HW-09。
  - [x] **NV8a-2 常用只读指令本地匹配（软件完成）**：固定句式直接调用 R0 工具并用本地模板回答；不确定时交给大模型；默认开、可关闭；失败不编造。证据：技术书 §13.11、§9 联合验证记录。
  - [~] **NV8a-3 识别热词**：X-ASR beam search、云端请求字段、行业/自定义词、SenseVoice 不支持提示和 greedy 选项已有软件验证；真实热词识别率及相对 greedy 速度尚待记录，归 HW-09。
  - [~] **NV8a-4 本地合成边合成边播**：协议已升 3，分段返回和旧版握手拒绝已有自动验证及 helper 构建；真实模型长句首段耗时、设备无断音尚待 HW-09。分段返回不等于模型内部逐采样推理。
  - [x] **NV8a-5 各环节耗时记录（软件完成）**：最近 20 轮内存耗时、模型和字段白名单复制已接线/自动验证，不保存识别/回答正文；真实数字采集另归 HW-09。
- [~] **NV8b 自然打断（设备端回声消除）**（软件实现、7 项专项测试及发布构建见 §13.12/§9；设备 AEC 与真人噪声未验，整体 partial）：
  - [~] **NV8b-1 固件核实**：§8.3 已记录上游 AEC/实时协议依据和板子参考通道；所用设备实际固件编译选项、播放上传和 abort 行为仍待核对，不从板子型号推定 AEC 已开启；本次不刷固件。
  - [x] **NV8b-2 插话检测（软件完成）**：realtime、默认 400 ms、300 ms 开播保护、问候/告别保护、保留开头和 auto 模式不变已有假设备测试；真人效果归 HW-10。
  - [x] **NV8b-3 历史只记已开始播放的句子（软件完成）**：以已发 `sentence_start` 为准，未开始尾句不进入历史；这不证明用户实际听完该句。
  - [x] **NV8b-4 设置与事件（软件完成）**：默认关、时长 300～600 ms、设置往返与界面测试已有记录；真实插话事件与噪声误触发归 HW-10。
- [~] **NV9 第三方 MCP**（2026-10-10 四段集成已落地；发布构建见最新日志，真实服务未验证）：
  - [x] 桌面配置与凭据保存：主窗口及来源校验、配置版本、独立凭据引用、空白保留／移除字段删除、原配置保护。
  - [x] 连接维护：stdio / Streamable HTTP、独立有界队列、心跳／清单刷新、1–60 秒退避、故障不重放、全局预算和冲突提示。
  - [x] 持久审批与审计：external_tool、真实会话及当前目标／schema／凭据／参数校验、单次消费、配置变更及撤销失效、结果未知和审计失败路径。
  - [x] 语音聚合和统计页面：只进入内置语音、三档策略、参数审批卡、内存调用统计、持久审计；1280/960 像素假桥页面操作通过。
  - [ ] 真实第三方服务、账号与真人语音验收；软件发布构建状态以 §9 日志为准。
- [~] **NV10 记忆**（[六项实施细则](moss-native-voice-implementation.md#nv10)；软件功能与发布构建已完成，实际 GUI/真实语音模型/设备留 FINAL）：
  - [x] **NV10-1** schema v7、记忆表/CRUD、revision 条件更新、迁移与并发测试通过。
  - [x] **NV10-2** 已接入按设备摘要注入、会话断开后的有界后台总结，fake LLM 和过滤测试通过；真实模型质量另验。
  - [x] **NV10-3** 开关 generation、设备撤销 epoch、编辑/清空乐观锁，fake 竞态和隔离测试通过，旧结果不会覆盖清空。
  - [~] **NV10-4** 桌面记忆卡片、开关/列表/编辑/清空、主窗口专用 Tauri capability 均已开发；Rust 工作区 485/485、前端单测/构建、FakeGrbl E2E 32/32、Windows release/NSIS 与 Android release APK 构建通过。实际 GUI 按钮操作需 FINAL 验收，本项在此之前仍 partial。
  - [ ] **NV10-5** 真实模型摘要质量、真实小智会话及长期记忆体验测试；统一留到 §10 FINAL，不阻塞独立软件任务。
- [~] **NV11 信息插件**（[六项实施细则](moss-native-voice-implementation.md#nv11)；五种插件软件已接线，未完成项和真实环境留 FINAL）：
  - [x] **NV11-1（供应商软件验收）** 六类供应商固定依据/合成fixture、本机真实TLS请求响应、鉴权/额度/头与正文超时/跳转/有长度和chunked上限/准确1MiB/缺字段/空结果/缓存失效去重已补；HTTP3秒连接/10秒总时限/1MiB、转义后外层2000字符、禁跳转及自动重试；原急停raw保留。定向33 PASS、最终workspace710/7 ignored及本次三端通过，见§9；真实提供方另留NV11-5。
  - [~] **NV11-2** 和风当前天气（配置地址/坐标、API Key/Bearer）和本机农历（1900～2100、显式日期、时区、春节/闰月测试）已接线；真实天气提供方待 FINAL 验证。
  - [x] **NV11-3（供应商软件验收）** RSS/NewsNow、Tavily、Metaso和RAGFlow适配及固定官方SDK/类型依据、六家fixture/TLS矩阵已验；Metaso普通搜索无需专题ID，独立凭据、不暴露文件元数据、HTTP200应用错误分类；NewsNow数字/字符串时间、RSS完整XML/实体、RAG非空来源与响应数据集白名单复核。天气/新闻5分钟缓存、去重及配置/凭据隔离通过，最终回归/三端见§9本次回执；真实源质量另留NV11-5/FINAL。
  - [~] **NV11-4** 桌面配置卡/default off/独立Keep-Set-Clear与供应商选择已接线，实际React+假native验证旧配置、Metaso密钥隔离、失败草稿保留及窄屏；管理仅packaged voice-private（不是main主窗口直接管理）。最新测试/构建见本次§9回执；跨settings文件/多secret存储故障的原子性与真实WebView2操作仍待验收，假桥不证明DB故障回滚。
  - [ ] **NV11-5** 真实账号/限额/供应商数据、真人语音与设备验收；统一留到 §10 FINAL，mock 不等于真实服务通过。
- [~] **NV12 视觉**（[实施细则](moss-native-voice-implementation.md#nv12)；软件与双端产物已验证，真实设备/模型留 FINAL）：
  - [x] **NV12-1** 独立视觉模型、非流式多模态请求/响应、独立配置及提供方本机 fake 4/4 通过；真实服务留 FINAL。
  - [~] **NV12-2** 8003 multipart、设备/client/token 只读鉴权与代次、图片体积/像素/MIME 门禁已实现；假设备 HTTP 5/5 通过，更全面的畸形上传、实际固件 fixture 待补。
  - [~] **NV12-3** MCP initialize 能力与真实端口、全局2/每设备1并发、撤销再允许不恢复旧请求均通过软件测试；超时/取消竞态与图片不落盘快照仍待更完整的覆盖。
  - [x] **NV12-4** 默认关闭的桌面模型、SQLite 密钥、合成图片连接测试及本地主窗口 ACL/React 卡已接线；Rust 511/511、前端测试/构建、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 通过；Android aarch64 编译完成，APK 内库与本轮新 `.so` 的 SHA-256 一致（Gradle 复用旧包时间戳）。
  - [ ] **NV12-5** 真实小智图片上传、视觉模型、摄像头与端到端性能留 FINAL，由用户统一验收。
- [~] **NV13 Claude Code / Codex**（[实施细则](moss-native-voice-implementation.md#nv13)；独立审批/记录/桌面路径卡已开发，真实 CLI 严格锁定）：
  - [~] **NV13-1** 精确版本/help 参数检查、可执行文件与已有工作目录的 canonical/symlink/junction/敏感根校验、固定 argv+stdin 已用 fake 测试；本轮经用户授权真实探测本机 `codex-cli 0.157.1` 及 sandbox/help，详细结果见 NV13 设计 §5；真实沙箱负向测试**发现越界读取和本机网络风险**。
  - [~] **NV13-2** AgentRun 类别和专用执行路由、voice permit、指纹重算、桌面单次批准、原子消费与审计已写入，假执行器端到端通过；默认不暴露 Agent 工具。
  - [~] **NV13-3** Windows Job Object 独立进程夹具已实现：挂起创建→分配 Job→恢复，kill-on-close、最多 8 个进程、300 秒上限、取消/撤销、父子树回收、stdout/stderr 同时 drain、64KiB 单行/256KiB 总输出和环境白名单；Windows fixture 10/10 通过，父子回收重复 5 次。但**仅供隔离测试，未接入生产 AgentRunner**；Codex 真实目录读取与本机网络隔离仍 FAIL，不开放真实 CLI。
  - [~] **NV13-4** schema v8 最近 50 项历史/重启 result_unknown、桌面路径与运行记录卡、主窗口私有 Tauri 权限、fake runner、契约/CHANGELOG 已接线；NV13-3 阶段全量 Rust **529/529**、Windows Job 专项 **10/10**（父子取消回收额外重复 5 次）、前端单测/构建、FakeGrbl E2E **32/32**；Windows release EXE/普通 NSIS 新产物成功，Android APK 构建 exit 0 且本轮 arm64 `.so` SHA-256 一致。普通 NSIS 不是 NV18 专用 sidecar 安装包；真实 CLI 继续禁用。
  - [ ] **NV13-5** 2026-10-11 用户许可无硬件真实测试：在清空环境的临时 `CODEX_HOME`/workspace 内实测原版 `codex-cli 0.157.1` 的 `:read-only` 沙箱，**写入被拒绝，但工作区外 canary 可读取、可连接自建 loopback TCP；严格限定读取的 profile 失败（缺 elevated Windows sandbox backend）**，因此 NV13-5 **未通过**、不得开启。真实模型/用户登录、Job Object、取消/超时/MCP 仍待后续单独验收；`REAL_CLI_SANDBOX_VERIFIED=false`、UI 拒绝 `enabled:true`。
    - **NV13-5 离线容器备选已实测，仍不解锁**：Windows Home 环境无需安装即可使用现有 Docker Linux/WSL2 + 缓存 Ubuntu。`scripts/test-nv13-container-isolation.ps1` 用随机临时目录验证只读绑定、宿主机未挂载文件不可见、禁止写入/默认路由/Windows loopback、进程 8 个、无继承敏感环境及容器清理，成功退出。该夹具 **不是** 真实 Codex Linux CLI/认证/网络/MCP 的端到端验收；需另行授权并实现隔离认证与仅模型 API 受控出站通道，生产保持 `DisabledRunner`。
    - **NV13-5 官方 Linux Codex 安装/离线验收（2026-10-11）**：已在用户授权范围内通过 `scripts/build-nv13-linux-codex-test-image.ps1` 从官方 GitHub 固定 `rust-v0.162.1` 资产下载并核对 GitHub SHA-256，离线构建专用 `moss-nv13-codex-test:0.162.1`；`scripts/test-nv13-linux-codex-image.ps1` 对 **真实 CLI 版本/帮助**与 Docker 无宿主机凭据、仅临时目录只读挂载、禁网、8 进程、退出清理等检查通过。**Codex 内置 Linux 二次沙箱仍因镜像缺 bubblewrap 不能启动**，没有安装额外组件或放宽 Docker 安全参数；模型、受控联网、独立登录与生产审批整合仍未验收，保持禁用。
    - **NV13-5 后续离线测试（2026-10-11）**：派生镜像内安装官方 Ubuntu `bubblewrap 0.6.1`，实际 `unshare --user` 与 `codex sandbox` 均因非特权 user namespace 权限失败（内层 BLOCKED，不放宽 Docker）；另在 `docker --internal` 网络上验证**假 HTTP 模型代理**能回应合成请求、客户端无外网默认路由、未连宿主机回环/无密钥/按次清理，离线 PASS。两个固定测试脚本和安全实施准则见 [`nv13-codex-docker-security.md`](nv13-codex-docker-security.md)。真实 TLS+白名单代理、独立认证、MCP 禁入与审批取消整合均**未实现**，继续禁止生产。
    - **NV13-5 离线 HTTP 策略代码阶段（2026-10-11）**：新增 `laser-host::voice_agent::model_proxy`，固定假 `/v1/responses` 路由 + JSON schema、OS 随机单次短 TTL 令牌绑定 run/device/许可，拒绝其他 Host、绝对 URL、CONNECT、异常头和非法长度、额外工具字段；8/8 Rust 定向/loopback TCP mock 通过。**无真实代理上游 TLS、DNS/IP 限制、独立凭据/模型费用或生产审批与 Docker 集成，继续禁用 Codex。**
    - **NV13-5 本轮软件构建回归**：Rust 537/537、模型代理专项 8/8、前端单测/构建、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 与 Android aarch64 APK 均成功；Android APK 内的 Rust `.so` 哈希一致。保留生产 `DisabledRunner` 与 `REAL_CLI_SANDBOX_VERIFIED=false`；真实代理 TLS/DNS、密钥、费用、真实模型与实机均待后续。
    - **NV13-5 TCP/TLS/Docker 离线端到端安全链路（2026-10-11，partial）**：新增有界真实 TCP framing/server、只接受固定 HTTPS 目标+公网 IPv4 纯策略、模拟 TLS 完整证书主机名校验/拒绝重定向/中途撤销、一次性 token → 假 HTTPS 响应整条 E2E。模型代理专项 **23/23**；固定镜像摘要 Docker Plan 3/3，实际离线 Codex `--version` 探针 1/1 通过（无登录、无模型调用、只有临时 synthetic 挂载）；生产 AgentRunner 没有接入这些模块。真实 TLS 公网、凭据/费用/可信代理出口防绕过、完整 NV13 单次审批+容器撤销、MCP 隔离、Codex 内置 userns 依然待验收。保留禁用。
    - **NV13 最新双端发布复核（2026-10-11）**：Runner 断线后重新编译，Windows release EXE/普通 NSIS 和 Android aarch64 APK **exit 0**，APK embedded `.so` 与最新 Rust ARM64 `.so` 哈希一致。实测构建产物 SHA-256 和字节数见 CHANGELOG；代理 23/23、Rust 555/555、FakeGrbl 32/32、前端回归及固定镜像 Docker 无模型探针通过。与生产执行器未对接，仍 fail-closed；同期存在 NV14 未提交开发改动，不能把构建通过等同于 NV14 验收。
- [~] **NV14 音乐**（[六项实施细则](moss-native-voice-implementation.md#nv14)；真实会话和桌面配置已接线，格式与实体听音待验收）：
  - [x] **NV14-1** 显式本机目录、深度和文件限额、精确/模糊/歧义/随机选择，敏感目录/软链及重解析点排除；按文件句柄/size/mtime 复核。
  - [x] **NV14-2** Symphonia 逐包解码、24kHz mono 重采样、60ms Opus 和2秒内队列；有效纯合成 MP3/FLAC/Ogg Vorbis、WAV、Ogg Opus 不支持、损坏/截断数据和取消专项均通过。格式 fixture 与可重复生成脚本在 `crates/moss-voice-server/tests/fixtures/` 和 `scripts/generate-nv14-synthetic-audio-fixtures.ps1`；真人听音/长曲留 NV14-5。
  - [x] **NV14-3** 受控 PlayMusic 工具与会话、单设备一源/generation、切歌/abort/自然插话/撤销清泵，假小智 WebSocket 点歌/停止测试通过。
  - [x] **NV14-4** 默认关闭的配置卡/原生文件夹对话框、main-only Tauri ACL、Live PlayMusic、契约/CHANGELOG/提示词归档完成；Rust 572/572、前端单测/构建、FakeGrbl E2E 32/32、Windows EXE/普通 NSIS 与 Android APK release 均成功，Android 内嵌 .so 哈希一致。
  - [ ] **NV14-5** 真实小智设备音质、长歌、断音与插话延迟留 FINAL，不操作激光机。
  - **NV14 软件阶段已验（2026-10-11）**：合成 MP3/FLAC/Ogg Vorbis 正常 24kHz PCM，Ogg Opus 明确不支持与截断 FLAC 均覆盖；新解码专项 **6/6 PASS**。本轮 Rust workspace **587/587 PASS**，前端 `npm test` / `npm run build` 成功、FakeGrbl E2E **32/32 PASS**、Windows EXE/普通 NSIS 和 Android aarch64 release 均 exit0；Android APK 内原生库与本轮 .so SHA-256 一致，完整哈希见 CHANGELOG。NV14-1/2/3/4 软件完成；NV14-5 真实听音、长曲、设备插话/安装仍属 FINAL/unverified，未读取个人歌曲或连接激光机。
- [~] **NV15 Home Assistant**（[六项实施细则](moss-native-voice-implementation.md#nv15)；仅 mock 安全核心，真实控制一律关闭）：
   - [~] **NV15-1** 固定 states/services HTTPS、响应上限/无跳转/安全属性白名单、DPAPI 独立凭据、voice-private 配置和显式只读同步已实现，见后续软件回归；真实 HA DNS/TLS/凭据兼容尚未验收，不能从 mock 推定已连接。
  - [~] **NV15-2** 固定 light/switch/climate/lock/alarm/cover/media_player 服务映射和参数上限，未勾选实体/缺失服务/自定义 URL 和未批准媒体拒绝；只生成不会发送的控制计划。
   - [~] **NV15-3** HomeControl 独立类型、SQLite 单次消费/审计、设备/配置/密钥 revision 校验与私有审批收件箱已实现；批准执行仍锁定，生产 POST 未注册。不得复用激光/ExternalTool 免批策略，真实审批 WebView2 操作另验。
  - [~] **NV15-4** 真实HomeControl协调器在cfg(test)内存假服务+临时SQLite验证准备/审批/单次消费/错误摘要/取消/撤销/失败Unknown不重发，7项通过；生产控制仍关闭，真实网络/TLS/家居观测与WebView2人工批准未验。
  - [ ] **NV15-5** 实际家居设备动作必须由用户逐实体授权，留 FINAL。
  - **NV15 本轮离线验证**：`cargo test -p laser-host voice_home --lib` 12/12 PASS，所有 HA socket 为 `127.0.0.1` 合成 GET，不发控制 POST；完整回归和双端产物结果另行记录。

  - **NV15-4 追加合成结果观察（2026-10-11）**：`voice_home/observe_mock.rs` 已在 mock 中区分目标已确认、服务接受但状态未确认和发送后断线/撤销结果未知，最多三次状态查询，不自动重新 POST；`cargo test -p laser-host voice_home --lib` **19/19 PASS**。仍未接入真实 HomeControl 审批、独立密钥/桌面设置/真实 HA POST，NV15-4 保持 `[ ]`，NV15-5 留 FINAL。
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
- [~] **NV16 声纹**（[六项实施细则](moss-native-voice-implementation.md#nv16)、[实际登记说明](nv16-speaker-enrollment.md)；本地模型/登记/按人软件接线已实现，生产识别待真人标定）：
  - **2026-10-12 标定准备**：新增`calibrate-nv16-speaker.mjs`，显式consent/本地WAV/实际helper生成匿名FAR/FRR/15组候选与Wilson上界；6项单测、3组合成参考/5测试片段通过。helper不继承凭据/用户profile，不从项目目录运行；报告不含音频/姓名/向量，不改设置，labels_verified=false/accepted_threshold=null/production_ready=false；真人数据、验收标准和可信报告加载仍未完成，不能开启识别。
  - [x] **NV16-1（本地软件）** CampPlus公开模型/URL/28,281,138字节/hash/Apache声明/16kHz/192维已核实，catalog/下载界面及协议4/独立helper已实现；实际模型+合成PCM通过。外部兼容为可选且未启用，未伪造契约。
  - [~] **NV16-2** 3段私有登记、embedding事务、取消/撤销/代次/1.5秒超时收尾、质量/重复输入、24/48kHz重采样和CAS删除已接入；识别与ASR并行、未知接口和helper适配已实现，生产Calibration=None，真实识别未开启。
  - [~] **NV16-3** 按人历史/摘要核心、SQLite适配、私有本地编辑已实现；快照绑定人revision/目录/设备/会话/授权代次，未知不回退共享摘要，删除/撤销使旧任务失效，停止不等待。真人阈值及未来报告可信加载尚缺，可选仅登记者加工限制未实施，绝不由声纹授予权限。
  - [~] **NV16-4** 私有UI、向量/协议/独立进程/模型替换/登记/ACL/临时SQLite及按人假WebSocket测试已补；实际React确认/二元会话/取消/摘要CAS冲突/草稿恢复通过。CHANGELOG和本轮跨端/专用封包以§9最终回执为准，真人准确率未验。
  - [ ] **NV16-5** 真人同人/异人/噪声/重放标定，报告 FAR/FRR；未标定阈值不得当发布保证。
  - **NV16 离线起步（2026-10-11）**：私有 `moss-voice-server::speaker` 纯合成策略，三段 2..10s 元数据、向量有限值与一致性、模型/generation 隔离、相似度/差距、全局2/设备1/队列16、1.5秒未知及删除失效；**永不授权加工、无真人录音与模型**，4/4 Rust 测试通过。NV16-1 模型校验/许可和 NV16-2..4 真实注册协议/持久化仍 `[ ]`，NV16-5 留真人 FINAL。
  - **NV16-3 离线隔离增量（2026-10-11）**：新增私有 SyntheticPersonMemory，模拟已登记身份的摘要隔离、未知者不可读取、expected revision 冲突和删除清除；`speaker` **5/5 PASS**。该 fixture 不是真实登记/SQLite 持久化或可信授权路径，NV16-3 尚未标完成。
  - **NV16 个人记忆持久操作（2026-10-11）**：RuntimeDb 新增个人摘要只读、按 revision 条件更新、删除声纹记录与记忆同事务清除，数据库专项3/3、workspace624/624；上层可信识别接口与真实向量、审批联动仍未实现，NV16 不提前标为完成。
- [~] **NV17 固件下发**（[六项实施细则](moss-native-voice-implementation.md#nv17)；已有合成安全核心与数据库状态，正式 OTA 鉴权/下载未接线）：
  - **2026-10-12 协议前置**：[可信下载候选v1](nv17-authenticated-download-contract.md)及真实Ed25519/SHA测试夹具5项通过；清单/票据/设备持钥证明防自报身份、篡改/重放/撤销。仍缺可信配对、具体小智固件实现/跨语言fixture/下载路由和恢复验收；不是FluidNC固件，不推送或刷写，NV17-1继续阻塞。
  - [!] **NV17-1** 官方固件当前下载为裸GET，OTA设备/client信息自报，未消费SHA/签名字段；可信下载鉴权/hash协议前置未齐。离线导入/hash/mock已有记录，生产下载保持关闭。
  - [ ] **NV17-2** 默认关闭、型号/版本/hash 确认、候选版本比较与 offer。
  - [ ] **NV17-3** 白名单下载、设备/client/撤销/签名鉴权、hash/并发/路径防护。
  - [~] **NV17-4** 私有只读交付UI和启动中断转unknown已接线，下载/设备上报分开呈现，不重试或冒充刷写；schema/mock/临时SQLite和CHANGELOG已补，真正鉴权的生产记录产生路径仍受NV17-1阻塞。
  - [ ] **NV17-5** 另获具体设备刷写授权后验收；不加入自动降级或回滚。
  - **NV17 安全离线起步（2026-10-11）**：新增不接 OTA 路由的 `moss-voice-server::firmware` synthetic staging/hash/不覆盖写入，单件16MiB、总量256MiB、原始型号 hash 名称、版本规范化、默认关闭、完整 sha/revision 才能假确认；设备版本非法/身份未允许拒绝升级候选，变更或篡改的文件不返回字节；4/4 Rust 模拟通过。真实 device/client 鉴权、OTA 下载签名、持久状态/恢复、下载取消以及实机刷写均未验收，不能打开 OTA 推送。
  - **NV17-3 仅测试模块新增下载许可模拟（2026-10-11）**：`firmware/download_mock.rs` 在 `cfg(test)` 下实现 32 字节一次性随机 token 摘要存储、device/client/board/hash/revision/模拟 credential revision 与有效期绑定、全局 2/每设备 1 在途限制、64 KiB 分块传输中权限撤销。固件模拟专项 **8/8 PASS**（含拒绝重放、换身份、超时、禁用/篡改和传输中取消），但**没有任何生产 OTA 下载路由、真实设备身份、签名下载 URL、数据库持久状态或固件刷写**，NV17-1～4 仍是软件待办，不能标成完成。
  - **NV17-4 合成状态边界补强（2026-10-11）**：新增只在 Rust 内部使用的 FakeDelivery，独立跟踪 Downloaded 与设备 Reported；中断为 Unknown 不允许自动重试，错误设备/文件摘要不能伪造安装回报，固件专项 **9/9 PASS**。仍缺真实持久数据库及 OTA HTTP 身份路由，NV17 仍待办。
  - **NV16/NV17 新增正式 SQLite 迁移 v9（2026-10-11）**：独立说话人表、按人记忆表和固件下载/设备上报分离状态表；`recover_firmware_downloads` 恢复中断状态为 Unknown、revision 递增、不自动重试。旧迁移兼容与 SQLite 重启集成测试通过；真正声纹模型连接器、设备下载鉴权/OTA 路由仍未接通，不应当作全功能完成。
  - **NV17 SQLite 条件状态更新（2026-10-11）**：`RuntimeDb::update_firmware_download_if_revision` 新增生产级状态前置条件和修订版本原子检查，防旧请求重放/Unknown 自动复活；SQLite 集成 2/2、Rust workspace 623/623、前端 103/103、FakeGrbl 32/32、Android APK 构建通过。Windows release 编译成功后 EXE 文件被运行中的 PID 49120 占用，Tauri 打包未完成，未关闭用户进程。正式 OTA 鉴权与下载路由仍未接线。
- [~] **NV18 收尾**（[六项实施细则](moss-native-voice-implementation.md#nv18)；sidecar、helper 构建/源码暂存已接线，第三方静态依赖许可完整性和安装/升级验收待做）：
  - **2026-10-12 递归候选补充**：解析固定ORT根tar，核对41个deps声明，35源码+3gitlink候选实际收集431,766,263字节、SHA/许可核对通过；6主机工具二进制不冒充源码。匿名release/attestation未取得实际构建证明，候选内部递归及预编译配置/链接集合还待对应审核，严格发行保持拒绝。新增`native-license-evidence/ort-dependency-evidence.json`随专用内部包收集，不能因此勾选NV18-3完成。
  - [x] **NV18-1** 安全无损构建入口、独立 helper / source / COPYING / 初版 notices、动态 sidecar overlay、新版 release-manifest 与哈希核对均实现；从封包暂存源码离线 release 重编译也通过。**原生第三方完整许可证归 NV18-3 未验收**。
  - [x] **NV18-2（本轮软件）** 2026-10-12阶段B：Rust workspace698 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS、前端构建、独立helper构建检查/实际合成推理、常规Windows EXE/NSIS、专用带helper NSIS及Android arm64 release均exit0；独立SHA/文件内容对照见§9。不等于许可闭环、安装或真实设备已验收。
   - [~] **NV18-3** 专用 NSIS 包内 helper/源码/许可文件已实际核查，从暂存源码独立重建已通过但二进制未达到字节级可复现；第三方完整许可/原生链接依赖对应源码及安装后的 helper 协议仍未验，不能对外分发。
  - [ ] **NV18-4** 获授权后干净环境安装、升级迁移失败恢复、备份回退、卸载和 helper 启停验收。
  - [~] **NV18-5** Android arm64 APK 与 Rust `.so` 真实哈希对应及正式构建已完成；真实模型/小智设备、干净机、签名与发布仍待用户另行批准，不能标为完整验收。

  - **NV18 无损构建与实际 archive 核查（2026-10-11）**：独立 `scripts/build-nv18-sidecar-safe.ps1` 编译 helper 及带 voice sidecar 的 NSIS exit 0，按 GUID 创建暂存源码/许可证与 `target/nv18-.../release-manifest.json`，sha/bytes/协议3/schema8已与文件交叉核对。原 `build-windows-installer.ps1` 已转接到无损脚本，不再执行旧递归删除。新增 `scripts/verify-nv18-nsis-in-docker.ps1`，官方 Ubuntu p7zip 断网/只读扫描生成 NSIS **22 个条目**，确定包内含 `moss-desktop.exe`、`moss-voice-local.exe`、COPYING、THIRD_PARTY_NOTICES 和 helper 对应源码。临时检查镜像已清理。**NV18-3 只有文件在包内通过；第三方原生许可完整性、对应源验证与干净机安装仍未通过，不得对外发布。**
  - **NV18-2 软件回归完整回执（2026-10-11）**：Rust workspace **609/609**，前端单测/构建、FakeGrbl **32/32**，独立 GPL helper 与专用 NSIS、常规 Windows EXE/NSIS、Android arm64 APK 全部 exit 0；APK 内 Rust `.so` 与本轮编译产物 SHA-256 一致，具体版本/字节/哈希见 CHANGELOG。
  - **NV18-3 高强度内存解包对照**：NSIS 内 helper 完整 SHA-256 与清单一致；desktop 因 Tauri `__TAURI_BUNDLE_TYPE_VAR_UNK`→`NSS`，仅限 3 字节差异且在内存模拟该修补后 SHA-256 与解包内容一致；其余字节必须完全一致。已从封包暂存源码独立 offline release 重编译 helper（源码可编译，**二进制 SHA 不同**）。Cargo metadata **147 crates 全有 SPDX license 字段**，但 native sherpa/espeak-ng 完整源码与许可文本、干净系统安装仍未通过；详见 [审计](../developer/nv18-source-license-audit-2026-10-11.md)。
  - **NV18-3 原生许可审查新增自动门禁（2026-10-11）**：`sherpa-onnx-sys 1.13.8` 实际链接 13 个 native 静态库，使用的预编译 archive **123,206,268 字节**、SHA-256 `56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf`；解包后共 14 个 `.lib`、许可证文本 **0**，现有初版 THIRD_PARTY_NOTICES 未逐项涵盖 12 个静态链接组件。新增只读 `scripts/audit-nv18-native-licenses.ps1`，诊断模式 **14 个阻塞**、`release_ready=false`，严格 `-RequireComplete` 已验证 fail-closed。对应原生源码/许可资料仍未补全，**不是只剩下设备手测，不可对外分发**。详见 [NV18 许可审计](../developer/nv18-source-license-audit-2026-10-11.md)。

- [ ] **NV19 长期记忆与智能上下文**（[详细方案](moss-native-voice-implementation.md#nv19)；已确认纳入后续计划，**八项全部未开发**；先处理原 NV13/15/16/17/18 安全与交付阻塞，不更改旧进度）：
  - [ ] **NV19-01 结构化记忆模型**：定义五类记忆、可信 scope、来源/版本/候选确认状态。验收：设备/个人/项目边界矩阵和来源契约单测通过。
  - [ ] **NV19-02 SQLite 存储与 CRUD**：从实际下一 schema 迁移，独立条目/索引/CAS/删除/备份；旧 NV10 数据保持可用。验收：迁移、回滚、重复执行、冲突、旧任务不得复活删除记录。
  - [ ] **NV19-03 异步候选提取**：用户明确开启后仅从已提交的有界文字提取；不保存原音频/完整对话/凭据/审批/工具正文。每次提取写变更清单并可整批撤销；设备/材料/校准不提取进记忆。验收：假 LLM、并发/撤销/清空负测，候选经验不冒充已验证，撤销后记录与清单一致。
  - [ ] **NV19-04 智能记忆检索**：受信身份优先过滤，FTS5 `trigram`（少于 3 字退回 `LIKE`），Top-K/Token 预算；快速模式（语音默认，不经大模型）与深度模式（桌面）；闲聊/急停/本地快捷命令跳过检索；设备/材料/校准直接查现有表；失败回退现有 NV10。验收：固定中文检索集（含两字短词）、零跨身份泄露、来源/时效明确、闲聊与急停轮次无检索调用。
  - [ ] **NV19-05 分层知识库**：用户明确选文件/目录；目录 L0（约 256 字）/L1（约 4000 字）+ 原文 L2 按需读，一次过滤搜索不逐层下钻，复用 NV11 RAGFlow 白名单。验收：敏感路径和重解析点拒绝、资料更新/删除与来源正确、不信任文档指令。
  - [ ] **NV19-06 设备故障经验**：存设备/固件条件、故障现象、排查与验证证据。验收：同类召回、错机型/过期版本不套用，经验不能授权控制硬件。
  - [ ] **NV19-07 跨会话项目只读恢复**：摘要固定为决策、待办、证据、未验证项四节；重新核实代码和实际任务。验收：撤销/结果未知不重放，NV13 生产 Runner 仍禁用。
  - [ ] **NV19-08 私有 UI 与全端回归**：分类、查找、来源、确认、编辑/清空、备份、存储/模型开销提示；新功能默认关闭。验收：主窗口 ACL、隔离/迁移/质量/性能及 Rust/前端/E2E/Windows/Android 正式回归；真人/实机另留 FINAL。
  - **不预先排期的可选增强**：OpenViking 独立 HTTP 服务（需 Python 3.10+/uv，Windows 走 Docker，需 embedding 与视觉语言模型；打进安装包即触发 AGPLv3 分发义务）、向量搜索及 Skills 智能发现；先比较成本/性能/许可证并另获安装部署许可。

**语音体验 · 可选项**（2026-10-08 列入，**尚未决定是否实施**，不排期；设计方向见内置语音技术书 §8.2，用户决定后再细化并排期）

- [ ] **VO-1 语义判停**（可选，未决定）：除静音长度外，按句尾语气词、数字或单位未说完等规则延长等待，必要时加轮次判断小模型（中文支持需核实）。
- [ ] **VO-2 端到端语音模型**（可选，未决定）：豆包实时语音大模型、OpenAI Realtime 等作为可选提供方，用于闲聊；工具调用仍走语音身份与审批，停止快速通道需本地并行识别或关键词检测。
- [ ] **VO-3 严格模式关闭时的本地口头确认**（可选，未决定）：待执行的运动 / 出光调用须由下一句在本地匹配到确认词且参数与复述一致才放行；可结合声纹（NV16）限定确认人。

**P5 可选扩展**（[六项技术细则 s1](moss-self-hosted-extension-implementation.md#p5)；P0-09 尚未确定时不排期，先核对与 NV9/NV13 的功能重叠）

- [ ] **P5-01** xiaozhi-client 聚合层：先锁定版本/传输/工具开关和命名规则、查与 NV9 是否重复；若采用则只提供工具，不增加第二个音频编排者，R3/R4 规则不变。检查：假 MCP 重名、撤销、重复调用、断线不重放。
- [ ] **P5-02** 海门 Agent：先确认真实对外工具接口和权限，禁止监听设备音频、继承 MOSS 凭据或直接控制激光。检查：只读/命令/MCP 负向测试；真实联调另获授权。

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

| 日期 | 步骤 | 结果 | 说明 / 未验证项 / 下一步 |
| --- | --- | --- | --- |
| 2026-10-05 | 技术书 r1→r3 | 完成（文档-only） | 未写代码、未运行测试与构建；下一步 S1-01（待用户确认）与 P0-01 |
| 2026-10-05 | 技术书 r4：语音中心 | 完成（文档-only） | 新增 §9.4、D12，P4 改为语音中心 11 步；P0-07 增加启动命令/配置文件/状态接口确认 |
| 2026-10-05 | 上游源码阅读 → 技术书 r5 | 完成（文档-only） | 读 xiaozhi-esp32-server `87c6df7`：正式链路改为 MCP 接入点（无需插件/网关），网关+插件降为可选 P2b；三模型字段映射写入 §9.4.4；P0-05 部分完成、P0-07 完成 |
| 2026-10-05 | 技术书 r6：桌面端前端改造 | 完成（文档-only） | 新增 §9.5（语音中心页、全局审批卡、顶栏真实状态 S2、桌面会话头、视觉提亮 UI-1）；清单新增 S2、UI-1，细化 P1-07、P1-09、P4-08；原型 frontend/ui-preview-voice-center.html |
| 2026-10-05 | 技术书 r7：前端整体改版 UI-2 | 完成（文档-only） | UI-1 视觉提亮升级为 UI-2；原型 frontend/ui-preview-forge-v2.html 改为曜石设计 + 流浪地球太空舱背景；**设计为草案，下次会话先按用户意见继续修改原型（UI-01）** |
| 2026-10-05 | 原型背景改为动画太空舱 | 完成（原型与文档） | 加行星发动机光柱、流星、视差、旋转光环；性能自动降级；技术书 §9.5.5 C 与 UI-03 已同步。设计仍为草案 |
| 2026-10-05 | 原型背景提供四种动画 | 完成（原型与文档） | 新增木星引力弹弓（默认）、等离子流体（WebGL）、超空间跃迁，保留太空舱；原型顶部可切换；待用户选定 |
| 2026-10-05 | 背景：木星换成流动星河 | 完成（原型与文档） | 用户否定木星；新增流动星河（银河星云带漂移旋转 + 上千星尘沿流场带拖尾流动 + 流星 + 550W 核心）并设为默认 |
| 2026-10-05 | 背景选定等离子流体并接入现有前端 | partial | 改 `frontend/src/components/CyberBackground.jsx`、`frontend/src/styles.css`（删除网格/扫描线/罗盘/HUD 样式），CHANGELOG 已记。已验证：`npm test` 44 项（云端）、esbuild 打包后 Chromium 渲染（正常/降级/无 WebGL 回退/减少动态效果）。未验证：`npm run build`、E2E、Windows/Android 构建、真实显卡帧率——云端无法安装 npm 依赖，本机命令行因 Windows 更新无法挂载目录 |
| 2026-10-05 | 安装包与卸载规范 | 完成（文档-only） | 新增 docs/developer/packaging-install.md、技术书 §9.6 与 PKG 工作包、清单 §3c；修正快速开始 / 架构总览 / 数据库文档中运行目录的错误描述（Windows 实际为 %APPDATA%\moss） |
| 2026-10-05 | 修复安装版 `.env` 读取与迁移默认目标 | partial | `config::load_env_files` 额外读 `%APPDATA%\moss\.env`（不覆盖已有变量）；`RuntimeRoot::default_base()`，`moss-host migrate` 默认目标改为实际数据目录；新增 4 个单元测试，云端用替身依赖编译通过 6 项；未在真实依赖下 `cargo test`、未做 Windows/Android 构建 |
| 2026-10-06 | S1-01 读现状 | 完成 | 输出 CORS 头的位置：`http_options`、`http_json_with_headers`（经 `http_json`）、`http_bytes_with_disposition`（经 `http_bytes`）共 3 个构造函数，均写死 `*`；`http_sse` 不带 CORS 头；`feedback.rs` 中的 HTTP 文本仅为测试假服务器。响应构造函数拿不到请求 `Origin`。前端无自定义请求头；E2E 用 Node `fetch`，不发 `Origin` |
| 2026-10-06 | S1-02 定方案 | 完成 | 原 `handle_http_from` 改名 `route_http`；新 `handle_http_from` 解析 `Origin`/`Host`，先校验再路由，允许时统一在状态行后注入 CORS 头。`InProcessBackend` 经 `handle_http` 不带 `Origin`，行为不变 |
| 2026-10-06 | S1-03..07、S1-10 | 代码与文档完成，partial | `is_allowed_origin`（含 5173，与 `authSession.js` 一致）、`parse_allowed_origins` / `allowed_origins_from_env`、`ApiService::with_allowed_origins`、`laser-host` 接线；403 加上 `Forbidden` 原因短语；改写 2 个旧测试并新增 6 个测试（laser-api 5、laser-runtime 1）。云端无 crates.io：仅用替身依赖编译了新函数与入口逻辑并通过断言，`rustfmt` 解析通过；**未跑** `cargo test --workspace`。文档：api-contracts §1.1/§1.6/§5、safety §2、quick-start 变量表与常见问题、技术书 §6.5（含 DNS 重绑定残留风险） |
| 2026-10-06 | S1-08、S1-09、S1-11 | 待本机 | 需本机运行：`cargo test --workspace`、`cd frontend; npm test`、`npm run test:e2e`、`npm run build`、`cargo tauri build`、Android APK；实测桌面端与 Android 实际 `Origin`（预期 `http://tauri.localhost`） |
| 2026-10-06 | S1-12 DNS 重绑定防护（用户要求提前修复） | 代码与文档完成，partial | `is_allowed_loopback_host` + `handle_http_from` 前置 Host 校验（仅回环连接，任何方法 403 `host_not_allowed`）；改 1 处测试夹具（本机申请配对码改用 `Host: 127.0.0.1:9666`，断言未改）；新增 2 个测试。替身编译通过；真实 `cargo test` 待本机 |
| 2026-10-06 | 本机验证（用户执行） | partial | `cargo test --workspace`：laser-api 49/50，唯一失败为 `golden_api_snapshot_parity_test`（本机插着 COM7，串口枚举进了快照，与 S1 无关）——已改为可注入的串口枚举并新增测试，待重跑；`npm test`、`npm run test:e2e`、`npm run build` 通过；Windows exe 与 Android APK（未签名）构建成功。S1-08 完成；S1-09（桌面与 Android 实际 `Origin` 实测）仍待做 |
| 2026-10-06 | 本机 `cargo test --workspace` 复跑 | 通过 | 串口快照修复后全部 crate 测试通过（laser-api 51 项等） |
| 2026-10-06 | S1 收尾 | 完成（Android unverified） | 修复 `get_backend_origin` 未登记 Tauri ACL 导致的启动失败页；用户本机：`cargo test --workspace` 全绿，新 `moss-desktop.exe` 直接进入主页。Android APK 为 ACL 修复前构建，需重新打包实测 |
| 2026-10-06 | S2-01 读现状 | 完成 | 编造值位置：`laser-api` `/api/system/info`——`client_info` 回退 `xz-mcp-broker v0.0.1`、`recent_events` 写死 `16:00:02` 五条、`last_request` 回退 `tools/list`、`messages_count` 取 `max(实际,3)`、`tools_count` 固定 20、`configured` 有活动即为真；`laser-mcp` initialize 缺 clientInfo 时回退 `xz-mcp-broker`/`0.0.1`、tools/list 事件写死“20 项”、`record_event` 把日志条数当消息数；前端 `ConsoleDrawer` 回退值、`Navbar` 以 `configured` 显示 CONNECTED、`SettingsModal` 显示“已配置/已连接过” |
| 2026-10-06 | S2-02..04 | 代码完成，partial | `McpTelemetry::{new,set_connected,record_message}`、`connected` 字段；`laser-mcp` `LinkGuard`；`laser-api` `mcp_status_json` + `voice`；前端 `state/mcpStatus.js`、Navbar 四态胶囊、ConsoleDrawer/SettingsModal 去编造。新增测试：laser-contracts 1、laser-api 2、laser-mcp 断言 2 条、前端 2（`npm test` 47/47 已在本机工作区跑通）。Rust 未编译（工作区无 cargo、无网络） |
| 2026-10-06 | UI-01 定稿 | 完成 | 用户确认按技术书 §9.5.5 与原型实施；P0 模型连通性暂无模型可测，标记阻塞 |
| 2026-10-06 | UI-02/03 第一批 | partial | `styles.css` 顶部改为曜石令牌并把旧变量名映射到新色板；机械替换 JSX/CSS 中写死的青色、霓虹绿红与藏青底色；按钮（次要/白底主按钮/绿色开始加工带光环/红色危险）、输入、`.hud-card` 玻璃卡片与光标聚光（`spotlight.js`）重写；新外壳：`SideNav.jsx` 左侧导航（≤900px 折叠为顶部横排）、`Navbar.jsx` 顶栏（页面标题、硬件、小智四态、常驻急停 `estopBus.js`：交给当前页面执行其原急停 / 取消动作，无任务时如实提示用硬件急停）。验证：前端 `npm test` 49/49（本机工作区）；esbuild 打包 + Chromium 截图（预览桩数据）三页与窄屏无脚本错误。未做：Geist 字体自托管（无字体文件）、`npm run build`、E2E、三端构建 |
| 2026-10-06 | UI-04..07 第二批 | partial | 用户检查第一批后同意继续。雕刻工作台 `HomePage.jsx` 改为三栏（去掉与顶栏重复的大标题，原元素与处理函数原样移入新卡片），控制台页签改为“终端 / 小智网关 / G 码行视图”；创作画板 `DrawPage.jsx` 改为左大画布 + 右三卡；材料实验室 `MaterialLabPage.jsx` 材料列表改为表格（每行显示厚度胶囊，点击调用原 `handleSelectMaterialAndThick`），矩阵按钮行可换行；弹窗统一样式。验证：`npm test` 49/49；Chromium 预览桩渲染 1600 / 1280 / 1100 宽度与偏好设置弹窗无脚本错误、无横向溢出。未做：`npm run build`、E2E、三端构建、功能对照清单真机逐项核对、视觉基线截图、Geist 字体自托管、旧样式规则清理 |
| 2026-10-06 | 用户反馈：画布导出不行；要求设置可折叠 | 代码完成，partial | 导出改走 Tauri 命令 `save_export_file`（写入“下载”文件夹，返回路径）；卡片与偏好设置分区可折叠（`FoldHeader`）。待用户本机重建后按测试清单验证 |
| 2026-10-06 | UI-2 收尾 | 完成（HW-01/02 待实机） | 用户本机：`cargo test --workspace`、`npm test`、`npm run build`、`cargo tauri build` 通过；画板 PNG / 场景导出、空画板提示、材料库导出、卡片折叠与记忆、偏好设置分区折叠均验证通过。删除无引用的旧样式规则 13 条（`.site-header`、HUD 四角、监视舱准星、`.hud-pill`、`.preview-image-card`、`.btn-job-dispatch` 等）；生成视觉基线 5 张。需实机的测试移入 §2a |
| 2026-10-06 | PKG-01/03/04/05 | 配置完成，partial | 用户选择先做安装包。`tauri.conf.json` 开启 NSIS（both、中文、开始菜单 MOSS、embedBootstrapper、图标与图片、钩子）；`installer/hooks.nsh` 运行中拦截与卸载删数据；生成安装器图片；快速开始与规范文档更新。Tauri 钩子位置与变量名依据官方模板源码核对；未能在本地打包验证 |
| 2026-10-06 | PKG 本机验收 | 完成 | 用户本机打包并完成安装、自定义目录、开始菜单启动、运行中拦截、卸载保留 / 删除数据测试 |
| 2026-10-06 | EXP-01..05 | 代码完成，partial | 壳层 `export.rs`：`get_export_settings`、`pick_export_dir`、`reset_export_dir`、`set_export_ask_each_time`、`open_export_dir`、`reveal_exported_file`，改造 `save_export_file`（自定义目录、每次询问、`export_dir_unavailable`、`fallback_default`）；`tauri-plugin-dialog` 仅桌面；`settings.json` 的 `export` 键临时文件 + 重命名写入；前端 `exportFile` 交互、`showChoice` 三态对话框、偏好设置“导出”分区；laser-api 测试确认 `/api/settings` 不接受导出目录且保留 `export` 键。前端 `npm test` 60 项通过；Rust 未编译（工作区无 cargo） |
| 2026-10-06 | 技术书 r9：导出目录设置 | 完成（文档-only） | 用户要求 App 内可选择导出目录。新增技术书 §9.7、工作包 EXP、验收 A-82..A-88、清单 §3d；核实现状：`save_export_file` 固定写“下载”文件夹。未写代码；下一步 EXP-01（待用户确认开始） |
| 2026-10-06 | EXP-06 | 完成 | 用户本机构建并按 1–8 项测试导出目录（A-82..A-85、A-88）全部通过 |
| 2026-10-06 | P1-00 | 代码完成，partial | 用户暂无模型，P0 / P2a 连通性暂缓，先做不依赖模型的 P1。`CallerContext` 定义于 laser-contracts；`handle_http_with_ctx`（InProcessBackend → McpBridge）；`route_http` 构造上下文并传给发送 / 安全动作 / 走边框入口；POST 入口删除请求体身份字段（A-36 测试）。既有测试只补实参、断言未改。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-00 验收 | 完成 | 用户本机 `cargo test --workspace` 通过 |
| 2026-10-06 | P1-01 / P1-02 | 代码完成，partial | `laser-contracts/src/approval.rs`：`ApprovalStatus`（含迁移规则）、`ApprovalKind`、`ApprovalSource`、`ApprovalConnection`、`ApprovalOutcome`、`ApprovalRecord`（`connection` 不序列化）、`AuditAction`、`AuditResult`、`AuditDetail`、`AuditEntry`、`audit_client`、TTL 夹取与 JSON Schema。`laser-runtime`：schema v4 `approvals` 表与索引，`append_audit` / `recent_audit` / `schema_version`；A-41 测试（幂等、失败回滚后重试、旧版打开新库）。既有测试 `runtime_schema_seeds_twelve_builtin_cells` 的版本行数 3 → `SCHEMA_VERSION`（4），属 schema 升级的必要更新，非放宽。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-01 / P1-02 验收 | 完成 | 用户本机 `cargo test --workspace` 通过 |
| 2026-10-06 | P1-03 | 代码完成，partial | `laser-api/src/approval.rs`：`ApprovalService`（创建且按 `(source, request_id)` 幂等、批准、拒绝、到期、重启全部过期、单次消费、执行结果）、`approval_digest` + `canonical_json`、`record_digest_facts`、`resolve_approval_target`（只用显式参数与 `.env` 默认值，不枚举串口）。`laser-runtime`：approvals 读写与条件迁移（单条 UPDATE 原子）。测试覆盖 A-22、A-23、A-32、A-35。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-03 验收 | 完成 | 用户本机 `cargo test --workspace` 通过 |
| 2026-10-06 | P1-04 | 代码完成，partial | `MOSS_FUSION_STRICT`（默认 false，写错取值或开网关不开严格模式时启动失败）；`require_r3_authority` 接入 `launch_send_job_for`（workflow / 文字任务 / 标定矩阵发送与断点续雕）、`/api/safe-action` R3 动作、`/api/device/framing`；小智桥接 / 局域网 / 网关 → 202 `approval_required`，回环 → 403 `strict_mode_blocked`，审批身份 → 单次消费并使用保存的连接；启动时遗留审批全部过期。走边框设备阶段原样抽成 `run_framing`。测试覆盖 A-21、A-23、A-24、A-25、A-26、A-27、A-40。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-04 验收 | 完成 | 用户本机 `cargo test --workspace` 通过 |
| 2026-10-06 | P1-06 | 代码完成，partial | `laser_contracts::tool_risk`（R0–R4，按工具名 + action / confirmed 等参数；不认识的工具返回 None 按 R4）、`tool_never_exposed`；`HostBackend::strict_mode`（默认 false，进程内后端取 ApiService 开关）；接入点严格模式：R4 / 未分级 → `tool_not_allowed`，采用反馈建议 → `strict_mode_blocked`，R3 照常交给执行层生成审批，R0–R2 不变；`tools/list` 与 `/api/system/info` 的工具数在严格模式下去掉 4 个永不开放的工具。测试：清单内 20 个工具全部有分级（A-28、A-20、A-25、A-40）。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-06 验收 | 完成 | 用户本机 `cargo test --workspace` 通过 |
| 2026-10-06 | P1-07 | 代码完成，partial | 后端：`enable_desktop_session` / `check_desktop_session`（仅回环，常量时间比较，错误 403 `desktop_session_invalid`），有效会话 → `CallerContext::Desktop`；`/v1/approvals` 列表、批准（重算摘要 → 批准 → 以审批身份重放原请求）、拒绝，仅桌面会话；schema v5 `approvals.request`；安全动作 / 走边框摘要要素抽成共用函数。桌面壳：命令 `get_desktop_session`（Android 返回不可用），已登记 build.rs 与 capability。前端：`refreshDesktopSession`、`fetchWithDesktopSession`（只对相对路径加头，403 `desktop_session_invalid` 时只重试一次），`apiFetch` 与画板改用之。测试：laser-api 5 个（A-21、A-22、A-75）、laser-runtime v5 迁移、前端 1 个；`npm test` 61 项通过（用户电脑工作区）。既有迁移测试中写死的版本号 4 / 5 改为 `SCHEMA_VERSION` / `SCHEMA_VERSION + 1`，模拟 v3 的删除条件改为 `version>=4`（schema 升级的必要更新，非放宽）。`npm run build` 在用户电脑工作区无法运行（rollup 缺少 Linux 原生包），未执行；`cargo test --workspace`、`cargo tauri build`、Android 构建需用户本机 |
| 2026-10-06 | P1-07 验收 | 完成 | 用户本机 `cargo test --workspace`、`npm test`、`npm run build`、`cargo tauri build` 通过；严格模式实测：桌面端发送通过检查、浏览器发送被拒、无会话访问审批接口得到 `desktop_session_required`；测试后已删除 `MOSS_FUSION_STRICT`。同时确认 PKG-02（安装版读取 `%APPDATA%\moss\.env`） |
| 2026-10-06 | P1-08 | 代码完成，partial | 审批服务在创建 / 批准 / 执行时与审计同事务写入（审计失败即回滚并 503 `approval_unavailable`）；拒绝、过期、失败尽力写审计；模型自填 `confirmed=true` 记 `confirmed_field_stripped` 并从保存的执行参数中删除；回环被严格模式拦截记 `strict_mode_blocked`；接入点 R4 拒绝与采用反馈拦截经 `HostBackend::audit`（默认空实现，laser-host 写库）记录。测试：A-20、A-34 及审计全流程。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-09 | 代码完成，partial | `ApprovalCenter.jsx`（全局审批卡 + 批准后的发送覆盖层）、`approvalState.js`（纯逻辑，6 项单元测试）、顶栏“审批”按钮与数量角标、`SendOverlay` 可选急停按钮 / 取消文字 / 标签。`npm test` 通过（用户电脑工作区）；云端预览桩 + Playwright 验证：自动弹出、未勾选不可批准、稍后处理与顶栏重新打开、`approval_mismatch` 卡内显示且只剩“关闭”、批准后四步覆盖层带急停并走到完成、倒计时归零自动关闭并切到下一条、拒绝、窄屏布局，无页面报错；视觉基线新增 3 张。未执行：`npm run build`、`cargo tauri build`、人工检查（需用户本机）；E2E 未加入 |
| 2026-10-06 | P1-10 | 代码完成，partial | 审批结果新增进展 `phase`（running / completed / failed / cancelled / unknown）；经审批的发送开始前记 running，发送线程结束时经 `JobTerminalHook` 写回完成 / 失败 / 取消；启动恢复把仍为 running 的已执行审批记为失败、`result_unknown`（不重放）。测试：A-31（无人处理到期）、A-33 单元与集成（SQLite 同库“崩溃重启”：workflow failed、审批结果未知、重启后不连接设备、授权不能再用、迟到回写不覆盖）、作业完成写回。HW-07（审批卡真实流程）加入 §2a。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1-10 修复 + P1-11 | 代码完成，partial / 文档完成 | 用户报告 `restart_during_approved_send_marks_result_unknown_and_never_replays` 失败。根因：`recover_after_restart` 只改 workflow 状态列、不改记录正文（状态查询返回正文，重启后仍显示“发送中”），且完全不处理标定矩阵；另外测试用“只确认 2 行”的模拟设备，在途时序不稳定。修复：workflow 与标定矩阵的列和正文同时改为 failed；测试改用“首条加工行阻塞到放行”的模拟设备；新增 laser-runtime 恢复测试。P1-11：架构、功能字典、API 契约、数据库、快速开始“严格模式”文档同步。P1-12 验收对应表写入本清单。未执行 `cargo test --workspace`（需用户本机） |
| 2026-10-06 | P1 验收 | 完成 | 用户确认 P1 构建与测试全部完成（P1-08..P1-12）；P1-13 实机出光待另行授权。下一步按用户要求开始 P4 语音中心（前置“P2b 至少跑通一次”暂缓：P2b 为可选网关，用户暂无服务端；P4 先用假子进程与假数据完成，真实服务端验证留待 P2a） |
| 2026-10-06 | P4-01 / P4-02 / P4-04 | 代码完成，partial | 新 crate `crates/moss-voice`（只供桌面端，不依赖 Tauri，不进入 laser-api / moss-host / Android）：`settings`（`settings.json` 的 `voice` 对象、链路模式、官方接入点备份、读-改-写保留其他键）、`redact`（A-66 脱敏）、`logfile`（5 MB × 3 轮转、写入与读取均脱敏）、`supervisor`（单实例、端口探测识别外部实例、先请求退出 5 秒后强制、Windows Job Object、崩溃记录退出码不自动重启）。云端在 Linux 上用去掉 serde 的副本跑通 7 项测试（`sh` 假子进程：启动 / 重复启动拒绝 / 停止、外部实例、崩溃、脱敏、轮转）；Windows 分支（cmd 假子进程、taskkill、Job Object）未在云端执行。前置“P2b 至少跑通一次”暂缓（见上一条）。未执行：`cargo test --workspace`（需用户本机，首次会拉取 windows-sys 的新特性编译） |
| 2026-10-07 | P4-03 / P4-05..P4-10 | 代码完成，partial | `moss-voice` 新增 `center`（链路切换含回滚、服务端启停、日志、向导编排、健康检查轮询）、`models`（测试连接：15 秒超时、不跟随重定向、错误不含地址与密钥）、`config_yaml`（按行读-改-写 `data/.config.yaml`，保留注释、`.bak`、`serde_yaml` 核对、全模块部署拒绝写入）、`health`；`Supervisor::detach`。桌面端 `voice.rs` 10 个 Tauri 命令 + 退出联动；`laser-host` 新增 `mcp_bridge_connected` / `set_voice_device_sessions`；前端 `/voice` 页面、第四个导航、`SettingsModal` 跳转、关闭弹窗“同时停止语音服务端”。**已验证**：`npm test` 80/80（用户电脑工作区）；YAML 按行编辑用独立程序 + PyYAML 跑 5 种文件形态通过；预览桩 Playwright 走查（切换确认、先测试后保存、外部运行无停止、崩溃、等待设备、全模块不可保存、窄屏无横向滚动、关闭弹窗参数）。**未验证**：Rust 部分（云端无 crates 无法编译；新增约 30 个 Rust 测试待本机 `cargo test --workspace`，首次需下载 `serde_yaml`）；Windows Job Object 解除；托盘对话框；真实 xiaozhi-server 联调（A-60..A-69 的 I/M 部分）。**P4-10 结论**：首版不把 Python 运行环境打进安装包——xiaozhi-server 依赖 Python 3.10 及 torch / funasr 等大体积包（数 GB，且随所选模块变化），安装包体积与杀毒误报风险都不可接受；保持“安装包 + 用户自备服务端目录”，由语音中心填写目录与 Python 路径；卸载不删除外部目录。链路稳定后可再评估“可选的服务端便携包”单独下载。遗留：`frontend/voice.css.tmp`（空文件，本次误建，因无删除权限未能删除，请手动删除）。下一步：用户本机 P4-11 |
| 2026-10-07 | 内置语音方案（NV） | 完成（文档-only） | 用户要求把 xiaozhi-esp32-server、海门、xiaozhi-client 的功能做进 MOSS、取最优、本地与云端都支持。读三项目源码（`7a260ee` / `513d9e6` / `fe3c382`，均 MIT），写 [moss-native-voice-spec.md](moss-native-voice-spec.md)：功能对照与取舍、架构（Rust 内置语音服务 + sherpa-onnx 本地模型，无需 Python）、安全规则、NV0..NV11 计划、待确认事项 4 条；融合技术书升 r11。未写代码。下一步：用户确认 §9 后做 NV0 |
| 2026-10-07 | 内置语音方案 n2 | 完成（文档-only） | 用户确认 V1–V4：密钥存 SQLite `app_secrets`；范围加入第三方 MCP、记忆、视觉、天气与新闻等插件、Claude Code / Codex 调用（飞书 / 钉钉 / GitHub 不做）；端口 8000 / 8003；保留外部服务端链路。技术书升 n2：补 §5.4–§5.8 各新增范围的设计、§6 密钥与 Agent 安全规则、计划改为 NV0..NV14；融合技术书 r11 同步。未写代码。下一步：NV0 |
| 2026-10-07 | 内置语音方案 n3 | 完成（文档-only） | 用户追加范围：声纹、固件下发、音乐、Home Assistant、RAGFlow。技术书升 n3：§5.7 补音乐 / Home Assistant / RAGFlow（HA 控制默认需批准，门锁等始终需批准），新增 §5.9 声纹（本地 sherpa-onnx，只存特征向量）、§5.10 固件下发（默认不推送，逐型号确认并显示 SHA-256）；计划改为 NV0..NV18。未写代码。下一步：NV0 |
| 2026-10-07 | P4-11 | 完成 | 用户确认 P4 全部测试与三端构建通过（`cargo test --workspace`、`npm test`、`npm run build`、E2E、`cargo tauri build`、Android）。P4-01..P4-08 由 partial 改为完成。未覆盖：与真实自建 xiaozhi-server 的联调（外部链路保留作兼容，主线转 NV） |
| 2026-10-07 | NV0（第一部分） | 文档 + 工具，partial | 读源码核实依赖：sherpa-onnx 1.13.8（Apache-2.0，构建时从 GitHub 下载静态库，**含 GPL-3.0 的 espeak-ng**）、opus2 0.4（bundled 需 CMake）、univoice Rust 版（MIT，依赖重且版本冲突，改为移植豆包协议）、rmcp 3.5.1（Apache-2.0，stdio + streamable HTTP，无 SSE 客户端）。新增 `THIRD_PARTY_NOTICES.md`、`tools/nv0-probe`（独立工作区的测量程序 + `run.ps1`；云端无法访问 crates.io，未编译）。提出 V5：本地模型放独立 GPL 辅助进程，MOSS 保持 MIT。另修正本清单中重复出现的 LINK 与旧版 NV 章节。下一步：用户运行 `tools/nv0-probe/run.ps1` 并确认 V5 |
| 2026-10-07 | NV0（试编 1） | 失败 → 已调整 | 用户运行 `run.ps1`：电脑未装 CMake，`libopus_sys`（opus2 bundled）构建失败。读 opus2 源码（`e117ac7`）确认纯 Rust 后端 `backend-mousiki` 与 libopus 后端 API 相同；探针默认改用 mousiki，并增加解码音量比检查；装了 CMake 时额外编译 C 版对比。V5 用户确认：本地模型放独立 GPL 辅助程序 `moss-voice-local.exe`。下一步：用户重跑 `run.ps1` |
| 2026-10-07 | NV0 | 完成 | 用户本机重跑 `run.ps1` 全部通过（无 CMake）：编译 82 秒，探针 19.8 MB；Opus 16k/24k 往返正常（音量比 0.92/0.96）；Silero 实时率 0.003；SenseVoice 中英粤三段均正确，实时率约 0.04；Kokoro 首段 0.40–1.21 s，实时率约 0.55；说话人特征约 65 ms/段。结论写入技术书 §11.3（n5）。报告文件留在本机，已加入 `.gitignore`，不入库。下一步 NV1 |
| 2026-10-07 | NV1 | 代码完成，partial | 新 crate `moss-xiaozhi-proto`（消息、帧 v1/v2/v3、设备 token、OTA）与 `moss-voice-server`（专用 tokio 运行时 + axum：OTA 8003、设备 WebSocket 8000、设备允许流程、回声模式、每设备一个会话）；schema v6 `voice_devices`；桌面端 `voice_builtin.rs` 5 个 Tauri 命令（启动 / 停止 / 状态 / 允许撤销 / 改名），退出时随急停后停止；语音中心新增“内置语音服务”卡片与两个端口设置；`npm test` 81/81（用户电脑工作区）。Rust 部分云端无法编译（无 crates.io），新增约 21 项 Rust 测试待本机运行。界面预览（演示数据）：未运行 / 运行中、三种设备状态、允许确认框、1440 与 820 宽度均正常，无页面错误、无横向滚动。未验收：真实设备 |
| 2026-10-07 | NV1 本机测试 | 通过（设备 unverified） | 本机 `cargo test --workspace` 全部通过（`moss-xiaozhi-proto` 13、`moss-voice-server` 7、`laser-runtime` 34 等）、`npm test` 81/81。真实设备验收并入 HW-08 |
| 2026-10-07 | NV2 | 代码完成，partial | `moss-voice-server/src/audio/`：Opus 编解码（mousiki，`Audio` 模式——`Voip` 在 24 kHz 下回放音量仅约 0.7）、有状态线性重采样、`Vad` 接口 + 双阈值能量 VAD（静音 200 ms 判停）、`PlaybackPump`（启动缓冲、补静音、打断丢弃）；会话改为 空闲 → 拾音 → 播放，下行 24 kHz，回声经完整音频链路；修复“设备已收到 hello、会话尚未登记”的竞争。**计划调整**：Silero 随 NV6 辅助程序接入同一接口（技术书 n7）。验证：`cargo test --workspace` 全部通过（`moss-voice-server` 21 项，新增 14，连续 5 次稳定）；`npm test` 81/81；`npm run build`、`cargo tauri build`（`target/release/moss-desktop.exe` 与 `bundle/nsis/MOSS_0.1.0_x64-setup.exe`）、Android APK（`gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`）均于本机成功。E2E 未跑：未改 HTTP 行为、前端逻辑与发送链路（只改卡片说明文字）。已清理增量编译缓存与 `tools/nv0-probe/target`。未验收：真实设备（HW-08）。下一步：HW-08，然后 NV3 |
| 2026-10-07 | 实机测试安排 | 用户决定 | 所有需要实机的项目（§2a HW-01..HW-08、P1-13）留到最后统一测试，期间继续按计划写代码 |
| 2026-10-07 | NV3 | 代码完成，partial | 新 crate `moss-voice-providers`（OpenAI 兼容识别 / 流式大模型 + 工具调用 / 合成、切句、历史截断、WAV）；`moss-voice-server` 对话模式（每轮一个任务、逐句合成、句子文字与音频同步、工具 5 轮上限、唤醒问候、退出词、空闲再见、打断中止并保留部分历史、首句延迟事件）。验证：`cargo test --workspace` 通过（providers 20 项、voice-server 32 项）；首轮 `moss-voice` 既有测试因取空闲端口的竞争失败一次，与本改动无关，重跑通过并另立任务；`npm test`、`npm run build`、`cargo tauri build`、Android APK 本机成功。桌面端仍为回声模式，对话模式随 NV5 的模型配置启用。未验收：真实模型（无）与设备。下一步 NV4 |
| 2026-10-07 | NV4 | 代码完成，partial | `CallerContext::Voice` 与审批来源 `voice`；内置语音在执行层始终按严格模式（R3 只生成审批、R4 拒绝并审计 `voice:<设备>`，停止类不受影响）；`laser_host::VoiceBackend` / `voice_tools_list` / `voice_tool_call`；`moss-voice-server::HostTools`（审批结果原话朗读）与设备自带 MCP 中继（`device__*`）；审批卡来源标签。验证：`cargo test --workspace` 全部通过（laser-api +2、laser-host +1、voice-server 36）；`npm test` 81/81；E2E 32/32；前端、Windows、Android 构建本机成功。桌面端 `ToolHost` 接线与对话模式随 NV5。未验收：真实设备与模型。下一步 NV5 |
| 2026-10-07 | NV5 | 代码完成，partial | 语音链路第三项“MOSS 内置服务”（切入启动、失败回滚、离开停止，随 MOSS 自动启动）；内置模型设置存 `voice.builtin_models`、密钥存 `app_secrets`（`voice.{llm,asr,tts}.main`），三项配齐为对话模式否则回声；桌面端接上 MOSS 工具（`CallerContext::Voice`）；顶栏与 `/api/system/info` 报告内置服务设备会话；语音中心卡片新增模型配置。同时修复 NV4 引入的会话事件顺序竞争（`select!` 改 `biased`）。验证：`cargo test --workspace` 全部通过；`npm test` 83/83；E2E 32/32；前端、Windows、Android 构建本机成功；预览桩走查。未验收：真实模型与设备。下一步 NV6 |
| 2026-10-07 | 内置语音加工确认 | 变更完成，partial | 用户决定：内置语音发起的运动与出光跟随 `MOSS_FUSION_STRICT`（开启需桌面批准，关闭时口头确认后直接执行，与官方链路一致），取代 NV4 的“始终严格”；桌面控制类工具始终拒绝。验证：全量测试、E2E 32/32、三端构建本机成功。技术书 n11 §13.6。下一步 NV6 |
| 2026-10-07 | NV6 | 代码完成，partial | GPL 辅助程序 `moss-voice-local`（独立工作区）+ MIT 帧协议与客户端；Silero 接入 `Vad`（出错退回能量判停）；模型下载 / SHA-256 / 安全解压 / 镜像；本地开关与界面。验证：`cargo test --workspace` 全部通过；真实环境测试（真实下载、三模型往返、端到端首句延迟 1078 ms）本机通过；`npm test` 84/84、E2E 32/32；前端、Windows、Android、辅助程序构建成功。安装包尚未包含辅助程序（NV18）。下一步 NV7 |
| 2026-10-08 | NV7 | 代码完成，partial | 流式识别 / 流式合成接口；豆包协议移植自 univoice（修正未知事件、会话失败、越界解析）；会话边说边识别（自动模式 VAD 确认后才建连接）、失败退回整段；合成分段播放、豆包时启动缓冲 300 ms；接口类型设置与界面；假密钥连真实地址得到 401，TLS 与鉴权头可用。验证：`cargo test --workspace` 全部通过、`npm test` 85/85；E2E 32/32，前端构建、Windows App（含安装包）、Android APK 均重建成功。未验收：真实豆包账号、首句延迟对比、真实设备。下一步 NV8 |
| 2026-10-08 | NV8 | 代码完成，partial | 多接入点（主 + 最多 4 个附加，自建模式下附加暂停）；30 秒心跳、90 秒无数据判定断线、稳定连接后退避归零；工具开关在列表与调用两处生效（停止类不可关闭），接入点与内置语音共用；调用统计；token 身份与有效期；语音中心“接入点与工具”卡片。验证：`cargo test --workspace` 全部通过、`npm test` 86/86、E2E 32/32；前端构建、Windows App（含安装包）、Android APK 均重建成功。未验收：真实接入点上的多连接、心跳与断网恢复。下一步 NV9 |
| 2026-10-08 | 本地模型可选 | 代码完成，partial | 用户要求加入 TEN-VAD、SenseVoice 2025、X-ASR 流式识别、Kokoro v1.1 int8、Matcha 中英，可自行选用并附优缺点。模型清单带类别 / 引擎 / 介绍，Matcha 声码器作附加文件下载校验；辅助程序协议 2（引擎、流式识别）；本地流式识别接入 NV7 的边说边识别；TEN-VAD 最短语音 100 ms；自动拾音保留开头 300 ms → 1 秒（实测原值会丢开头的字）。验证：`cargo test --workspace` 全部通过、`npm test` 87/87、E2E 32/32；前端、辅助程序、Windows App（含安装包）、Android APK 均重建成功；真实模型本机测试通过（快组合首句延迟 253–315 ms，NV6 组合约 1.1 秒；Kokoro v1.1 int8 合成约 1 倍实时，偏慢）。详见技术书 §13.10。未验收：真实设备、真人语音 |
| 2026-10-08 | 语音体验优化计划 | 完成（文档-only） | 用户决定：NV8a（停止类指令本地快速通道、常用只读指令本地匹配、识别热词、本地合成边合成边播、各环节耗时记录）排在 NV9 之前；耗时与体验实机数据随 §2a HW-09 统一测试；语义判停、端到端语音模型、严格模式关闭时的本地口头确认列为可选项 VO-1..VO-3（未决定）。内置语音技术书 n16 §8.1 / §8.2、V6。未改代码，未运行测试与构建。下一步 NV8a |
| 2026-10-08 | 自然打断计划 | 完成（文档-only） | 用户确认板子为 ES8311 + ES7210、MIC3 硬件回采，固件支持设备端回声消除；新增 NV8b（固件核实、插话检测、历史只记已播出句子、设置与事件），排在 NV8a 之后；实机 HW-10。技术书 n17 §8.3（含现有打断机制说明）、V7。未改代码，未运行测试与构建。下一步 NV8a |
| 2026-10-10 | NV8a / NV8b | 代码与自动验证完成，实机 unverified | 固定停止/只读快捷通道、流式中间识别每轮停止一次、热词、协议 3 分段合成、最近 20 轮耗时；自然插话默认关闭、400 ms（300–600）、realtime + 设备 AEC 前提、1 秒句首保留、开播与问候/告别保护、只提交已开始播放的历史。修复实时急停写失败被误报成功。验证：Rust 451 项、前端 90/90、回环 E2E 32/32；前端、Windows release/NSIS、Android release APK、辅助程序 release 均重建成功，辅助程序 --version 为 protocol 3。产物位置与边界见 CHANGELOG；升级须同步更新辅助程序，NV18 安装包集成仍未做；HW-09 / HW-10 留后统一实机验收，本次没有实机性能数字 |
| 2026-10-10 | NV9 客户端基础库 | 进行中，partial | 新增 moss-mcp-client：stdio / Streamable HTTP、有界发现、别名、逐工具 Ask/Allow/Disabled、凭据引用、连接/心跳 API 与内存统计。9 项假服务测试通过，默认审批不执行、伪造摘要拒绝、HTTP 错误不重放、显式重连不补发。未接桌面配置、持久审批或语音路由。下一步按顺序完成：桌面配置与 app_secrets 引用 → 连接维护及全局预算 → external_tool 单次持久审批/审计 → 语音聚合与统计 UI → 集成负面测试及发布构建。不得把基础库测试视为 NV9 验收通过 |
| 2026-10-10 | 六项安全修补 | 软件与三端构建完成，实机 unverified | 按用户顺序修复撤销会话、验证无任务停止、保留急停和未知状态监控、取消中间词提前停止、隔离本地判停、按可信来源隔离审批。Rust 462 项通过（6 项忽略未执行），前端 95/95，回环 E2E 32/32；前端、Windows release/NSIS、Android release APK 重新构建成功，实际产物与 SHA-256 见 CHANGELOG。旧 NV8a 中间结果立即停止方案由本条取代；原 HW 验收保持待测。原有两份名称疑似含密钥的日志仍需提交前另行检查，本次未读取或改动。 |
| 2026-10-10 | NV9 四段集成 | 代码／自动验证／发布构建完成；真实服务 unverified | 桌面主窗口配置及版本化凭据、独立连接维护、external_tool 持久单次审批／审计、内置语音聚合和统计页全部接入。Rust 478 项、前端 98/98、回环 E2E 32/32；真实 React 假桥 1280/960 像素操作通过；前端、Windows release/NSIS、Android release APK 和 release moss-host 构建成功，产物大小／时间／SHA-256 见 CHANGELOG。保留撤销和防误急停修补；未操作真实设备、服务或账号，未安装／提交／推送。提示词 latest/history 更新受混合换行及工具安全检查阻塞，未绕过；独立 NV9 契约已同步。辅助程序协议仍为 3，安装包集成仍待做。 |
| 2026-10-10 | NV10～NV18 实施细则与计划同步 | 完成（文档-only） | 新增实施细则 i1：9 项各含技术框架、修改位置、步骤、数据/接口、异常/安全、验证；TODO 拆为 45 子步骤。统一融合 r12 / 内置语音 n18，同步 NV8a/NV8b 软件与实机状态、严格模式、WebGL 背景、helper 协议与打包现状。保留本会话中间结果立即停机要求与 final-only 代码差项；未改代码。检查：5 文档、54 栏目、45 唯一任务编号、72 本地链接/锚点、SQL 示例及 revision 条件更新、历史日志保留、5 份浏览器渲染和无横向溢出通过。未运行功能测试、三端构建或真实设备/账号操作，因本次仅补文档；功能仍按任务状态验收。 |
| 2026-10-10 | 剩余技术说明补齐：r13 / s1 | 完成（文档-only） | 修正内置语音 R3 严格模式表格和 NV4 摘要；统一当前技术书版本与 NV 入口；P2b/P3/P5 补六项技术细则和对应 TODO，纠正 P4 对可选 P2b 的硬依赖；延续 NV18 helper 源码/许可收集与新 NSIS 安装验收未完成状态。未修改程序或现有设备控制行为；文档检查见本轮审查结果，功能测试/三端构建及实机测试未运行。 |
| 2026-10-10 | NV18 脚本 / NV8a-1 修补 / NV10-1 数据层 | partial，真实安装与硬件 unverified | 更新 sidecar 源码与初版许可资源、根 target 构建和发布清单脚本。专用 NSIS 在本轮生成了 22,641,672 字节的新文件，但执行任务状态 lost，release-manifest 未产出，不能算安装包验收通过；helper 与暂存输入哈希一致。NV8a-1 现在中间整句“停止”直接一次 `FastStop`，接受续句误停风险。NV10 数据库 v7 与版本条件更新接口/两项回归完成。Rust 工作区 481/481、前端 102/102、回环 E2E 32/32、前端构建通过，三端 release 另查最新构建日志。无真实设备操作、安装、提交或推送。 |
| 2026-10-10 | NV10-2～4 记忆功能软件接线与三端构建 | 软件通过，最终 GUI/模型/小智设备 unverified | 完成记忆默认关闭、按设备摘要注入、断线后有界异步 LLM 总结、背景 generation/设备撤销 epoch/SQLite revision 防旧结果覆盖、桌面管理列表/开关/编辑/清空与仅本地主窗口 Tauri 权限。Rust 工作区 485/485、前端单测与构建、FakeGrbl E2E 32/32、Windows release/默认 NSIS、Android release APK 全部通过；真实 GUI 操作和小智/云模型摘要质量统一留 FINAL。**默认 NSIS 并非 NV18 helper 专用 bundle，不能替代辅助程序安装验收。** 产物尺寸/hash 见 CHANGELOG；未操作设备、安装、提交或推送。 |
| 2026-10-11 | NV15-1/2 实体别名冲突安全修补 | 软件专项测试通过；家居实际控制仍禁用，阶段 partial | 消除 `light.desk` 别名遮蔽 `switch.fan` 的歧义，并禁用伪装成未登记设备 ID 的别名，拒绝大小写重复/空白别名；`laser-host voice_home` 22/22、`moss-voice nv15_` 3/3、前端 102/102 与 Vite build 通过。全量 Rust/E2E/三端 release 依本轮最终验证结果另行报告；真实 Home Assistant 与最终用户安装继续留 FINAL。 |
| 2026-10-11 | NV15 最终软件回归与最新常规跨端构建 | 软件通过，生产 HA 禁用，阶段仍 partial | 最终补强测试 `laser-host voice_home` 22/22、`moss-voice nv15_` 3/3、Rust workspace 615/615、前端 102/102、FakeGrbl 32/32、Vite build PASS。Windows EXE 57,097,728B / `3b9b4cc67796be8001c4e382817872040d7032ba92a6e230286c635bbdb6d039`，常规 NSIS 18,092,999B / `997fc067a37bb1781408da5070aefad6b69bb171cabf8b5d15e1aa74b60c0dec`，Android APK 51,840,922B / `f81b32d852167799011ff40338d5f5fca634a8dc0e4d739efd330480620ab14c`；此普通 NSIS 不含 NV18 GPL helper，NV18-3 静态原生库许可审计仍为阻塞；无任何正式家居/激光动作或实机安装，不得标记发布就绪。 |
| 阶段A（历史） | NV15/16/17软件与NV18源码许可资料 | 当时部分软件通过，整体partial | NV15门禁统一及围栏顺序；NV16协议4/公开真实模型+合成推理/独立进程/文件锁/私有三段登记/事务删除；NV17只读记录/中断恢复；原生13库与Rust146源码/初次277许可、审核pending。Rust686项/7 ignored、前端109、FakeGrbl32及React登记通过。阶段A当时尚缺协调器/按人接线和r-efi正文；下述阶段B已更新这些软件证据，不能把本历史行当当前剩余清单。 |
| 本轮阶段A | 跨端和独立封包证据 | 构建通过；发行与真实验收未通过 | 新Windows EXE58,232,320B、普通NSIS18,544,601B、Android52,596,158B及内嵌Rust SHA对应；专用NSIS366,098,342B/协议4/schema9、清单 `target/nv18-6d11b13bef0a47a2b89e23b2fd3c9ae7/release-manifest.json`。官方7-Zip只读解包核验748条路径和helper完整20,828,160B/hash相同，未安装。实际hash与UTC见CHANGELOG。阶段A后新增协调器cfg(test)fake接线7/7专项通过，生产门禁仍false；后续增量须再次全量验证及交付构建，不把阶段A当最终状态。 |
| 2026-10-12阶段B | NV15协调器假服务、NV16当前句/按人隔离及NV18许可补正 | 软件实现/回归/跨端构建通过；真实识别/服务/发行仍partial | HomeControl协调器7项，按人相关10项、临时SQLite/旧配置/普通窗口绕过门禁；workspace698 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS及实际React登记/摘要CAS/窄屏通过。按人快照绑定人revision/目录/设备/会话/授权代次，未知不回退，删除/撤销/取消前首轮清理与停止不等待；生产Calibration=None。r-efi从锁定源码提取AUTHORS原文，146归档/279文本，实际文件hash均核对，严格发行仍拒绝原生对应审核/ORT传递闭环2类。技术书r13/n19/i2、NV8a中间结果停止现状、UI-03字体和PKG构建状态已同步，历史不覆写。 |
| 2026-10-12阶段B最终 | 前端/Windows/Android/专用内部包及独立归档核对 | 构建exit0；未安装/发布/操作实机 | Windows EXE58,601,984B（09:57 UTC）SHA `fefd563d1c4900f8f2d6ce5dccaf95d06a950b1c043e6daf2b8fdf2de289805e`；普通NSIS18,603,192B SHA `a3b9142b2705670eafe8c0b00e2f6b855ad1876cc91adc7cfe6066fcb8743300`；Android52,601,878B（09:59）SHA `04e726e322a70149330e209f997ec661bd36e0e0d95fc28f098bf2ae23d8f336`，包内/本轮Rust `.so` SHA相同`3b11d0e37a075eea031afba9defd799ba51d08fda60235dcc2e0281219240a20`。专用NSIS366,144,877B（10:05）SHA `044730bf3f37cec94539176ef2059c9fe824fe1db253657925b100a59cfb8f3d`；清单`target/nv18-a11f4d32a8d040e380d5c90495fab4f0/release-manifest.json`协议4/schema9，独立扫描749条路径，helper/证据清单/r-efi字节一致，desktop仅允许唯一Tauri标记3字节差异。helper源码未变，构建检查复用同hash产物、本轮实际合成推理44ms/192维及负测通过，不声称重生成旧字节；全部完整hash和边界见CHANGELOG。文档结构及渲染通过，无既有缓存清理。 |
| 2026-10-12 | NV19 新增规划与文档交叉引用 | 完成（文档-only）；NV19-01～08 均未开发 | 更新技术书 n20、实施细则 i3、唯一 TODO/文档入口，关联已有 NV10/NV11/NV13/NV16 并参考 OpenViking 官方定义；未更改程序/数据库、Goal、既有任务状态和历史日志，未运行功能测试/三端构建或真实设备、外部模型操作，开发与验收均待办。 |
| 2026-10-12续做最终 | NV13→18软件工具/全量回归/三端及专用内部包 | 自动检查与构建通过；真实门禁及外部分发未通过 | workspace698 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS（新release host），Node最新11 PASS（NV16 6/NV17 5）、opt-in实际Docker探针1 PASS、HA合成TLS33 PASS。前端/Windows/Android/专用内部NSIS均exit0；普通EXE58,601,984B、普通NSIS18,603,117B、APK52,601,878B、专用内部NSIS734,429,264B。独立只读核对875资源路径、38份ORT候选源码实际字节和helper完整hash，APK内嵌Rust与本次产物一致；helper源码未变，构建复用原字节并重跑实际合成诊断。完整路径/UTC/SHA/清单见CHANGELOG本次最终回执。Windows三个产物NotSigned；APK v2签名验证通过，发行身份与升级未验。NV13内层blocked，真实HA按用户决定暂缓，真人标定/可信开放、设备可信下载、原生对应/递归许可/安装升级未通过；7文档/32本地链接与渲染通过，无实机/安装/外部发布。 |
| 2026-10-12 NV11最终 | 供应商测试与适配补齐 | 本任务软件/回归/三端通过，NV11整体仍partial | 六类固定样本+真实本机TLS（无账号/外网），插件33 PASS、workspace710 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS及实际React假桥/窄屏通过；前端build/普通Windows EXE及NSIS/Android aarch64 release全部exit0。EXE58,604,544B SHA `32e0e739f9bd68d282dab354754b8fbc66c19a993b34a7dc18fbedac80dfbf1e`；普通NSIS18,608,125B SHA `d7a468f5bde56fbc8f8c4e3f714fbe03711cbe9d24ac62587e428830cb2967e0`；APK52,601,878B SHA `c3ba05878048983b2203c917352c8281c7469e605cb9f339235e18a5d310245f`，包内/本轮Rust `.so` SHA一致。完整UTC/hash见CHANGELOG本次回执。NV11-1/3软件勾选，NV11-4跨存储故障原子性未验；真实账号/限额/数据质量/设备留FINAL。5文档/46本地链接/渲染通过。NV18 helper/资料未改、专用包未重建，旧包不代表本次代码；未安装/实机/发布或清理既有缓存。 |
| 2026-10-13 | NV19 按 OpenViking 官方文档修正 | 完成（文档-only，用户确认）；NV19-01～08 仍未开发 | 技术书 n21、实施细则 i4：检索改为一次过滤搜索（L0 粗筛、L1 重排）；中文检索定为 FTS5 trigram（内置 SQLite 3.46.0 已开启 FTS5），两字词退回 LIKE；新增提取变更清单与整批撤销、闲聊/急停跳过检索、快速与深度检索、四节式会话摘要；设备/材料/校准以现有表为准；补充 OpenViking 服务门槛与 AGPL 分发边界。未改代码，未运行测试与构建。 |

## 10. 最终统一真实环境验收（其余软件开发完成后集中执行）

**用户决定（2026-10-10）**：先完成 NV10～NV17、NV18 其余软件实现及 Rust/前端、假设备/假模型、E2E、Windows/Android release 和 NSIS 自动检查；真实小智设备、真人语音、激光机、需用户操作的 Windows VM 安装/升级/卸载全部移到最后统一测试。提前未测的仍标 `partial/unverified`，不当作 PASS；不阻塞可以独立完成的后续软件开发。实体设备和有风险的系统操作须用户在场分别授权，禁止用模拟器通过替代实机证据。

- [ ] **FINAL-01 干净 Windows 安装验收**：用户参与，在隔离 Windows VM 安装含 helper 专用 NSIS，确认启动、握手、helper 缺失和旧协议报错；验证安装路径、进程退出、升级/回退、SQLite 数据、卸载不误删用户数据；记录实际安装包 SHA-256、版本、日志。**专用包自动构建、解包/hash、第三方源代码/许可证完整核验仍应在开发阶段完成**，不拖到人工验收。
- [ ] **FINAL-02 真实小智收音与播报**：用户参与，真实板子确认固件、麦克风收音、中文识别、扬声器播放、网络断开和恢复；记录设备版本、首包/完整耗时。此项不接激光机。
- [ ] **FINAL-03 自然打断/语音停止体验**：用户参与，确认板端 AEC 已启用后测自然插话、取消旧播报、噪声干扰、同轮停止只执行一次及“停止之后怎么办”前缀误停边界；关联 HW-09、HW-10 与 NV8a-1。
- [ ] **FINAL-04 激光机安全及急停**：用户参与，先建立物理隔离和可靠物理急停，确认安全条件及单独授权后再测真正设备；核对 `0x18` 高优先级急停、无任务/状态未知急停入口、权限撤销与断线；运动或出光须单独明确同意，不得由自动测试触发。
- [ ] **FINAL-05 统一结项**：记录每个真实场景的时间、设备/安装版本、操作人、PASS/FAIL 及失败回归；仅在软件和交付自动检查、第三方许可、隔离安装、真实设备与安全验收都通过后才勾选对应 NV18/HW 完成，未验项保留 `partial/unverified`。
