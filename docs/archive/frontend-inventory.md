> **历史归档（只读）**：本文是 Python 版到 Rust 版重构期间的文档，内容截止于文中日期，不再维护。被盘点的旧前端源文件已删除；仍有效的状态映射与保留行为见 [前端行为契约](../developer/frontend-behavior.md)。 当前状态以代码、`CHANGELOG.md` 与 [`docs/developer/`](../developer/) 为准；归档说明见 [README](README.md)。

# 前端页面与状态清单

状态：已确认（2026-09-14；认证资源加载与 bootstrap 来源已修订）
日期：2026-09-14
上级文档：[rust-rebuild-spec.md](rust-rebuild-spec.md) §4.4、§15；接口见 [api-contracts.md](../developer/api-contracts.md)

现有前端有三个独立界面，外加一个只在开发时用的预览版：

| 界面 | 路径 | 来源 | 技术 | 行数 |
|---|---|---|---|---|
| A 主页 | `/` | `apps/laser_web/ui/index.html` | 单文件原生 JS，Excalidraw 经 `window.ForgeDraw` IIFE 引入 | 3892 |
| B 材料实验室 | `/material-lab` | `moss_mcp/web_server.py` L1675-2194 服务端模板，`__MATERIAL_LAB_DATA__` 注入初始数据 | 原生 JS | 约 1517 |
| C 独立画板 | `/draw` | `apps/excalidraw_lab/web/src/main.jsx` | React 18 + Excalidraw | 960 + 662 CSS |
| D 画板预览版 | 仅 `preview_server.py` 本地起 | `apps/excalidraw_lab/ui-preview/` | React | 不上线，不纳入重构范围，但其 `jobSummaryView`、`rasterInkElements` 与 C 同源 |

新前端把 A、B、C 合成一个 React 工程，路由保持 `/`、`/material-lab`、`/draw`。旧页面全部在本机回环场景使用，没有认证界面；新前端在非回环场景新增输入配对码状态（数据请求收到 401 进入，提交 `/v1/auth/pair`，见 rust-rebuild-spec.md §17），本清单其余状态不含此项。非回环请求统一经过鉴权 `fetch`：JSON 请求附加 Bearer；预览图和下载先取 Blob 再生成 Object URL；`/v1/events` 用带 Bearer 的流式 `fetch` 读取 SSE，不使用不能设置请求头的原生 `EventSource`。前端不假设仓库目录或 `InstallRoot` 是数据目录；程序资源由 `InstallRoot` 提供，运行时文件路径由后端 `RuntimeRoot` 管理，非回环响应不得暴露任一目录的绝对路径。

---

## 1. 主题与视觉资产（A）

| 项目 | 内容 | 行 |
|---|---|---|
| 配色变量 | `--bg-0 --bg-1 --bg-2 --panel --panel-strong --ink --ink-dim --ink-faint --laser --laser-2 --magenta --gold --green --danger --line --line-bright` | 28-43 |
| 字体变量 | `--font-mono`、`--font-display` | 44-45 |
| 缓动 | `--ease-out`、`--ease-snap` | 46-47 |
| 动画 | gridDrift、shine、ledPulse、beamFlicker、beamRise、charUp、riseIn、wheelDot、spin、floaty、scanY、fadeIn、modalPop、logIn、sendRingSpin、ringPulse | 92-1128 |
| Canvas | `#bg-canvas` 全屏背景粒子/网格（`resizeBg`、`drawBg` L1640-1739）；`#preview-canvas` 预览渲染（`drawPreviewFrame` L3095） | 1187、1465 |
| 装饰层 | `.bg-grid`、`.noise`、`#cursor-glow` 跟随光标 | 1188-1190 |
| 断点 | 1020px、640px、520px；`prefers-reduced-motion` 关闭动画 | 759、1036、1163、1180 |
| vendor | `vendor/lucide.min.js`；`vendor/excalidraw/forge-draw.bundle.js` + `index.css` + `fonts/`（28MB，237 文件） | 8-21 |
| Excalidraw 离线字体 | 新 React 工程从 `@excalidraw/excalidraw/dist/prod/fonts` 自托管到 `frontend/public/excalidraw/fonts/`，并设置 `window.EXCALIDRAW_ASSET_PATH`；断网不得访问 CDN | 重构阶段 7 |

材料实验室（B）另有一套变量：`--bg --panel --soft --ink --muted --line --accent --accent-weak --danger --warning --shadow`，断点 1180px、760px、`pointer: coarse`。重构后统一到 A 的暗色变量体系（已确认）。

## 2. 主页（A）区块与状态

### 2.1 顶栏
- 品牌、导航链接、连接状态胶囊 `#conn-pill`（class `offline` / `online`，LED 闪烁）。
- 状态：离线、在线。

