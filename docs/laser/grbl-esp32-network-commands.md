# Grbl_ESP32 与 FluidNC 网络控制命令参考

本文档整理了基于 ESP32 的激光雕刻机控制器（包括旧版 `Grbl_ESP32` 与现代 `FluidNC`）的网络控制协议、命令集与上位机对接规范，用于指导 MOSS 系统与硬件控制器的安全通信。

不要把真实设备 IP、Wi-Fi 密码、WebUI 密码或 Token 写进代码和提交记录。示例统一使用 `<device-ip>`、`<http-port>`、`<telnet-port>`、`<path>` 等占位符。

---

## 0. 核心代际演进与架构差异（Grbl_ESP32 vs FluidNC）

目前市面上的 ESP32 激光控制器存在两代主流固件，开发与运维时必须严格区分：

| 维度 | 旧版 Grbl_ESP32 (v1.x) | 现代 FluidNC (v3.x+) | MOSS 上位机规范 |
|---|---|---|---|
| **配置机制** | 基于 NVS key-value 及编译时 `#define` 宏，修改繁琐 | 统一采用 `config.yaml` 声明硬件（引脚、步进电机、激光模式、speed_map、Wi-Fi/网络） | MOSS 严禁盲目自动改写用户固件配置；通过 `$$` 读取 `$32=1` 与 `$30` 标尺 |
| **网络 Telnet** | 默认端口 `23`，纯文本流 | 默认端口 `23`，纯文本流，支持全量 G-code、实时命令与 `$` 命令 | **MOSS 主链路通道**：TCP 模式默认连接 23 端口，走有界握手与字符计数流控 |
| **WebSocket** | 独立端口（通常为 HTTP 端口+1，如 `81`），仅用于 WebUI 状态同步，不直接执行文本命令 | 与 HTTP 共享同一端口（如 `80`，路径 `/`），`AsyncWebSocket` 直接投递进系统命令队列 | 现阶段上位机统一收拢在后端 Rust 接入，严禁前端浏览器绕过门禁直接直连 |
| **命令体系** | 大量依赖 `[ESPxxx]` 格式（如 `[ESP111]` 查 IP，`[ESP800]` 查固件） | 全面采用统一的 `$Setting` 层级体系（如 `$WiFi/SSID`、`$S`、`$CD`），同时对传统 GRBL `$$` 兼容 | MOSS 优先采用 GRBL 兼容的 `$$`、`?`、`G4 P0`，并支持 FluidNC 状态报告解析 |
| **激光功率标尺** | 早期常固定为 1000 | 默认可能为 `255` 或 `1000`（取决于 YAML 中的 `speed_map`） | **MOSS 自动自适应**：连接时读取 `$30`，若是 255 自动将 1000 标尺的 G-code 等比例重算（流式发送与 SD 脱机上传一致，含 `G1X10S500` 紧凑写法；2026-11-05 起） |
| **完成状态等待** | 容易发生 ACK 提早返回 | 规划器存在运动缓冲，发送完 G-code 后控制器仍在 `Run` 状态 | **MOSS wait_until_idle**：发送 `G4 P0` 同步，等待 `Run` 平稳过渡为 `Idle`，绝不误判 Alarm |

---

## 1. 网络入口

### HTTP WebUI 命令

基础格式：

```text
http://<device-ip>:<http-port>/command?commandText=<url-encoded-command>
```

默认 HTTP 端口通常是 `80`。如果端口是 `80`，浏览器地址里可以省略 `:<http-port>`。

PowerShell 编码示例：

```powershell
# FluidNC 查询设置
$cmd = [uri]::EscapeDataString('$S')
Invoke-RestMethod "http://<device-ip>/command?commandText=$cmd"

# 旧版 Grbl_ESP32 兼容命令
$cmd = [uri]::EscapeDataString('[ESP800]')
Invoke-RestMethod "http://<device-ip>/command?commandText=$cmd"
```

### Telnet / GRBL 通道（MOSS 推荐主信道）

默认 Telnet 端口通常是 `23`。

```powershell
Test-NetConnection <device-ip> -Port 23
telnet <device-ip> 23
```

Telnet 连接后可以发送 GRBL 查询命令、G-code、实时单字节命令（如 `?`, `!`, `~`, `0x18`）和系统配置命令。它相当于“网络虚拟串口”，所有输入直接进入控制器规划器。

---

## 2. FluidNC 现代网络配置命令（层级 $ 命令体系）

在现代 FluidNC 中，不建议使用已被废弃的 `[ESPxxx]` 命令，应采用标准层级命令：

| 命令 | 用途 | 说明 / 示例 |
|---|---|---|
| `$S` | 查看所有设置 | 打印当前所有的层级配置项与值 |
| `$SS` | 查看与默认值不同的设置 | 仅打印被修改过的配置项 |
| `$CD` | 打印当前硬件配置 (YAML) | 打印机器当前运行的 YAML 配置内容 |
| `$WiFi/Mode` | 读写 Wi-Fi 模式 | `STA`（连接路由器）、`AP`（热点模式）、`OFF` |
| `$WiFi/SSID=<ssid>` | 设置 Wi-Fi 名称 | 例如 `$WiFi/SSID=<ssid>` |
| `$WiFi/Password=<pwd>` | 设置 Wi-Fi 密码 | 例如 `$WiFi/Password=<pwd>`；不要把真实密码写进文档或日志 |
| `$WiFi/IPMode` | 设置 IP 模式 | `DHCP` 或 `Static` |
| `$Telnet/Enable` | Telnet 服务开关 | `ON` 或 `OFF` |
| `$Telnet/Port` | Telnet 服务端口 | 默认 `23` |
| `$Http/Enable` | HTTP WebUI 开关 | `ON` 或 `OFF` |
| `$Http/Port` | HTTP 服务端口 | 默认 `80` |
| `$Config/Filename` | 当前加载的配置文件名 | 例如 `/littlefs/config.yaml` |
| `$MD5=<path>` | 计算文件 MD5 校验和 | 用于校验上传的 G-code 或配置文件完整性 |

---

## 3. 旧版 Grbl_ESP32 HTTP 查询命令（向后兼容参考）

这些命令偏只读，适合作为自动化探测的第一批 allowlist。仍然要处理认证失败、设备忙、网络超时和固件差异。

