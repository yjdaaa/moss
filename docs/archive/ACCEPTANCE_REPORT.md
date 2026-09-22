> **历史归档（只读）**：本文是 Python 版到 Rust 版重构期间的文档，内容截止于文中日期，不再维护。其中 E2E 通过数已被 `CHANGELOG.md` 的 E2E 清理条目修正；§4 清理清单已执行。 当前状态以代码、`CHANGELOG.md` 与 [`docs/developer/`](../developer/) 为准；归档说明见 [README](README.md)。

# MOSS Xiaozhi MCP Rust 重构全量验收报告

日期：2026-09-17（2026-09-19 复核修订）  
基线规范：[rust-rebuild-spec.md](rust-rebuild-spec.md) §20  
执行环境：Windows 11 MSVC / Rust 1.98.1 / Node 24.20.0  
项目状态：核心功能闭环，自动化回归通过；Python 遗留已于 2026-09-19 清理；真机出光与真实小智端点联调仍未进行

---

## 1. 验收结论概要

依据《需求与实施基线》（rust-rebuild-spec.md）§20 验收标准，本次 Rust 重构已完成**从底层核心算子、数据库存储与迁移、硬件双信道与安全急停、20 个 MCP 工具、17 个 Web API 路由、到现代 React 18 前端工程与 Tauri 桌面端宿主的全栈闭环**。

- **Rust Workspace 测试**（2026-09-19）：10 个 crate/app，**121 项单元与集成测试 100% 通过**（含 `crates/laser-contracts/tests/type_boundary.rs` 类型边界守卫 3 项）；
- **前端工程测试与构建**：**40 项 Node 测试 100% 通过**，`npm run build` 成功输出 `frontend/dist/`；
- **全量可交互控件 E2E 业务闭环测试**：**41 项 100% 通过**（材料实验室 BTN-19 现为真实逐行下发到 completed），详见 [APP_BUTTONS_TEST_REPORT.md](APP_BUTTONS_TEST_REPORT.md)；
- **历史说明**：2026-09-17 版本引用的 Python 黄金样本差分（`migration/*.py`、624 项 Python 测试）已随 Python 遗留一并移除；`tests/golden/` 样本保留，`tools/list` 一致性改由 `laser-mcp` 单测与 `ToolCatalog` 逐名比对断言。

---

## 2. 逐阶段验收明细表

| 阶段 | 模块 | 验收状态 | 核心证据与结论 |
|---|---|---|---|
| **阶段 0** | 环境与骨架 | **完成** (移动端 partial) | Rust 1.98.1 MSVC；Tauri CLI 2.11；8 crate + 2 apps 骨架全绿；类型边界 CI 门禁建立。 |
| **阶段 1** | 黄金样本与基线 | **完成** (真实 Token 离线隔离) | G-code、安全拒绝、材料库、mcp_tools.json、17 API 快照建立；Python 624 用例全绿。 |
| **阶段 2** | laser-core | **完成** | pycompat 浮点 Banker's 舍入/str 全形态对齐；INTER_AREA 降采样；光栅 G-code writer 逐行等价；自适应平坦度贝塞尔细分；Floyd-Steinberg 抖动 0.058s（原 Python 6.619s）。 |
| **阶段 3** | laser-runtime 与迁移 | **完成** | SQLite WAL 单写连接；D6 启动未完成任务强制清零；材料三表拆分、A1 预设播种与墓碑清除、keep/overwrite 冲突归档；两步路径重映射与 stale 标记；dry-run 报告与二次导入幂等验证。 |
| **阶段 4** | laser-device 与 JobWorker | **完成** | 顺序流与单字节抢占式双信道隔离；$32=1 固件门禁；三门禁动态重算 SHA；10,000 行积压 100ms 急停到达；失联 500ms 保护；脱离 Tokio 裸急停句柄。 |
| **阶段 5** | laser-tools 与 laser-mcp | **完成**（2026-09-19 补齐） | 20 个工具全部真实分发：探测/安全动作真开 TCP 或串口，workflow/标定/材料库/文字任务经 host 后端；小智 WebSocket 桥接（tungstenite + rustls）落地并有回环往返测试；command_execution_tool 默认禁用；真实小智端点尚未联调。 |
| **阶段 6** | laser-api | **完成** | 17 个路由全量接线；D4 8777 实验服务功能并入；D11 报错文案排除项确认；SPA 单页回落；SSE 事件流；非回环 401 配对与 Bearer 鉴权；18 个黄金快照测试全通。 |
| **阶段 7** | frontend（前端重构） | **完成** | React 18 + Vite 6；主页、材料实验室（统一暗色）、画板三合一 SPA；D9 多厚度批量原子保存；D10 三套状态词表归一化；36 项 Node 测试与生产构建全通。 |
| **阶段 8** | Tauri 桌面 | **完成** | 进程内托管 laser-host；窗口关闭、托盘退出、Ctrl+C、控制台事件、Panic hook 全部接入 0x18 激光急停安全兜底；测试验证任务安全清零。 |
| **阶段 9** | 树莓派 CI | **partial** | 2026-09-19 复核：`ci.yml` 尚无 `aarch64-unknown-linux-gnu` job，此前记录有误；现有 job 为前端、Windows 测试+构建、Android、iOS。 |
| **阶段 10** | Android / iOS 移动端 | **完成** (本地 Android + 云端 iOS CI) | D7 拍板由 Tauri 移动端统一替代旧 Android 原生，D12 确认 iOS 采用 GitHub Actions macos-14 云端自动编译；Android APK 已在本地通过 CLI 自动化构建输出至 `target/app-arm64-debug.apk`；iOS 接入 GitHub Actions macos-14 自动化矩阵生成未签名应用包。 |
| **阶段 11** | 全量验收与端到端闭环 | **完成** | 121 项 Rust 测试、40 项前端测试、41 项 E2E 全绿（2026-09-19）；开展全系统交互控件深层闭环审查，彻底修复画板物理刀路缺失、confirm_send 快照断层、材料生命周期管理、网络异常处理等 10 项隐蔽缺陷；40 项端到端业务闭环测试 100% 通过（详见 [APP_BUTTONS_TEST_REPORT.md](APP_BUTTONS_TEST_REPORT.md)）。 |