### 2.2 Hero `#top`
- 大标题 LASER / FORGE 逐字入场动画、中文副标、两个按钮 `#cta-start`、`#cta-sim`、三个统计卡、滚动提示。
- 状态：初次加载动画、`reveal` 滚动入场。

### 2.3 工作台 `#workbench`（核心区，可拖拽缩放）
容器 `#console`，五个缩放把手，`#workbench-reset-size` 复位；尺寸存 localStorage（`saveWorkbenchSize`）。状态：`resizing`、`dragging`。

**连接栏 `#connection-bar`**
- `#connection-mode-select` 网络/串口；串口字段组 `#connection-serial-fields`（端口下拉 `#serial-port-select`、刷新 `#serial-refresh-btn`、波特率）；网络字段组 `#connection-network-fields`（host、telnet 端口）；提示 `#connection-priority-note`。
- 数据：`/api/ui/runtime-config`、`/api/ui/serial-ports`。
- 状态：网络模式 / 串口模式；串口列表加载中、为空、缺 pyserial 报错；连接检查中（`working`）、成功、失败、回退到另一模式（`fallback_used`）。

**输入源切换 `#tool-row`**，五个 chip：text / image / gcode / preset / draw，`applyToolUI` L2771 控制显示：

| tool | 显示区块 | 状态 |
|---|---|---|
| text | `#text-input` 24 字上限，`#char-counter`，光束装饰 | 空、输入中、超限（`invalid`） |
| image | `#image-drop` 拖放区 | 空、拖入中（`over`）、已选（`#image-file-name`）、上传中、上传失败 |
| gcode | `#gcode-drop` | 同上，限 `.gcode/.nc` |
| preset | 无额外区块，用材料推荐参数 | 默认选中 |
| draw | `#draw-pad`：材料/厚度/宽/高/模式/host 控件、导出 PNG、导出场景、`#draw-excalidraw` 挂载点、拖拽调高 `#draw-resize` | 未加载、加载失败（`excalidraw-load-error`）、已挂载、空画板、有元素 |

**参数区**
- `#material-select`、`#mode-select`（雕刻光栅/轮廓/切割，`modeApi` L2004 映射）。
- 三个滑块 `#power`、`#speed`、`#thickness`，各带数值框与单位；`has-custom-size` 状态。
- 推荐：`/api/ui/material-recommendation`，`scheduleRecommendation` 防抖，`updateRecommendation` L3182。
- 状态：推荐加载中、命中精确厚度、命中邻近厚度（`match`）、无推荐、手动改过参数（触发 `manual_params_confirmed` 语义）。

**动作行**
- `#btn-gen` 生成预览（`generateGcode` L3246，按 tool 分发到 `/api/text-task/generate`、`/api/workflow` preview、`/api/draw/preview`）。
- `#btn-send` 确认发送，默认 disabled，预览成功后启用（`refreshActionButtons` L3733）。
- 操作格 `#op-test-conn`、`#op-save-cfg`、`#op-status`、`#op-cancel`、`#op-regen`；反馈框 `#feedback-text` + `#btn-feedback`。
- 状态：生成中（`working`）、生成成功、材料被拒（`material_blocked`，红色提示）、材料需澄清（`material_clarify`）、生成失败；各 op 按钮忙碌态。

**预览面板**
- 切换 `#pv-nav-img` / `#pv-nav-gcode`，链接区 `#pv-nav-links`（`renderArtifactLinks` L3388：图片、G-code 下载、task.json、workflow.json）。
- `#preview-canvas` + 扫描线 `#scanline` 动画；空态 `#preview-empty`。
- 侧卡：`#meta-model`、`#meta-eta` 时间估算、`#preview-warn-card` 警告（hidden 切换）、进度条 `#progress-fill`。
- 状态：空、加载图片中、图片预览、G-code 预览、带警告、进度推进。

### 2.4 材料库 `#materials`
- `#mat-add-btn`、`#mat-refresh-btn`、卡片网格 `#material-grid`（`renderMaterialCards` L2443，按材料着色 `matColorFor`）。
- 数据：`/api/ui/material-options`；失败时降级到内置规则（L2072 注释）。Rust 版后端库内始终含内置预设（rust-rebuild-spec.md §22，2026-09-14），用户库文件缺失不再产生空列表或错误；前端降级只在接口本身失败（网络、401、500）时触发。
- 状态：加载中、空、正常、加载失败降级、选中卡片（`active`）。

### 2.5 推荐参数 `#params`
- `#reco-grid` 展示当前材料各厚度推荐。状态：空、有数据。

### 2.6 日志 `#log`
- `#log-body` 逐条入场动画 `logIn`，级别 info/warn/error/success；`#btn-clear-log`。