| 命令 | 名称 | 用途 | 风险 |
|---|---|---|---|
| `[ESP0]` 或 `[ESP]` | `WebUI/Help` | 查看 WebUI 命令帮助 | 低 |
| `[ESP111]` | `System/IP` | 当前系统 IP | 低 |
| `[ESP200]` | `SD/Status` | SD 卡状态 | 低 |
| `[ESP210]` | `SD/List` | SD 卡文件列表/容量 | 低 |
| `[ESP221]<path>` | `SD/Show` | 查看 SD 文件内容 | 中，可能暴露文件内容 |
| `[ESP400]` | `WebUI/List` | WebUI 设置列表 | 中，可能返回敏感配置的占位或隐藏值 |
| `[ESP410]` | `WiFi/ListAPs` | 扫描附近 AP | 中，泄露周边网络信息 |
| `[ESP420]` | `System/Stats` | 系统统计 | 低 |
| `[ESP720]` | `LocalFS/Size` | SPIFFS/LocalFS 容量 | 低 |
| `[ESP701]<path>` | `LocalFS/Show` | 查看 LocalFS 文件 | 中，可能暴露 WebUI/宏配置内容 |
| `[ESP800]` | `Firmware/Info` | 固件、认证、通信方式等信息 | 低 |

常用探测顺序：

```text
[ESP800]
[ESP111]
[ESP400]
[ESP420]
[ESP720]
[ESP200]
[ESP210]
```

## 3a. 旧版 HTTP 配置命令（向后兼容参考）

以下命令“无参数时通常可读，有参数时会写配置”。自动化工具默认只能读，写入必须二次确认。

| 命令/名称 | 配置项 | 说明 | 写入风险 |
|---|---|---|---|
| `[ESP100]` / `Sta/SSID` | Station SSID | STA 模式连接的 Wi-Fi 名称 | 高，可能导致设备离线 |
| `[ESP101]` / `Sta/Password` | Station Password | STA Wi-Fi 密码 | 高，敏感信息 |
| `[ESP102]` / `Sta/IPMode` | DHCP/STATIC | STA IP 模式 | 高，可能导致设备不可达 |
| `[ESP103]IP=<ip> MSK=<mask> GW=<gateway>` / `Sta/Setup` | STA 静态网络 | STA IP/掩码/网关 | 高，可能导致设备不可达 |
| `Sta/IP` | Station Static IP | STA 静态 IP | 高 |
| `Sta/Gateway` | Station Gateway | STA 网关 | 高 |
| `Sta/Netmask` | Station Netmask | STA 子网掩码 | 高 |
| `[ESP105]` / `AP/SSID` | AP SSID | 设备热点名称 | 中 |
| `[ESP106]` / `AP/Password` | AP Password | 设备热点密码 | 高，敏感信息 |
| `[ESP107]` / `AP/IP` | AP IP | 设备热点 IP | 中 |
| `[ESP108]` / `AP/Channel` | AP Channel | 设备热点信道 | 中 |
| `[ESP110]` / `Radio/Mode` | `STA|AP|BT|OFF` | Wi-Fi/蓝牙模式 | 高，可能关闭网络入口 |
| `[ESP112]` / `System/Hostname` | Hostname | 主机名 | 中 |
| `[ESP115]` / `Radio/State` | `STA|AP|BT|OFF` 或 ON/OFF 风格状态 | 当前无线状态 | 高 |
| `[ESP120]` / `Http/Enable` | HTTP 开关 | Web 服务开关 | 高，可能关闭网页入口 |
| `[ESP121]` / `Http/Port` | HTTP 端口 | Web 服务端口 | 高，可能导致原地址失效 |
| `[ESP130]` / `Telnet/Enable` | Telnet 开关 | Telnet 服务开关 | 高 |
| `[ESP131]` / `Telnet/Port` | Telnet 端口 | Telnet 服务端口 | 高 |
| `[ESP140]` / `Bluetooth/Name` | 蓝牙名称 | BT 名称 | 中 |
| `[ESP401]P=<position> T=<type> V=<value>` | WebUI/Set | 原始位置写设置 | 极高，不建议自动化使用 |
| `[ESP555]<password>` | WebUI/SetUserPassword | 修改/重置用户密码 | 极高，敏感信息 |

配置写入原则：

- 默认禁止语音直接触发。
- 必须先读取旧值并展示差异。
- 必须要求用户明确确认目标设备 IP、配置项、旧值、新值。
- 修改网络入口后要提示用户新地址/端口可能变化。
- 不记录 Wi-Fi 密码、WebUI 密码、Token 明文。

## 4. 文件接口

### LocalFS / SPIFFS HTTP 文件接口

`/files` 管理设备本地 SPIFFS/LocalFS，WebUI 的 `index.html.gz`、`favicon.ico`、`macrocfg.json` 常在这里。

```text
GET  http://<device-ip>/files?action=list&filename=all&path=/
POST http://<device-ip>/files
```

常见操作：

| 接口/命令 | 用途 | 风险 |
|---|---|---|
| `GET /files?action=list&filename=all&path=/` | 列 LocalFS 根目录 | 低 |
| `POST /files` | 上传文件到 LocalFS | 高，可能覆盖 WebUI 文件 |
| `[ESP701]<path>` | 查看 LocalFS 文件 | 中 |
| `[ESP700]<path>` | 运行 LocalFS 文件 | 极高，可能执行 G-code/宏 |
| `[ESP710]FORMAT` | 格式化 LocalFS | 极高，会清空 LocalFS |
| `[ESP720]` | LocalFS 容量 | 低 |

注意：源码里还注册了 `LocalFS/List` 和 `LocalFS/ListJSON` 这类文本命令，但通过 `/command` 获取直接 HTTP 响应时，优先使用 `/files` 接口；文本命令在不同通信通道下的响应路径需要实机验证后再自动化依赖。

### SD 文件命令

SD 功能取决于固件编译选项和硬件是否有 SD 卡。

| 命令/接口 | 用途 | 风险 |
|---|---|---|
| `[ESP200]` | SD 卡状态 | 低 |
| `[ESP210]` | 列 SD 卡文件 | 低 |
| `[ESP221]<path>` | 查看 SD 文件 | 中 |
| `[ESP220]<path>` | 运行 SD 文件 | 极高，可能开始运动/激光 |
| `[ESP215]<path>` | 删除 SD 文件或目录 | 极高 |
| `GET /upload?...` | SD 文件列表/管理，需 SD 功能启用 | 中到高 |
| `POST /upload` | 上传到 SD，需 SD 功能启用 | 高 |

## 5. Telnet / GRBL 查询命令

这些命令通过 Telnet 或串口风格通道发送。只读查询可作为 allowlist，但仍要处理设备忙、认证、断线和响应格式差异。