---

## 3. 诚实边界说明

1. **未进行实机物理出光验收**：
   - 根据《重构规格》§23 执行授权边界与激光物理安全红线，在无物理光屏蔽与防护箱的环境下，**严禁向真实硬件发射激光或执行物理走位**；
   - 硬件通信与流控均由 `FakeGrbl`、本地 TCP 回环设备及 10,000 行高压模拟完成验证。
2. **移动端跨平台构建边界**：
   - Android APK 安装包（`app-arm64-debug.apk`）已在本地通过全套命令行工具编译生成；iOS 应用针对本地 Windows 环境无 Xcode 的限制，采用 GitHub Actions 官方托管的 `macos-14` 云端 Mac 运行器进行自动化矩阵编译，输出未签名应用包，供用户在 Windows 下使用爱思助手、AltStore 或 Sideloadly 免越狱安装。
3. **激光急停软件覆盖边界**：
   - 软件 5 路径安全兜底覆盖常规退出、窗口关闭、信号中断、控制台关闭与 Panic 崩溃；
   - 若进程被任务管理器强制杀进程（`taskkill -F`）或宿主机直接掉电，软件进程瞬间消亡无法执行代码，此时激光物理安全完全依赖 GRBL 固件本身的看门狗机制与控制器物理急停开关。

---

## 4. 阶段 12 清理旧内容清单建议（待用户最终授权）

待用户确认全量验收通过后，可授权安全清理以下旧内容（严格保护 `.git`、新数据与仍在使用的视觉资源）：

1. **旧 Python 源码与环境**：
   - `core/` 下所有旧 Python 实现文件（已由 `crates/` 对应 crate 完全取代）；
   - `tools/` 下所有旧 Python 工具脚本（已由 `crates/laser-tools/` 完全取代）；
   - `moss_mcp/` 下旧 Python Web 服务与桥接（已由 `laser-api` 与 `laser-mcp` 完全取代）；
   - `apps/*.py`（如 `server.py`、`laser_web_server.py`）；
   - `apps/excalidraw_lab/` 旧实验工程（已由 `frontend/` 完全取代）；
   - `apps/laser_web/ui/index.html`（已由 React 18 前端完全取代）；
   - `venv/` 虚拟环境。
2. **已归档历史任务**：
   - `.trellis/tasks/07-10-android-native-app-foundation`（D7 拍板废除的旧 Android 原生工程）。
3. **禁止清理的保留项**：
   - `.git/` 完整版本历史与已暂存改动；
   - `tests/golden/` 与 `tests/visual-baseline/` 黄金基线；
   - `.runtime/` 历史数据与 `moss.sqlite`；
   - 所有全新构建的 `crates/`、`apps/desktop/`、`apps/moss-host/` 与 `frontend/`。