### 2.7 页脚
- 品牌与版本信息。

### 2.8 覆盖层与弹窗

**发送覆盖层 `#send-overlay`**（`setSendStage` L3459）
- 环形进度 `send-ring`（`indeterminate` 旋转 / 终态定格）、中心文字 `#send-core`、标题 `#send-title`、状态码 `#send-pct`、四步 `#st-0..3` 与连线 `#stl-0..2`、取消按钮 `#send-cancel`。
- 状态映射（必须原样保留）：

| workflow 状态 | 步骤 | 中心文字 | 标题 |
|---|---|---|---|
| pending / queued | 0 | 等待 / 排队 | 等待任务 |
| preparing | 1 | 准备 | 准备发送 |
| running / sending | 2 | 发送中 | 正在向设备发送 |
| completed | 3 全 done | 完成 | 发送完成 |
| failed | 2 标 failed | 失败 | 发送失败 |
| cancelled | 2 标 failed | 已取消 | 任务已取消 |

- 轮询 `/api/workflow` status（`startStatusPolling` L3577），连续失败计数后停止。
- 取消走 `/api/workflow` cancel（`cancelSend` L3662）。

**材料编辑弹窗 `#mat-modal`**（`openMaterialModal` L2551）
- 字段：名称、厚度列表（逗号分隔，`parseThicknessList`）、功率、速度；错误区 `#mat-modal-err`；删除按钮仅编辑态显示；保存文字随新建/编辑切换。
- 走 `/api/material-lab/manage` save / delete（delete 需 confirmed）。
- 状态：新建、编辑、保存中（`ModalSetBusy`）、校验错误、删除确认（`confirmDeleteMaterial` L2656 二次确认）。

**确认发送**（`confirmSend` L3606）
- 发送前先 `/api/connection/check`，再 `/api/workflow` confirm_send 带 `confirmed:true`。
- 确认请求只携带 `workflow_id`、`confirmed:true` 和 API 白名单允许的连接参数；不携带快照 SHA，由服务端从持久化 confirmation snapshot 重算并校验。
- 状态：连接检查失败中止、发送已提交、进入覆盖层。

## 3. 材料实验室（B）区块与状态

三栏布局：材料列表、参数编辑器、校准矩阵面板。

| 区块 | 元素 | 接口 | 状态 |
|---|---|---|---|
| 头部 | 来源路径 `#sourcePath`、搜索 `#materialSearch`、新建 `#newMaterial`、保存最近 `#saveRecent`、导出 `#exportLibrary`、导入 `#importLibrary` + `#importFile` | `/api/material-lab/export`、`manage(import)`、`recent` | 搜索过滤、导入中、导入成功刷新、导入失败、最近无参数。来源路径显示 bootstrap 的 `source_path`：回环为相对路径、非回环只有文件名（api-contracts.md §2.3），前端原样显示，不拼接目录 |
| 材料列表 `#materialList` | 按材料/厚度展开 | 初始数据来自 `GET /v1/material-lab/bootstrap` | 空、选中 |
| 编辑器 | 材料名、别名、厚度下拉 `#thicknessSelect`、来源、机型档案、置信度、证据四项（品牌、镜片、测试日期、功率 W）、模式页签 `#modeTabs`（engrave raster / engrave outline / cut）、七个参数框（min/max power、feed、travel、passes、pixel、threshold）、备注、按钮 保存 `#saveParams` / 存新版本 `#saveNewVersion` / 复制 `#copyParams` / 删除 `#deleteParams`、状态行 `#editorStatus` | `manage(save/delete)` | 未选材料、编辑中、保存成功、保存失败、删除二次确认 |
| 矩阵面板 | 模式摘要 `#matrixModeSummary`、功率/速度区间与行列数、格大小、次数、host、telnet 端口；生成 `#generateMatrix`、发送 `#sendMatrix`、存最佳 `#saveBest`；链接 `#matrixLinks`、坐标轴 `#matrixAxis`、矩阵格 `#matrixPreview`、最佳摘要 `#bestSummary`、状态 `#matrixStatus` | `preview`、`send`（需 confirmed）、`select-cell` | 未生成、生成中、已生成可点选、发送需填 host、发送中、已选最佳格、保存成功 |

## 4. 独立画板（C）区块与状态

`main.jsx` 单组件 `App`，状态变量 L78-92：