| 命令 | 名称 | 用途 | 风险 |
|---|---|---|---|
| `?` | 实时状态 | 获取 `<Idle|MPos:...>` 等状态 | 低 |
| `$$` | `GrblSettings/List` | 标准 GRBL 设置 | 低 |
| `$+` | `ExtendedSettings/List` | 扩展设置 | 低到中 |
| `$L` | `GrblNames/List` | GRBL 名称列表 | 低 |
| `$S` | `Settings/List` | 全量设置列表 | 中，可能含敏感项占位 |
| `$SC` | `Settings/ListChanged` | 已变更设置 | 中 |
| `$CMD` | `Commands/List` | 命令列表 | 低 |
| `$A` | `Alarms/List` | 报警列表 | 低 |
| `$E` | `Errors/List` | 错误列表 | 低 |
| `$G` | `GCode/Modes` | 当前 G-code 模态 | 低 |
| `$#` | `GCode/Offsets` | 坐标偏移 | 低 |
| `$I` | `Build/Info` | 固件/机器信息 | 低 |
| `$N` | `GCode/StartupLines` | 启动行 | 中，启动行可能包含会执行的 G-code |
| `$V` | `Settings/Stats` | NVS 设置统计 | 低 |

常见只读排障顺序：

```text
?
$I
$$
$+
$G
$#
$N
```

## 6. 关键 GRBL 设置名

这些设置名称和编号由 Grbl_ESP32 源码定义，具体数值以实机返回为准。

| 设置 | 含义 | 自动化注意事项 |
|---|---|---|
| `$0` | Stepper/Pulse | 不要自动写 |
| `$1` | Stepper/IdleTime | 写入会影响电机保持 |
| `$2` | Stepper/StepInvert | 写错会导致步进异常 |
| `$3` | Stepper/DirInvert | 写错会导致方向反转 |
| `$10` | Report/Status | 影响状态返回字段 |
| `$20` | Limits/Soft | 软限位 |
| `$21` | Limits/Hard | 硬限位 |
| `$22` | Homing/Enable | 回零能力开关 |
| `$23` | Homing/DirInvert | 回零方向 |
| `$24` | Homing/Feed | 回零进给速度 |
| `$25` | Homing/Seek | 回零搜索速度 |
| `$27` | Homing/Pulloff | 回零后退距离 |
| `$30` | GCode/MaxS | 最大主轴/激光 S 值 |
| `$31` | GCode/MinS | 最小 S 值 |
| `$32` | GCode/LaserMode | 激光模式，激光机通常应为 `1` |
| `$100` `$101` `$102` | X/Y/Z StepsPerMm | 步进/mm，写错会导致尺寸错误 |
| `$110` `$111` `$112` | X/Y/Z MaxRate | 最大速度，写错会导致失步或危险运动 |
| `$120` `$121` `$122` | X/Y/Z Acceleration | 加速度 |
| `$130` `$131` `$132` | X/Y/Z MaxTravel | 行程范围 |

上位机不会把任何 `$` 设置值写死到代码：行程来自握手时读到的 `$130`/`$131` 或 FluidNC `$CD` 的 `max_travel_mm`，功率标尺来自 `$30`，激光模式来自 `$32`。翼宿 V1.0 的标称行程见 [硬件 BOM 基线](./hardware-bom.md)。

## 7. 高危命令清单

后续做语音控制或 MCP 自动化时，这些命令必须默认禁止或要求强确认。

### 文件/系统高危

| 命令 | 风险 |
|---|---|
| `[ESP220]<path>` | 运行 SD 文件，可能开始雕刻/运动 |
| `[ESP700]<path>` | 运行 LocalFS 文件，可能执行宏/G-code |
| `[ESP215]<path>` | 删除 SD 文件/目录 |
| `[ESP710]FORMAT` | 格式化 LocalFS |
| `[ESP444]RESTART` | 重启控制器 |
| `[ESP401]...` | 原始设置写入，容易破坏配置 |
| `POST /files` | 可能覆盖 WebUI 或宏配置 |
| `POST /upload` | 可能上传后被运行 |
| `POST /updatefw` | 固件升级，失败可能导致设备不可用 |

### GRBL/运动/激光高危

| 命令 | 风险 |
|---|---|
| `$H` | 回零，会移动机器 |
| `$X` | 解锁，可能解除报警后允许运动 |
| `$J=...` | 点动，会移动机器 |
| `$C` | 切换 G-code 检查模式，状态改变 |
| `$RST...` | 恢复设置，可能改变机器行为 |
| `$NVX` | 擦除 NVS 设置，极高风险 |
| `G0/G1/G2/G3...` | 运动命令 |
| `M3/M4` | 开启激光/主轴 |
| `M5` | 关闭激光/主轴，本身安全但仍属于控制命令 |
| `~` | 恢复运行 |
| `!` | 暂停运行 |
| `Ctrl-X` | 软复位 |

## 8. 推荐自动化策略

1. 默认只允许读取：`[ESP0]`、`[ESP111]`、`[ESP200]`、`[ESP210]`、`[ESP400]`、`[ESP420]`、`[ESP720]`、`[ESP800]`、`?`、`$$`、`$+`、`$G`、`$#`、`$I`、`$N`、`$CMD`、`$A`、`$E`。
2. 网络配置、端口配置、密码配置只允许在明确“配置模式”下执行。
3. 所有运动、激光、运行文件、删除、格式化、重启命令必须要求用户确认。
4. 发送任何 G-code 前先查询 `?`，确认设备状态，不要在 `Alarm`、`Run`、`Hold`、`Jog` 等状态下盲发。
5. 自动化生成 G-code 后，优先走“预览/检查/确认”流程，不要语音一句话直接开跑。
6. 日志中隐藏密码、Token、真实 Wi-Fi SSID/密码；设备 IP 可在本地日志显示，但不要写死进代码或规范。

当前仓库实现补充（Rust 版）：

- 设备传输只有串口与 Telnet（`laser_device::ConnectionSpec`）。没有 HTTP `/command` 通道，也没有 `[ESP800]` 探测；连接检查固定走 `$$` 握手并要求 `$32=1`。
- 任意单条命令不对外开放。`laser_grbl_tool` / `laser_network_grbl_tool` 的 `send_command` 与 `send_file` 一律拒绝；受控动作走 `laser_safe_action_tool` / `/api/safe-action`，其中 `status` 与进给/功率倍率不需确认，`unlock`、`home`、`set_origin`、`laser_off`、移动、`0x18` 需要 `confirmed=true`。
- 完整任务只走 workflow：`preview` 冻结 SHA-256 与参数，`confirm_send` 重算 SHA、握手、读 `$30`、`$CD`、Idle 与引脚预检后逐行发送；网络任务强制 Telnet。
- SD 脱机模式用固件 HTTP `/upload` 把文件写入 SD 卡，用 `$File/ShowHash` 比对哈希后 `$SD/Run`；这是 HTTP 在本仓库唯一的用途。
- 局域网发现用 mDNS（`POST /api/ui/discover-controllers`），只读。
- 上位机不写 `$30`、`$32`、`$100` 等只读代理；FluidNC 对这些写入回 `error:162`。

### 8a. 逐行流式发送：流控与应答对齐（2026-11-05 修订）

实现位于 `laser_device::JobWorker` / `DeviceConnection`。

