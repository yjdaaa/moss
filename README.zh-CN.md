<p align="center">
  <img src="./assets/moss-banner.png" width="960" alt="MOSS 550W AI 激光雕刻机数控中枢">
</p>

<h1 align="center">MOSS 550W AI 激光数控雕刻中枢</h1>

<p align="center">纯 Rust 重构的工业级激光数控雕刻控制系统，融合小智 AI 空间智能与跨平台中控台。</p>

<p align="center">
  <a href="./README.md">English</a> | <a href="./README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <a href="./LICENSE"><img src="https://img.shields.io/badge/License-MIT-22C55E?style=flat-square" alt="协议: MIT"></a>
  <img src="https://img.shields.io/badge/Rust-1.98%2B-DEA584?style=flat-square&logo=rust" alt="Rust 1.98+">
  <img src="https://img.shields.io/badge/Tauri-2.0-24C8DB?style=flat-square&logo=tauri" alt="Tauri 2.0">
  <img src="https://img.shields.io/badge/硬件-FluidNC%20%2F%20GRBL-0284C7?style=flat-square" alt="FluidNC / GRBL">
  <img src="https://img.shields.io/badge/平台-Windows%20%7C%20Android-475569?style=flat-square" alt="Windows | Android">
</p>

## 核心亮点

| 亮点 | 对使用者的价值 |
|---|---|
| 纯 Rust 算法内核 | 完全剥离 Python 运行时依赖，实现微秒级运控规划，内存安全且无 GC 顿挫。 |
| FluidNC 与 GRBL 深度协同 | 握手自动感知机床 YAML 参数 ($CD)，动态拓展 1024B 流控滑窗，支持 SD 脱机加工杜绝断网报废。 |
| 工业级强安全门禁 | 物理 G-code 读盘重算 SHA-256 防篡改门禁，0x18 独立软件复位通道，以及后端材料安全门禁：PVC 自动拦截，成分不明塑料要求澄清。 |
| 跨端桌面与移动控制台 | 桌面端基于 Tauri 2.0 超轻量架构，Android 原生端在纯英文字符路径下支持一条命令全自动打包。 |
| 小智 AI 语音中枢 | 内置标准 Model Context Protocol (MCP JSON-RPC 2.0) 服务端，支持语音自然语言下发与参数微调。 |

## 架构

```text
网页 / Tauri 桌面及 Android ── HTTP 请求、状态轮询 ──► laser-api
Tauri 外壳 ── 初始化 ──► laser-host ── 组装 ──► laser-api / laser-mcp
                                 └── 初始化 ──► laser-runtime (SQLite)
小智控制台 ◄── WebSocket ──► laser-mcp ── 工具目录 ──► laser-tools
                                   └── HostBackend (laser-host 实现)
                                           └──► laser-api (保存记录/正式加工)
laser-api ──► laser-core (生成刀路) / laser-runtime (记录)
laser-api ── 安全确认、文件 SHA、设备预检 ──► laser-device
laser-tools ── 独立安全动作/探测 ───────────► laser-device
laser-device ── 串口 / Telnet ──► FluidNC / GRBL 控制器
```

`laser-core` 只计算刀路，不直接连接机器；需要保存记录或正式加工的 MCP 工具经宿主的 `HostBackend` 进入 `laser-api`。`0x18` 是软件复位请求，不能代替机器上的实体急停按钮。

## 使用示例

以下展示从生成文字矢量轮廓到触发物理发送门禁的闭环过程：

```bash
# 1. 预览文字矢量任务（只解算 G-code 并生成安全快照，绝对不点火出光）
curl -X POST http://127.0.0.1:9666/api/workflow \
  -H "Content-Type: application/json" \
  -d '{"action":"preview","source_type":"text","text":"MOSS","engraving_mode":"outline","material":"椴木","thickness_mm":3}'

# 预期响应包含：
# {"status":"prepared","gcode_sha256":"<文件物理哈希>","workflow_id":"wf-..."}

# 2. 执行确认发送门禁（若文件被外部篡改或未确认则强行拒绝）
curl -X POST http://127.0.0.1:9666/api/workflow \
  -H "Content-Type: application/json" \
  -d '{"action":"confirm_send","workflow_id":"wf-...","confirmed":true}'

# 只有在物理 SHA-256 读盘重算一致且设备处于 Idle 状态时，JobWorker 才会逐行向机床安全传输。
```

## 快速安装

确保系统具备 Rust 1.98+、Node.js 20+ 与 Android SDK/NDK（用于移动端打包）。

```powershell
# 克隆仓库
git clone https://github.com/farion1231/moss-rust.git
cd moss-rust

# 安装前端依赖
cd frontend
npm install
cd ..
```

## 快速开始

### 1. 启动桌面端界面 (GUI)

```powershell
cd apps/desktop/src-tauri
cargo tauri dev
```

### 2. 构建 Android Debug APK（标准单条命令）

```powershell
cd apps/desktop/src-tauri
cargo tauri android build --debug --target aarch64 --apk
# 输出安装包位于 gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

### 3. 运行独立命令行宿主

```powershell
cd apps/moss-host
cargo run -- serve
```

## 文档体系

完整的项目技术与操作文档统一归档于 [`docs/`](./docs/README.md)：

- [用户快速开始](./docs/user/quick-start.md)：硬件准备、运行目录、局域网配对与首次雕刻安全流程。
- [系统架构总览](./docs/developer/architecture.md)：Rust 进程模型、crate 分层拓扑、HTTP/MCP 桥与急停链路。
- [项目开发规范](./docs/developer/standards.md)：代码约定、MCP 工具注册流程与 PR 提交流程。
- [安全门禁入口](./docs/developer/safety.md)：物理 SHA-256 重算、FluidNC 功率自适应换算与完整发送预检。
- [Web API 契约](./docs/developer/api-contracts.md)：全部 HTTP 路由、SSE 事件与局域网 Bearer 鉴权规则。
- [数据库结构与迁移](./docs/developer/db-schema.md)：SQLite 第二版表结构、数据迁移流程与材料库格式。
- [激光硬件与 FluidNC](./docs/laser/README.md)：机型硬件基线、固件配置推荐与网络控制协议。
- [小智提示词归档](./docs/integration/xiaozhi-prompts/README.md)：控制台提示词规范与字符预算。

## 自动化验证

每次提交前必须运行全量回归测试，严禁写假测试：

```powershell
# 运行 Rust Workspace 全量测试（含架构类型边界守卫）
cargo test --workspace

# 运行前端纯逻辑单元测试
cd frontend && npm test && cd ..

# 运行全交互端到端 (E2E) 闭环测试（自动拉起回环 GRBL 仿真机）
cd frontend && npm run test:e2e && cd ..
```

## 开源协议

本项目采用 [MIT 许可证](./LICENSE) 开源。
