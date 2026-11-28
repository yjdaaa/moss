# 激光雕刻机 AI 自主测试（`scripts/laser-bench/`）

面向 MOSS 激光主链路的自主测试与结构化归档工具集，分为三级边界（融合技术书 `r74` / `r75`）：

| 级别 | 脚本 | 目标与安全边界 |
| --- | --- | --- |
| **L0 模拟机全自动** | `laser-sim-bench.mjs` | 完全隔离：临时运行目录 + 独立 `moss-host serve` + 回环 GRBL/FluidNC 模拟器（`127.0.0.1` 随机端口），不碰用户运行目录或真实设备 |
| **L1 真机只读核对** | `laser-readonly-check.mjs` | 连用户本机运行中 MOSS 回环接口；强白名单仅放行握手探测（`$$`/`$CD`/`$I`）与状态查询（`?`），忙碌立即阻断；请求带 `X-Moss-Bench: readonly` |
| **L2 真机空跑** | `laser-dryrun-bench.mjs` | 需用户先在 MOSS 桌面端连接机器并打开「允许 AI 真机空跑测试（不出光、限速）」开关（60 分钟自动关闭）；后端最底层强制 `S0` 与限速，请求带 `X-Moss-Bench: dryrun` |
| **真机出光** | — | **永远由用户手动执行**，不交给任何测试脚本或 AI |

## 1. L0 模拟机全自动（`laser-sim-bench.mjs`）

默认覆盖 `scenarios.json` 中的 **26 个场景**：生成（文字/图片/画板/材料矩阵/4 类向导图案）、流式发送（含 `$30=255` 等比换算）、运行中控制（走边框暂停/继续/停止、加工取消 `!→?→0x18`、背压下急停 `0x18` 实时直达、FluidNC `Hold:0|Ln` 断点续雕）、实时倍率（空闲拒绝 + 运行中命中原通道）、移动（走边框/点动/回原点 `G1+S0`/机械回零 `$22=1` 与 `$22=0` 拦截）、故障注入（`ALARM:1`、断连、门开 `Pn:D`、`ALARM:2` 软限位、慢 ACK + 不报行程/超速警告）、安全门禁（未确认、SHA 篡改、超行程、局域网只读/严格模式/跨源）以及**空跑模式强制改写**（`dryrun_mode_enforced`）。

```bash
# 运行全部默认场景（FluidNC 画像）
node scripts/laser-bench/laser-sim-bench.mjs

# 切换 GRBL 1.1 画像或自定义画像 JSON
node scripts/laser-bench/laser-sim-bench.mjs --profile grbl
node scripts/laser-bench/laser-sim-bench.mjs --profile path/to/profile.json

# 仅跑指定场景（可重复传或逗号分隔）并保留临时运行目录排障
node scripts/laser-bench/laser-sim-bench.mjs --scenario send_estop_realtime --scenario dryrun_mode_enforced --keep-runtime

# 指定归档输出目录（默认 <runtime-or-tmp>/laser-bench/sim-<run-id>/）
node scripts/laser-bench/laser-sim-bench.mjs --out-dir /path/to/out
```

### 自动检查的 12 条不变量（`invariants.mjs`）

1. `laser_off_before_move_or_framing`（安全）：移动和无光走边框前先关光（`M5`/`S0`）
2. `laser_off_after_terminal`（安全）：任务结束、取消、急停后激光关闭，`0x18` 后同连接不再发刀路
3. `g90_restored_after_relative_move`（安全）：`G91` 相对移动后恢复 `G90`
4. `spindle_s_within_dollar30`（安全）：所有 `S` 值不超过固件 `$30` 换算上限
5. `laser_on_within_firmware_travel`（安全）：固件报行程时出光段范围加起点不超行程
6. `estop_realtime_not_queued`（安全）：急停 `0x18` 经原任务实时通道低延迟直达、不被排队 G-code 阻塞
7. `gcode_sha256_matches_preview`（安全）：发送的物理 G-code SHA-256 与预览一致
8. `resume_start_line_and_no_duplicate_burn`（安全）：续雕从 `resumable_from_line + 1` 开始，预定位关光且不重复出光
9. `override_only_during_active_job_same_channel`（安全）：倍率字节仅在运行中任务期间发往原任务通道，空闲必拒
10. `homing_disabled_when_dollar22_zero`（安全）：固件 `$22=0` 时后端不得向控制器发送 `$H`
11. `time_estimate_deviation`（提示）：记录工时估算与模拟物理耗时偏差，偏差大时标为提示（不致非 0 退出）
12. `warnings_consistent_with_reality`（功能）：后端返回的 `warnings` 与固件/工况一致