- **字符计数窗口**：未确认（未收到 `ok`/`error`）的字节总数不超过窗口。`Bf:<blocks>,<rx>` 的 `rx` 是**此刻剩余**的接收缓冲字节，不是总容量；
  任何时刻剩余值都不大于容量，只有在顺序信道没有未确认字节时读到的剩余值才等于容量。
  - 尚未观测到容量：`$CD` 确认是 FluidNC 的网络连接从 256 字节起步；串口与未知设备（含经 Wi-Fi 串口桥接的经典 GRBL）一律从 127 字节起步
    （GRBL 1.1 `RX_BUFFER_SIZE=128`，环形缓冲可用 127，空闲时 `Bf:` 报 128）。
  - 预检 `?`、完成等待等无未确认字节的时刻读到 `Bf:` 后，窗口 = 观测到的最大剩余值 − 1，最多 1016 字节，可低于初始值；
    FluidNC Telnet 空闲报 `Bf:15,1200` 时窗口为 1016（低于固件 1020 字节的行队列上限，2026-11-09）。
  - 发送中读到的 `Bf:` 只能把窗口抬高到"剩余值 − 1"（剩余值是容量的下界），不能把窗口压低；不再套用旧版的 255 字节下限。
  - `$10` 不带 bit1（状态报告无 `Bf:`）时窗口不超过 256，握手摘要提示设 `$10=3`。
- **读超时不丢半行**：串口与 Telnet 读取端各自保存跨超时的半行缓冲；`ok` 被 Wi-Fi 抖动拆成 `o` + 超过 500ms 的间隔 + `k` 时，下次读取拼成完整的 `ok`。
- **`$RI` 自动上报应答对齐**：FluidNC 设备在首行 G-code 前发 `$RI=200`，2 秒内收到应答即放行；未收到时补发 `G4 P0`（零时长暂停，不运动、不出光），
  按"应答与命令同序"在 3 秒内收齐 `$RI` 与 `G4 P0` 两条终止应答后才开始发送；收不齐则把连接标为 `Lost`、发 `$RI=0` 收尾并报错，不发送任何 G-code，
  避免迟到的 `ok` 被算作第一行 G-code 的 ack。

## 9. 当前 MCP 网络工具边界

本仓库的网络控制不是直接暴露全部 WebUI 能力，而是通过工具层做分类和确认：

| 工具/动作 | 用途 | 安全边界 |
|---|---|---|
| `check_laser_connection_tool` | 只读握手，返回连通与就绪摘要 | 不改变设备状态 |
| `laser_safe_action_tool` | 语义动作映射到受控命令或实时字节 | `status` 与倍率不需确认；其他动作需 `confirmed=true`；移动前固定 `M5` |
| `laser_workflow_tool` `preview` / `confirm_send` / `status` / `cancel` | 完整任务的唯一路径 | `confirm_send` 需 `confirmed=true` 并通过全部门禁；`cancel` 停止后续发送，不等于急停 |
| `run_calibration_grid_tool` / `start_tuned_job_tool` | 材料矩阵与调参后的正式任务 | 默认预览；确认后复用同一套发送门禁 |
| `laser_grbl_tool` / `laser_network_grbl_tool` | 仅 `status` / `check` / `probe` / `cancel` | `send_file`、`send_command` 已拒绝 |

每道门禁的顺序与错误码见 [开发者安全入口](../developer/safety.md)。

## 10. 源码依据

- `Grbl_Esp32-master/doc/Commands.txt`：ESP 命令参考。
- `Grbl_Esp32-master/Grbl_Esp32/src/WebUI/WebServer.cpp`：`/command`、`/files`、`/upload`、`/updatefw` 路由和上传处理。
- `Grbl_Esp32-master/Grbl_Esp32/src/WebUI/WebSettings.cpp`：ESP/WebUI 命令注册和实现。
- `Grbl_Esp32-master/Grbl_Esp32/src/ProcessSettings.cpp`：GRBL `$` 命令注册和执行。
- `Grbl_Esp32-master/Grbl_Esp32/src/SettingsDefinitions.cpp`：GRBL 设置编号和含义。
- `Grbl_Esp32-master/Grbl_Esp32/src/WebUI/WifiConfig.h`：默认 HTTP/Telnet 端口和 Wi-Fi 默认值。
- `Grbl_Esp32-master/Grbl_Esp32/src/WebUI/TelnetServer.cpp`：Telnet 启动和端口读取。

## 11. 全量 HTTP/WebUI 入口

这些是 `WebServer.cpp` 中注册或处理的 HTTP/WebUI 入口。部分入口需要对应编译宏启用。

| 入口 | 方法 | 条件 | 用途 | 风险 |
|---|---|---|---|---|
| `/` | `GET/POST/ANY` | HTTP 启用 | 根页面，优先返回 `index.html` 或内置页面 | 低 |
| `/login` | `GET/POST/ANY` | 认证功能启用时有意义 | WebUI 登录/会话 | 中，涉及凭据 |
| `/command` | `GET/POST/ANY` | HTTP 启用 | 执行 `[ESP...]`、`$...` 或 G-code 命令，参数 `commandText` 或 `plain` | 取决于命令 |
| `/command_silent` | `GET/POST/ANY` | HTTP 启用 | 静默执行命令 | 取决于命令 |
| `/files` | `GET/POST/ANY` | HTTP 启用 | SPIFFS/LocalFS 列表、删除、建目录、上传 | 中到极高 |
| `/updatefw` | `POST/ANY` | HTTP 启用 | Web OTA 固件更新 | 极高 |
| `/upload` | `GET/POST/ANY` | `ENABLE_SD_CARD` | SD 卡直接文件列表、删除、建目录、上传 | 中到极高 |
| `/generate_204` | `ANY` | AP captive portal | Captive portal 兼容入口 | 低 |
| `/gconnectivitycheck.gstatic.com` | `ANY` | AP captive portal | Captive portal 兼容入口 | 低 |
| `/fwlink/` | `ANY` | AP captive portal | Captive portal 兼容入口 | 低 |
| `/description.xml` | `GET` | `ENABLE_SSDP` 且 STA | SSDP 描述 | 低 |
| 静态文件路径 | `GET` | 文件存在于 SPIFFS/LocalFS | 返回 WebUI 资源，如 HTML/CSS/JS/GZ/ICO | 低 |

