# 贡献指南

## 环境

- Rust 1.98+（`stable-x86_64-pc-windows-msvc`）
- Node.js 20+
- Android：SDK、NDK 30+、JDK 17（仅构建 APK 时需要）

首次准备前端依赖：`cd frontend; npm ci`。

## 规则

- 安全边界、验证命令、结果状态、CHANGELOG 格式：[`AGENTS.md`](AGENTS.md)
- 目录职责、代码约定、PR 检查清单：[`docs/developer/standards.md`](docs/developer/standards.md)
- 改高风险代码前：[`docs/developer/safety.md`](docs/developer/safety.md)
- 安全问题报告：[`SECURITY.md`](SECURITY.md)