## 2. L1 真机只读核对（`laser-readonly-check.mjs`）

前提：用户已在 MOSS App 连接好激光机且无运行中/待确认停机任务。

```bash
node scripts/laser-bench/laser-readonly-check.mjs
node scripts/laser-bench/laser-readonly-check.mjs --port 9666 --out-dir /path/to/out
```

所有请求均先过 `assertReadOnlyRequest` 白名单校验，并附带请求头 `X-Moss-Bench: readonly`（仅作 `ProcessTrace` 来源标签，不授予任何权限）。

## 3. L2 真机空跑测试（`laser-dryrun-bench.mjs`）

前提：
1. 用户已在 MOSS 桌面端连接好激光机，无运行中或待确认停机任务；
2. 用户已在 MOSS 桌面端「偏好设置 → AI 激光自主测试」打开 **「允许 AI 真机空跑测试（不出光、限速）」** 开关（可设限速上限 `300–5000 mm/min`，默认 `2000`，60 分钟自动关闭）。

```bash
node scripts/laser-bench/laser-dryrun-bench.mjs
node scripts/laser-bench/laser-dryrun-bench.mjs --scenario dryrun_framing,dryrun_text_small
```

- 后端最底层（`DryRunTransport` + 发送预检 + SD 上传路径）强制所有 `S` 改写为 `S0`、任务开头补 `G21 G94 G1 M5 S0 F<折算上限>`、`G0`（含 `G53 G0`）改写为 `G1 F<折算上限> S0`、裸 `G53` 与复位后裸坐标行补 `G1`、`F` 钳在折算上限（`dry_run_max_feed_mm_min / 2`，留足 200% 最大进给倍率余量）；急停 `0x18` 与实时倍率照常可用；行程与越界检查照常生效。
- 空跑期间，发送预检直接拒绝含 `G28` / `G30`（回预存位置，按快速速度移动；返回 `dry_run_rapid_home_blocked`）、`G93`（反时间进给）、`G20`（英制单位）或外部固件文件/宏执行（`$LocalFS/Run`、`$Macro`、`$RM`、`$N0=`/`$N1=`、外部 `$SD/Run`）的文件，传输层兜底遇到时替换为无运动注释 `; dry-run blocked <code>` 并记录带来源标签的告警；仅保存坐标不运动的 `G28.1` / `G30.1` 放行。
- **`$H` 机械回零说明**：`$H` 按固件自身配置的回零速度移动，空跑模式下保持放行（前提是固件 `$22=1`；FluidNC 固件内配置的 `macros.after_homing` 等本地宏受控制器自身配置约束）。
- 全部请求附带 `X-Moss-Bench: dryrun`，在完整过程记录中标为 `source: "ai_laser_bench_dryrun"` /「AI 激光测试 · 空跑」；空跑开启期间所有激光记录（含手动操作）另标 `dry_run: true` /「空跑模式」。
- 覆盖场景：走边框（含暂停/继续）、约 `20×10 mm` 原点附近小文字任务、取消与断点续雕、点动与回原点、机械回零（仅 `$22=1` 时）、实时倍率；自动核对 `FS:` 功率恒为 0、速度不超上限、坐标不超行程、结束后回到 `Idle`。

## 4. 归档结构与退出码

每次运行在仓库外的 `<runtime-or-tmp>/laser-bench/{sim,readonly,dryrun}-<run-id>/` 生成全脱敏归档：

- `steps.jsonl`：每步 HTTP 请求、响应与耗时
- `serial.log`（L0）/ `firmware.json`（L1）/ `status-reports.jsonl`（L2）：字节级串口日志、固件核对快照或状态报告序列
- `gcode/`（L0）：每个任务的 `.gcode` 副本与 `.sha256.json`
- `invariants.jsonl`：每条不变量/检查项结果
- `timeline.jsonl` / `timeline.txt`：时间窗内截取的 `ProcessTrace` 片段
- `report.md`：按严重程度排序（安全类最高）的总览报告
- `summary.json`：机器可读摘要

**退出码**：存在安全类违反返回 `2`；存在一般失败或预检阻断返回 `1`；全部通过（含仅提示）返回 `0`。