> **⚠️ 固件协议与 WebSocket 关键架构差异提示（Grbl_ESP32 vs FluidNC）**：
>
> 1. **旧版 Grbl_ESP32**：
>    WebSocket 由 `WebSocketsServer(_port + 1)` 独立启动，端口通常为 HTTP 端口加 1（如 81）。旧源码中 WebSocket 主要用于 WebUI 状态/输出同步，其 `WStype_TEXT` 事件本身没有直接执行文本命令，直接命令入口仍以 HTTP `/command` 和 Telnet 为准。
> 2. **现代 FluidNC**：
>    FluidNC 使用 `AsyncWebSocket("/")` 与 HTTP 共享同一端口（如 80），并在 `WSChannel.cpp` 中将收到的文本及二进制数据直接投递进系统的接收队列执行。
> 3. **上位机对接规范**：
>    当前上位机（MOSS）**严禁直接套用旧版 Grbl_ESP32 文档开发 WebSocket 客户端**；设备通信现阶段统一收拢在后端 Rust（`laser-device`）通过 TCP/Telnet 或串口主链路进行受控访问与状态轮询，严禁前端浏览器直连硬件，确保所有 G-code 下发与实时命令严格遵循安全确认门禁。

## 12. 全量 ESP/WebUI 命令

这些命令来自 `doc/Commands.txt` 和 `WebSettings.cpp::make_web_settings()`。带 `[ESPxxx]` 的兼容名和斜杠名称通常指向同一个设置或命令。

| 命令 | 名称 | 参数 | 条件 | 用途 | 风险 |
|---|---|---|---|---|---|
| `[ESP]` | `WebUI/Help` | 无 | `WEB_COMMON` | WebUI 帮助 | 低 |
| `[ESP0]` | `WebUI/Help` | 无 | `WEB_COMMON` | WebUI 帮助 | 低 |
| `[ESP100]` | `Sta/SSID` | `<ssid>` | `ENABLE_WIFI` | 读/写 STA SSID | 高 |
| `[ESP101]` | `Sta/Password` | `<password>` | `ENABLE_WIFI` | 写 STA 密码 | 高，敏感 |
| `[ESP102]` | `Sta/IPMode` | `DHCP` 或 `STATIC` | `ENABLE_WIFI` | 读/写 STA IP 模式 | 高 |
| `[ESP103]` | `Sta/Setup` | `IP=<ip> MSK=<mask> GW=<gateway>` | `ENABLE_WIFI` | 设置 STA 静态 IP/掩码/网关 | 高 |
| `Sta/IP` | `Sta/IP` | `<ip>` | `ENABLE_WIFI` | 读/写 STA 静态 IP | 高 |
| `Sta/Gateway` | `Sta/Gateway` | `<gateway>` | `ENABLE_WIFI` | 读/写 STA 网关 | 高 |
| `Sta/Netmask` | `Sta/Netmask` | `<mask>` | `ENABLE_WIFI` | 读/写 STA 子网掩码 | 高 |
| `[ESP105]` | `AP/SSID` | `<ssid>` | `ENABLE_WIFI` | 读/写 AP SSID | 中 |
| `[ESP106]` | `AP/Password` | `<password>` | `ENABLE_WIFI` | 写 AP 密码 | 高，敏感 |
| `[ESP107]` | `AP/IP` | `<ip>` | `ENABLE_WIFI` | 读/写 AP IP | 中 |
| `[ESP108]` | `AP/Channel` | `1..14` | `ENABLE_WIFI` | 读/写 AP 信道 | 中 |
| `[ESP110]` | `Radio/Mode` | `STA|AP|BT|OFF` | `WIFI_OR_BLUETOOTH` | 读/写无线/蓝牙模式 | 高 |
| `[ESP111]` | `System/IP` | 无 | `ENABLE_WIFI` | 读取当前 IP | 低 |
| `[ESP112]` | `System/Hostname` | `<hostname>` | `ENABLE_WIFI` | 读/写主机名 | 中 |
| `[ESP115]` | `Radio/State` | `STA|AP|BT|OFF` | `WEB_COMMON` | 立即切换无线/蓝牙状态 | 高 |
| `[ESP120]` | `Http/Enable` | `ON|OFF` | `ENABLE_WIFI` | 读/写 HTTP 开关 | 高 |
| `[ESP121]` | `Http/Port` | `<port>` | `ENABLE_WIFI` | 读/写 HTTP 端口 | 高 |
| `[ESP130]` | `Telnet/Enable` | `ON|OFF` | `ENABLE_WIFI` | 读/写 Telnet 开关 | 高 |
| `[ESP131]` | `Telnet/Port` | `<port>` | `ENABLE_WIFI` | 读/写 Telnet 端口 | 高 |
| `[ESP140]` | `Bluetooth/Name` | `<name>` | `ENABLE_BLUETOOTH` | 读/写蓝牙名称 | 中 |
| `[ESP200]` | `SD/Status` | 无 | `WEB_COMMON` | SD 卡状态 | 低 |
| `[ESP210]` | `SD/List` | 无 | `ENABLE_SD_CARD` | SD 文件列表/容量 | 低 |
| `[ESP215]` | `SD/Delete` | `<file_or_directory_path>` | `ENABLE_SD_CARD` | 删除 SD 文件/目录 | 极高 |
| `[ESP220]` | `SD/Run` | `<path>` | `ENABLE_SD_CARD` | 运行 SD 文件 | 极高 |
| `[ESP221]` | `SD/Show` | `<path>` | `ENABLE_SD_CARD` | 查看 SD 文件 | 中 |
| `[ESP400]` | `WebUI/List` | 无 | `WEB_COMMON` | 列 WebUI 设置 | 中 |
| `[ESP401]` | `WebUI/Set` | `P=<position> T=<type> V=<value>` | `WEB_COMMON` | 按原始位置写设置 | 极高 |
| `[ESP410]` | `WiFi/ListAPs` | 可带输出格式参数 | `ENABLE_WIFI` | 扫描 AP 列表 | 中 |
| `[ESP420]` | `System/Stats` | 无 | `WEB_COMMON` | 系统统计 | 低 |
| `[ESP444]` | `System/Control` | `RESTART` | `WEB_COMMON` | 重启控制器 | 极高 |
| `[ESP555]` | `WebUI/SetUserPassword` | `<password>` 或空 | `ENABLE_AUTHENTICATION` | 修改/重置用户密码 | 极高，敏感 |
| `[ESP600]` | `Notification/Send` | `<message>` | `ENABLE_NOTIFICATIONS` | 发送通知 | 中 |
| `[ESP610]` | `Notification/Setup` | `TYPE=NONE|PUSHOVER|EMAIL|LINE T1=<token1> T2=<token2> TS=<settings>` | `ENABLE_NOTIFICATIONS` | 读/写通知配置 | 高，敏感 |
| `[ESP700]` | `LocalFS/Run` | `<path>` | `WEB_COMMON` | 运行 SPIFFS/LocalFS 文件 | 极高 |
| `[ESP701]` | `LocalFS/Show` | `<path>` | `WEB_COMMON` | 查看 SPIFFS/LocalFS 文件 | 中 |
| `[ESP710]` | `LocalFS/Format` | `FORMAT` | `WEB_COMMON` | 格式化 LocalFS | 极高 |
| `[ESP720]` | `LocalFS/Size` | 可选标题参数 | `WEB_COMMON` | LocalFS 容量 | 低 |
| `[ESP800]` | `Firmware/Info` | 可选标题参数 | `WEB_COMMON` | 固件信息、认证状态、通信模式 | 低 |
| `LocalFS/List` | `LocalFS/List` | `<path>` | `WEB_COMMON` | LocalFS 文本列表 | 低 |
| `LocalFS/ListJSON` | `LocalFS/ListJSON` | `<path>` | `WEB_COMMON` | LocalFS JSON 列表 | 低 |
| `Notification/Type` | `Notification/Type` | `NONE|PUSHOVER|EMAIL|LINE` | `ENABLE_NOTIFICATIONS` | 通知类型 | 高 |
| `Notification/T1` | `Notification/T1` | `<token1>` | `ENABLE_NOTIFICATIONS` | 通知令牌 1 | 高，敏感 |
| `Notification/T2` | `Notification/T2` | `<token2>` | `ENABLE_NOTIFICATIONS` | 通知令牌 2 | 高，敏感 |
| `Notification/TS` | `Notification/TS` | `<settings>` | `ENABLE_NOTIFICATIONS` | 通知附加设置 | 高，敏感 |
| `WebUI/UserPassword` | `WebUI/UserPassword` | `<password>` | `ENABLE_AUTHENTICATION` | 用户密码 | 极高，敏感 |
| `WebUI/AdminPassword` | `WebUI/AdminPassword` | `<password>` | `ENABLE_AUTHENTICATION` | 管理员密码 | 极高，敏感 |

