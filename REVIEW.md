# 代码审查准则（REVIEW.md）

本文件定义本仓库代码审查的红线、深审范围与输出契约。仓库总则见 [`AGENTS.md`](AGENTS.md)。

## 1. 输出与收敛契约

1. **首行先给结论**：第一行写明实质性问题总数；无实质缺陷直接输出 `无问题（no issues）`，不凑建议。
2. **精确定位与归类**：每条必须给出真实 `文件路径:行号`（严禁猜测），标注 `[Important]`（本次必须处理）或 `[Pre-existing]`（存量老问题），并点明**漏洞类别**，修复时连坐横扫同批同类文件。
3. **过滤噪音与收敛**：排版、`rules-of-hooks`、原生 `confirm`/`alert`、crate 分层、`Cargo.toml` lints/测试特性、`.ps1` ASCII 与本机路径/密钥扫描已由 `node scripts/verify.mjs --quick` 拦截，审查直接忽略风格偏好并聚焦状态机、并发、安全边界与契约同步；第 2 轮起只审增量补丁 diff，满 3 轮未清零立即停手交人工拍板。

## 2. 一票否决红线（触犯必标 `[Important]`）

- **激光与运动门禁**：生成/预览不得触碰硬件；完整发送与 `/api/device/framing` 严禁绕过 `require_r3_authority`、物理 G-code SHA-256 二次复算、行程复核（`job_exceeds_travel`）或 `evaluate_material`；急停 `0x18` 必须走实时通道不排队；发送或外控结果未知时严禁自动重发。
- **语音抢跑与急停**：新增任何由大模型触发、有副作用的执行路径必须排在 `TurnGate`（`Turn::wait_release`）之后（仅整句停止快路径除外）；停止类工具必须留在 `ALWAYS_ENABLED_TOOLS`。
- **权限、来源与隐私**：新外控/隐私能力默认关闭；Tauri 主窗口特权命令开头必须固定 `main` 窗口与精确回环来源，创建窗口命令必须为 `async fn`；CORS 与回环 `Host` 白名单不得放宽；声纹仅作提示，绝不授权任何动作；禁止写入真实设备地址、密钥、串口号或本机绝对路径。

## 3. 高危深审路径（`Deep` 档）

改动命中以下路径时，先对照 [`docs/developer/safety.md`](docs/developer/safety.md) 逐条核验状态机、并发与安全边界：

- `crates/laser-device/`、`crates/laser-api/`（发送门禁 / 审批 / CORS）、`crates/laser-host/`
- `crates/moss-voice-server/`、`crates/moss-mcp-client/`、`apps/desktop/src-tauri/`

## 4. 高频漏改与踩坑清单（细节见 [`docs/developer/standards.md`](docs/developer/standards.md)）

- **工具注册五件套**：增删改工具须同步 `ToolCatalog`、`dispatch_tool_call_with`、`tool_risk`、`tests/golden/mcp_tools.json` 与 `jev.rs` 粗筛归组（严禁单工具平铺 `noul`）。
- **类型与分层边界**：`laser-core`、`laser-device`、`laser-tools` 严禁依赖 `serde_json` 或直连数据库/设备。
- **契约与文档同步**：改路由、错误码、默认行为或小智提示词时，同批同步 `docs/` 与测试断言；弹窗页面入口确保挂载 `<AppDialogHost />`。

## 5. 忽略路径

跳过 `target/`、`frontend/dist/`、`node_modules/`、`*.lock`、`package-lock.json`、`apps/desktop/src-tauri/gen/`、`docs/developer/build-receipts.md`。