| 区块 | 内容 | 状态变量 |
|---|---|---|
| 顶栏 | 返回主页、下载场景 JSON、下载 PNG | `busy`、`excalidrawAPI` |
| 左栏控制面板 | 材料库下拉（`materialSelectState.js`，含"自定义"项与刷新）、尺寸网格（`laserExportDimensions.js`）、模式、host、保存配置、生成预览、确认发送、拆分动作、workflow 芯片 | `form`、`materialLibrary`、`materialMode`、`materialRefreshBusy`、`workflowId`、`canConfirmSend`、`hostReady` |
| 中栏画布 | Excalidraw，`rasterInkElements.js` 处理墨迹元素 | `excalidrawAPI`、`lastScene` |
| 右栏结果 | 预览图 `PreviewImage`、任务摘要 `JobSummaryCard`（尺寸提示、警告列表）、链接列表、诊断折叠 `details` | `result`、`status` |
| 弹窗 | `SendConfirmDialog`：设备信息、确认/取消 | `confirmOpen`、`connectionBusy`、`connectionResult` |
| 防重发 | `repeatSendGuard.js`，localStorage 记录上次发送指纹 | `repeatSend` |

接口：`/api/draw/preview`、`/api/workflow`（status/confirm_send/cancel）、`/api/connection/check`。
状态：画板空、生成中、预览完成、不可发送提示（两种文案 L677、L679）、连接检查中、确认弹窗、发送提交、重复发送拦截。

## 5. 视觉基线截图清单（阶段 1）

视口：桌面 1440×900、移动 390×844。每项两视口各一张，文件名 `<界面>-<编号>-<状态>-<视口>.png`，存 `tests/visual-baseline/`。截图前禁用 `#bg-canvas` 动画与 `#cursor-glow`（注入 `prefers-reduced-motion`），否则像素比对不稳定。

| 编号 | 界面 | 状态 |
|---|---|---|
| A-01 | 主页 | 首屏 hero，动画结束后 |
| A-02 | 主页 | 工作台，text 模式，空输入，网络模式 |
| A-03 | 主页 | 工作台，text 模式，串口模式展开 |
| A-04 | 主页 | 工作台，image 模式，拖放区空 |
| A-05 | 主页 | 工作台，gcode 模式 |
| A-06 | 主页 | 工作台，draw 模式，Excalidraw 已挂载 |
| A-07 | 主页 | 文字预览成功，图片视图，侧卡有 ETA |
| A-08 | 主页 | 同上切到 G-code 视图，带警告卡 |
| A-09 | 主页 | 材料被拒错误态（输入 PVC） |
| A-10 | 主页 | 发送覆盖层 sending |
| A-11 | 主页 | 发送覆盖层 completed |
| A-12 | 主页 | 发送覆盖层 failed |
| A-13 | 主页 | 材料库区块，正常 |
| A-14 | 主页 | 材料弹窗新建 |
| A-15 | 主页 | 材料弹窗编辑含删除 |
| A-16 | 主页 | 推荐参数区块 |
| A-17 | 主页 | 日志区块含四种级别 |
| B-01 | 材料实验室 | 初始，未选材料 |
| B-02 | 材料实验室 | 选中椴木 3mm engrave raster |
| B-03 | 材料实验室 | cut 页签 |
| B-04 | 材料实验室 | 矩阵已生成 |
| B-05 | 材料实验室 | 矩阵已选最佳格 |
| C-01 | 画板 | 初始空画板 |
| C-02 | 画板 | 有元素，预览完成，右栏有摘要 |
| C-03 | 画板 | 确认发送弹窗 |
| C-04 | 画板 | 不可发送提示 |

数据准备：截图用固定的材料库副本与固定工作流记录，放 `tests/visual-baseline/fixtures/`，保证可重复。

## 6. 重构时要保留的行为细节

1. 工作台尺寸、材料配置、画板配置存 localStorage，键名保持（`readWorkbenchSize`、`restoreSavedConfig`、`saveConfiguration`）。
2. 文字上限 24 字。
3. 状态轮询失败计数与停止逻辑。
4. `canceled` 归一为 `cancelled`。
5. 材料库接口失败时的降级规则（L2072）；触发条件收窄为接口失败，后端库为空或文件缺失不再是触发条件（§2.4）。
6. 画板重复发送指纹拦截。
7. Excalidraw 主题变量覆盖 `--color-primary: #35e0ff`（L687）与底栏隐藏（L690）。

## 7. 已确认决定

- 材料实验室（B）合并后统一为主页暗色风格。B-01 到 B-05 的基线截图只用于核对布局与信息完整性，不做像素比对；比对方式改为人工核对加元素存在性断言。
- 多厚度保存原子化（D9，2026-09-16 已确认）：旧主页材料弹窗在保存多厚度时按厚度列表循环调用 `manage(save)` N 次（`index.html:2609`、`2668`），重构后废除浏览器端 for 循环发 N 次 HTTP 请求的机制，改为单次调用 `/api/material-lab/manage` 批量提交当前材料下的所有厚度，由后端单事务保证原子保存，避免部分成功/部分失败的脏数据。