## 13. 全量 GRBL `$` 命令

这些命令来自 `ProcessSettings.cpp::make_grbl_commands()`。通过 Telnet、串口、Web `/command` 都可能进入同一执行路径。

| 命令 | 名称 | 参数 | 用途 | 风险 |
|---|---|---|---|---|
| `$` | `Help` | 无 | GRBL 帮助 | 低 |
| `$T` | `State` | 无 | 输出内部状态值 | 低 |
| `$J=...` | `Jog` | Jog G-code，如 `G91 X1 F1000` | 点动 | 高，运动 |
| `$$` | `GrblSettings/List` | 无 | 列标准 GRBL 设置 | 低 |
| `$+` | `ExtendedSettings/List` | 无 | 列标准与扩展设置 | 中 |
| `$L` | `GrblNames/List` | 无 | 列编号设置到名称的映射 | 低 |
| `$S` | `Settings/List` | 无 | 列全部设置名称和值 | 中，可能含隐藏/敏感项 |
| `$SC` | `Settings/ListChanged` | 无 | 列非默认设置 | 中 |
| `$CMD` | `Commands/List` | 无 | 列 `$` 命令 | 低 |
| `$A` | `Alarms/List` | 无 | 报警表 | 低 |
| `$E` | `Errors/List` | 无 | 错误表 | 低 |
| `$G` | `GCode/Modes` | 无 | 当前 G-code 模态 | 低 |
| `$C` | `GCode/Check` | 无 | 切换检查模式 | 高，状态改变 |
| `$X` | `Alarm/Disable` | 无 | 解锁报警 | 高 |
| `$NVX` | `Settings/Erase` | 无 | 擦除 NVS 设置 | 极高 |
| `$V` | `Settings/Stats` | 无 | NVS 设置统计 | 低 |
| `$#` | `GCode/Offsets` | 无 | 坐标偏移、G54-G59/G28/G30 等 | 低 |
| `$H` | `Home` | 无 | 全轴回零 | 高，运动 |
| `$MD` | `Motor/Disable` | 空、位掩码或轴列表 | 关闭电机使能 | 高 |
| `$HX` | `Home/X` | 无 | X 轴回零 | 高，运动，需 `HOMING_SINGLE_AXIS_COMMANDS` |
| `$HY` | `Home/Y` | 无 | Y 轴回零 | 高，运动，需 `HOMING_SINGLE_AXIS_COMMANDS` |
| `$HZ` | `Home/Z` | 无 | Z 轴回零 | 高，运动，需 `HOMING_SINGLE_AXIS_COMMANDS` |
| `$HA` | `Home/A` | 无 | A 轴回零 | 高，运动，需 `HOMING_SINGLE_AXIS_COMMANDS` |
| `$HB` | `Home/B` | 无 | B 轴回零 | 高，运动，需 `HOMING_SINGLE_AXIS_COMMANDS` |
| `$HC` | `Home/C` | 无 | C 轴回零 | 高，运动，需 `HOMING_SINGLE_AXIS_COMMANDS` |
| `$SLP` | `System/Sleep` | 无 | 进入睡眠 | 高，状态改变 |
| `$I` | `Build/Info` | 无 | 构建/机器信息 | 低 |
| `$N` | `GCode/StartupLines` | 无 | 查看启动行 | 中 |
| `$N0=...` | `GCode/Line0` | G-code 文本 | 写启动行 0 | 高，开机可执行 |
| `$N1=...` | `GCode/Line1` | G-code 文本 | 写启动行 1 | 高，开机可执行 |
| `$RST=$` 或 `$RST=settings` | `Settings/Restore` | `$` 或 `settings` | 恢复默认设置，需编译支持 | 极高 |
| `$RST=#` 或 `$RST=gcode` | `Settings/Restore` | `#` 或 `gcode` | 清 G-code 参数，需编译支持 | 极高 |
| `$RST=*` 或 `$RST=all` | `Settings/Restore` | `*` 或 `all` | 清全部设置，需编译支持 | 极高 |
| `$RST=@` 或 `$RST=wifi` | `Settings/Restore` | `@` 或 `wifi` | 恢复 Wi-Fi 设置 | 极高，可能断联 |

## 14. 全量 GRBL 设置项

设置项来自 `SettingsDefinitions.cpp::make_settings()`。带编号的设置可用 `$编号` 读取、`$编号=值` 写入；带名称的设置也可用 `$名称` 或 `[名称]` 风格读取/写入。写设置默认都应要求确认。

### 标准编号设置

