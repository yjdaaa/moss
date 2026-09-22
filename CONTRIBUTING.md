# MOSS 项目贡献指南 (Rust 重构版)

## 开发环境准备

- **Rust**：1.98+ (stable-x86_64-pc-windows-msvc)
- **Node.js**：v20+
- **Android SDK & NDK**：NDK 30+，JDK 17

---

## 验证规范

任何改动提交前必须运行全量回归验证，严禁虚报通过：

```powershell
# 1. Rust 工作区全量测试（包含 laser-contracts 类型边界守卫测试）
cargo test --workspace

# 2. 前端单元测试与生产打包构建
cd frontend
npm ci
npm test
npm run build
cd ..

# 3. 端到端闭环测试（拉起回环 GRBL 模拟器测试）
cd frontend
npm run test:e2e
cd ..
```

---

## 安全守则

- 涉及物理激光控制的改动，必须经过 `laser_device::authorize_send` 门禁与 SHA-256 物理重算比对。
- 新增 MCP 工具必须同时登记至 `laser_tools::ToolCatalog` 与 `laser_mcp::dispatch_tool_call`。
- 未连接硬件环境或未实机测试出光的链路，严禁宣称假 PASS。