| 编号 | 名称 | 含义 | 风险 |
|---|---|---|---|
| `$0` | `Stepper/Pulse` | 步进脉冲宽度 | 高 |
| `$1` | `Stepper/IdleTime` | 步进空闲保持时间 | 中 |
| `$2` | `Stepper/StepInvert` | 步进信号反相掩码 | 高 |
| `$3` | `Stepper/DirInvert` | 方向信号反相掩码 | 高 |
| `$4` | `Stepper/EnableInvert` | 使能信号反相 | 高 |
| `$5` | `Limits/Invert` | 限位输入反相 | 高 |
| `$6` | `Probe/Invert` | 探针输入反相 | 中 |
| `$10` | `Report/Status` | 状态报告字段 | 中 |
| `$11` | `GCode/JunctionDeviation` | 拐角偏差 | 高 |
| `$12` | `GCode/ArcTolerance` | 圆弧容差 | 中 |
| `$13` | `Report/Inches` | 英寸报告开关 | 中 |
| `$20` | `Limits/Soft` | 软限位 | 高 |
| `$21` | `Limits/Hard` | 硬限位 | 高 |
| `$22` | `Homing/Enable` | 回零开关 | 高 |
| `$23` | `Homing/DirInvert` | 回零方向掩码 | 高 |
| `$24` | `Homing/Feed` | 回零进给速度 | 高 |
| `$25` | `Homing/Seek` | 回零搜索速度 | 高 |
| `$26` | `Homing/Debounce` | 回零消抖 | 中 |
| `$27` | `Homing/Pulloff` | 回零后退距离 | 高 |
| `$30` | `GCode/MaxS` | 最大 S 值 | 高，影响激光功率 |
| `$31` | `GCode/MinS` | 最小 S 值 | 中 |
| `$32` | `GCode/LaserMode` | 激光模式 | 高 |
| `$33` | `Spindle/PWM/Frequency` | PWM 频率 | 高 |
| `$34` | `Spindle/PWM/Off` | PWM 关闭占空比/百分比 | 高 |
| `$35` | `Spindle/PWM/Min` | PWM 最小值 | 高 |
| `$36` | `Spindle/PWM/Max` | PWM 最大值 | 高 |

### 轴编号设置模式

这些编号按轴递增：X=base+0、Y=base+1、Z=base+2、A=base+3、B=base+4、C=base+5。实机实际有几个轴取决于 `N_AXIS`/机型配置。

| 编号范围 | 名称模式 | 含义 | 风险 |
|---|---|---|---|
| `$100`-`$105` | `<Axis>/StepsPerMm` | 各轴步进/mm | 高 |
| `$110`-`$115` | `<Axis>/MaxRate` | 各轴最大速度 | 高 |
| `$120`-`$125` | `<Axis>/Acceleration` | 各轴加速度 | 高 |
| `$130`-`$135` | `<Axis>/MaxTravel` | 各轴最大行程 | 高 |
| `$140`-`$145` | `<Axis>/Current/Run` | 各轴运行电流，扩展设置 | 高，取决于驱动支持 |
| `$150`-`$155` | `<Axis>/Current/Hold` | 各轴保持电流，扩展设置 | 高，取决于驱动支持 |
| `$160`-`$165` | `<Axis>/Microsteps` | 各轴细分，扩展设置 | 高，取决于驱动支持 |
| `$170`-`$175` | `<Axis>/StallGuard` | 各轴 StallGuard，扩展设置 | 高，取决于驱动支持 |
| 无编号 | `<Axis>/Home/Mpos` | 各轴回零后的机器坐标，扩展设置 | 高 |

三轴常见展开：

| X | Y | Z | 含义 |
|---|---|---|---|
| `$100` | `$101` | `$102` | StepsPerMm |
| `$110` | `$111` | `$112` | MaxRate |
| `$120` | `$121` | `$122` | Acceleration |
| `$130` | `$131` | `$132` | MaxTravel |
| `$140` | `$141` | `$142` | Current/Run |
| `$150` | `$151` | `$152` | Current/Hold |
| `$160` | `$161` | `$162` | Microsteps |
| `$170` | `$171` | `$172` | StallGuard |

### 具名扩展设置

| 名称 | 含义 | 风险 |
|---|---|---|
| `G54`、`G55`、`G56`、`G57`、`G58`、`G59` | 工件坐标系 | 高，影响坐标 |
| `G28`、`G30` | 预设返回点 | 高，影响运动目标 |
| `Errors/Verbose` | 详细错误输出 | 低 |
| `NumberAxis` | 轴数，只读/伪设置 | 低 |
| `Spindle/Type` | 主轴类型 | 高 |
| `Spindle/PWM/Invert` | PWM 输出反相 | 高 |
| `Spindle/Delay/SpinUp` | 主轴启动延迟 | 中 |
| `Spindle/Delay/SpinDown` | 主轴停止延迟 | 中 |
| `Spindle/Enable/OffWithSpeed` | S 为 0 时关闭使能 | 高 |
| `Spindle/Enable/Invert` | 主轴使能反相 | 高 |
| `Laser/FullPower` | 激光满功率值 | 高 |
| `Homing/Squared` | 双电机方正回零轴 | 高 |
| `Firmware/Build` | 构建信息 | 低 |
| `Report/StallGuard` | StallGuard 报告掩码 | 中 |
| `Homing/Cycle0` 到 `Homing/Cycle5` | 回零循环配置 | 高 |
| `User/Macro0` 到 `User/Macro3` | 用户宏 | 极高，可能执行 G-code |
| `Spindle/EM/Hold` | XBoard 电磁保持值 | 高，机型相关 |
| `Spindle/EM/Invert` | XBoard 电磁输出反相 | 高，机型相关 |
| `Spindle/Servo/MaxAngle` | 伺服最大角度 | 高，机型相关 |
| `Spindle/Servo/Invert` | 伺服反相 | 高，机型相关 |

## 15. 全量实时命令

实时命令来自 `Config.h::Cmd` 和 `Serial.cpp::execute_realtime_command()`。它们通常是单字符或扩展字节，可在流中立即生效。

| 命令 | 字节 | 含义 | 风险 |
|---|---|---|---|
| `Ctrl-X` | `0x18` | 软复位 | 极高 |
| `?` | `0x3F` | 实时状态报告 | 低 |
| `~` | `0x7E` | Cycle Start / 恢复运行 | 高 |
| `!` | `0x21` | Feed Hold / 暂停 | 中到高 |
| Safety Door | `0x84` | 安全门状态 | 高 |
| Jog Cancel | `0x85` | 取消点动 | 中 |
| Debug Report | `0x86` | 调试报告，仅 DEBUG | 低 |
| Feed Override Reset | `0x90` | 进给倍率恢复 100% | 中 |
| Feed Override Coarse Plus | `0x91` | 进给倍率大步增加 | 中 |
| Feed Override Coarse Minus | `0x92` | 进给倍率大步减少 | 中 |
| Feed Override Fine Plus | `0x93` | 进给倍率小步增加 | 中 |
| Feed Override Fine Minus | `0x94` | 进给倍率小步减少 | 中 |
| Rapid Override Reset | `0x95` | 快移倍率恢复 | 中 |
| Rapid Override Medium | `0x96` | 快移中倍率 | 中 |
| Rapid Override Low | `0x97` | 快移低倍率 | 中 |
| Rapid Override Extra Low | `0x98` | 源码标注不支持 | 中 |
| Spindle Override Reset | `0x99` | 主轴/激光倍率恢复 | 高 |
| Spindle Override Coarse Plus | `0x9A` | 主轴/激光倍率大步增加 | 高 |
| Spindle Override Coarse Minus | `0x9B` | 主轴/激光倍率大步减少 | 高 |
| Spindle Override Fine Plus | `0x9C` | 主轴/激光倍率小步增加 | 高 |
| Spindle Override Fine Minus | `0x9D` | 主轴/激光倍率小步减少 | 高 |
| Spindle Override Stop | `0x9E` | 主轴/激光停止覆盖 | 高 |
| Coolant Flood Toggle | `0xA0` | 冷却液 Flood 切换 | 中，取决于硬件 |
| Coolant Mist Toggle | `0xA1` | 冷却液 Mist 切换 | 中，取决于硬件 |

## 16. 源码支持的 G-code / M-code 指令

这些来自 `GCode.h` 和 `GCode.cpp` 解析器。它不是完整 CNC 标准全集，而是该源码实际解析的子集。所有运动、激光、主轴、探针、坐标和宏相关命令都应视为需要确认。

### G 指令

| 指令 | 分组 | 含义 | 风险 |
|---|---|---|---|
| `G0` | Motion | 快速直线移动 | 高，运动 |
| `G1` | Motion | 进给直线移动 | 高，运动/激光 |
| `G2` | Motion | 顺时针圆弧 | 高，运动/激光 |
| `G3` | Motion | 逆时针圆弧 | 高，运动/激光 |
| `G4` | Non-modal | 暂停/Dwell，通常配 `P` | 中 |
| `G10` | Non-modal | 设置坐标数据，配 `L/P/...` | 高 |
| `G17` | Plane | XY 平面 | 中 |
| `G18` | Plane | ZX 平面 | 中 |
| `G19` | Plane | YZ 平面 | 中 |
| `G20` | Units | 英寸单位 | 高，影响后续尺寸 |
| `G21` | Units | 毫米单位 | 中 |
| `G28` | Non-modal | 返回 G28 位置 | 高，运动 |
| `G28.1` | Non-modal | 设置 G28 位置 | 高 |
| `G30` | Non-modal | 返回 G30 位置 | 高，运动 |
| `G30.1` | Non-modal | 设置 G30 位置 | 高 |
| `G38.2` | Motion | 探针靠近，失败报错 | 高，运动，需探针引脚 |
| `G38.3` | Motion | 探针靠近，失败不报错 | 高，运动，需探针引脚 |
| `G38.4` | Motion | 探针远离，失败报错 | 高，运动，需探针引脚 |
| `G38.5` | Motion | 探针远离，失败不报错语义 | 高，运动，需探针引脚 |
| `G40` | Cutter compensation | 关闭刀补，源码只保留兼容 | 低 |
| `G43.1` | Tool length | 动态刀长偏移 | 高，影响坐标 |
| `G49` | Tool length | 取消刀长偏移 | 中 |
| `G53` | Non-modal | 机器坐标绝对覆盖 | 高，运动/坐标 |
| `G54` | Coordinate system | 选择坐标系 G54 | 中 |
| `G55` | Coordinate system | 选择坐标系 G55 | 中 |
| `G56` | Coordinate system | 选择坐标系 G56 | 中 |
| `G57` | Coordinate system | 选择坐标系 G57 | 中 |
| `G58` | Coordinate system | 选择坐标系 G58 | 中 |
| `G59` | Coordinate system | 选择坐标系 G59 | 中 |
| `G61` | Control mode | Exact Path，源码只支持 `G61` 不支持 `G61.1` | 中 |
| `G80` | Motion | 取消 canned cycle / motion none | 中 |
| `G90` | Distance | 绝对距离模式 | 中 |
| `G91` | Distance | 增量距离模式 | 高，影响后续运动 |
| `G91.1` | Arc distance | 圆弧 IJK 增量模式，默认且唯一支持 | 中 |
| `G92` | Non-modal | 设置坐标偏移 | 高 |
| `G92.1` | Non-modal | 清坐标偏移 | 高 |
| `G93` | Feed rate | 反时间进给 | 高 |
| `G94` | Feed rate | 每分钟单位进给 | 中 |

明确不支持或源码注释限制：`G90.1`、`G59.x`、`G61.1`、`G41/G42`、大多数 canned cycles、未列出的其他 G 指令。

### M 指令

| 指令 | 分组 | 含义 | 风险 |
|---|---|---|---|
| `M0` | Program flow | 暂停 | 中 |
| `M1` | Program flow | Optional Stop，源码标注有效但不支持/忽略 | 低 |
| `M2` | Program flow | 程序结束 | 中 |
| `M3` | Spindle/Laser | 主轴顺时针/激光常功率 | 极高 |
| `M4` | Spindle/Laser | 主轴逆时针或激光动态功率，需可反转主轴或激光模式 | 极高 |
| `M5` | Spindle/Laser | 关闭主轴/激光 | 高 |
| `M6` | Tool change | 换刀，取决于 `USE_TOOL_CHANGE` | 高，机型相关 |
| `M7` | Coolant | Mist 冷却，需 `COOLANT_MIST_PIN` | 中，机型相关 |
| `M8` | Coolant | Flood 冷却，需 `COOLANT_FLOOD_PIN` | 中，机型相关 |
| `M9` | Coolant | 关闭冷却 | 中 |
| `M30` | Program flow | 程序结束/复位 | 中 |
| `M56` | Override | Parking override，需 `ENABLE_PARKING_OVERRIDE_CONTROL` | 高 |
| `M62` | User I/O | 同步数字输出开 | 高，外设相关 |
| `M63` | User I/O | 同步数字输出关 | 高，外设相关 |
| `M64` | User I/O | 立即数字输出开 | 高，外设相关 |
| `M65` | User I/O | 立即数字输出关 | 高，外设相关 |
| `M67` | User I/O | 同步模拟输出 | 高，外设相关 |
| `M68` | User I/O | 立即模拟输出 | 高，外设相关 |

### 参数字

| 参数字 | 用途 | 说明 |
|---|---|---|
| `X Y Z` | 轴坐标 | 取决于机器轴数 |
| `A B C` | 扩展轴坐标 | 只有对应轴存在时支持 |
| `F` | 进给速度 | 不允许负值 |
| `S` | 主轴/激光速度或功率 | 不允许负值 |
| `I J K` | 圆弧偏移/探针相关 | 用于圆弧等 |
| `L` | G10/canned 参数 | 整数 |
| `N` | 行号 | 不允许负值 |
| `P` | G10/Dwell 等参数 | 不允许负值 |
| `Q` | User I/O 模拟值等 | 用于 M67/M68 |
| `R` | 圆弧半径 | 用于圆弧 |
| `T` | 刀号 | 仅跟踪/换刀相关 |
| `E` | User I/O 编号 | 用于 M67/M68 |

未列出的参数字会被解析器拒绝；源码中 `D`、`H` 等未启用。
