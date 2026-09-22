# 快速开始

面向在 Windows 上使用 MOSS 控制翼宿 V1.0 / FluidNC 激光机的用户。开发、测试与构建规范见 [开发规范](../developer/standards.md)。

## 1. 准备

- Windows 10/11。自行编译时需要 Rust 1.98+（stable，MSVC 工具链）和 Node.js 20+；只运行已编译的 `.exe` 时不需要。
- 激光机通过 USB 串口或局域网连接。网络模式只走 Telnet（默认端口 23），FluidNC 侧需要开启的项见 [FluidNC 固件侧配置](../laser/fluidnc-setup.md)。
- 想用小智语音控制时，准备小智控制台生成的 MCP 接入点地址。

## 2. 启动

### 方式 A：双击 `scripts/start.bat`

- 默认以桌面模式启动 `target/release/moss-desktop.exe`（没有 release 时退回 `target/debug/`）。
- `start.bat -Server` 或 `-Host`：改为启动 `moss-host.exe serve`，并用浏览器打开页面。
- 两个可执行文件都不存在时，脚本会执行 `cargo build --release -p moss-desktop -p moss-host`。这条命令依赖仓库根目录的 Cargo workspace 清单。

### 方式 B：命令行

```powershell
# 桌面端（Tauri 2，进程内自带后端）
cd apps/desktop/src-tauri
cargo tauri dev

# 无界面宿主（HTTP + MCP 桥），控制台会打印实际监听地址
cd apps/moss-host
cargo run -- serve

# Android 调试包
cd apps/desktop/src-tauri
cargo tauri android build --debug --target aarch64 --apk
```

`moss-host serve` 启动后打印 `moss-host serving on <host>:<port>`，以及 MCP 桥是否启用。

### 端口

- 代码默认端口是 `9666`。可用环境变量 `LASER_WEB_PORT` 覆盖，或在界面"偏好设置 → 服务监听端口"修改；界面修改写入运行目录的 `settings.json`，下次启动生效。
- 首选端口被占用时，宿主会自动向后尝试 50 个端口，再不行就由系统随机分配。实际端口以控制台输出为准。
- `start.bat -Server` 固定打开 `http://127.0.0.1:8766`，与代码默认的 9666 不一致；如果浏览器打不开页面，请按控制台打印的地址访问。

## 3. 配置文件 `.env`

`.env` 从启动时的当前目录读取（通常是仓库根目录），所有项都可选。真实 token、设备 IP、串口号只放本机 `.env`，不要写进文档或提交。

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `MCP_ENDPOINT` | 未设置 | 小智控制台的接入点，形如 `wss://<host>/mcp/?token=<token>`。未设置、为空或仍含 `<` 占位符时，MCP 桥静默不启动 |
| `LASER_WEB_HOST` | `127.0.0.1` | HTTP 监听地址。绑定 `0.0.0.0` 后局域网设备可访问，但需要配对（见第 7 节） |
| `LASER_WEB_PORT` | `9666` | HTTP 监听端口 |
| `LASER_DEFAULT_CONNECTION_MODE` | `network` | 默认连接方式，`network` 或 `serial` |
| `LASER_ENGRAVING_MODE` | `raster` | 默认雕刻策略，`raster` 或 `outline` |
| `LASER_MATERIAL_PARAMS_FILE` | 未设置 | 材料库 JSON 文件路径。未设置时使用内置材料库，保存的参数只写入 SQLite |
| `GRBL_DEFAULT_PORT` | 未设置 | 串口模式的默认串口，例如 `COM3`。未设置时必须在界面选择 |
| `GRBL_BAUDRATE` | `115200` | 串口波特率 |
| `LASER_NETWORK_HOST` | 未设置 | 网络模式的默认设备地址。未设置时必须在界面填写或扫描 |
| `LASER_NETWORK_TELNET_PORT` | `23` | Telnet 端口 |
| `MOSS_ENABLE_DEVELOPER_COMMANDS` | 未设置 | 设为 `true` 才向小智开放 `command_execution_tool`，仅供本机调试 |

## 4. 运行目录

数据不写入程序目录。运行目录按以下顺序确定：显式指定 → 可执行文件同级的 `.runtime/` → `%APPDATA%\moss` → 当前目录的 `.runtime/`。桌面端与 `moss-host` 都使用可执行文件同级的 `.runtime/`。

目录内包含 `uploads/`、`previews/`、`generated_assets/`、数据库 `moss.sqlite`、界面设置 `settings.json` 和实例锁 `moss.lock`。同一运行目录同时只允许一个实例，第二个实例会因为拿不到锁而初始化失败。

从旧 Python 版迁移数据：

```powershell
cd apps/moss-host
cargo run -- migrate --from <旧项目根目录> [--target <运行目录>] [--dry-run] [--on-conflict keep|overwrite]
```

迁移报告写入运行目录的 `migration-<时间戳>.json`；旧的未完成任务一律标为失败，不会自动续跑。

## 5. 界面

| 路径 | 用途 |
| --- | --- |
| `/` 主页 | 选择串口或网络连接（串口列表只显示系统真实识别到的串口；网络可点"扫描局域网"用 mDNS 发现 FluidNC）、文字雕刻任务、发送进度条、`0x18` 软件复位按钮、通信终端与小智桥接状态抽屉 |
| `/draw` 画板 | Excalidraw 手绘或导入图片，生成预览，"走边框"校位，确认发送；可勾选 SD 卡脱机模式 |
| `/material-lab` 材料实验室 | 材料库增删改、导入导出，生成并发送测试矩阵，选出最佳格写回材料库 |

桌面端关闭窗口时会询问"最小化到托盘"或"退出"，可记住选择。退出、崩溃或关闭控制台都会先向在途作业写入 `0x18` 再退出。

## 6. 第一次雕刻

1. 生成预览。预览只解算 G-code 并冻结文件 SHA-256 与加工参数，不连接机器。
2. 核对机器上实际放置的材料与厚度和预览一致。系统不会自动识别真实材料。
3. 在画板点"走边框"。这一步会真实驱动机器沿图形外接矩形运动，默认使用极低功率的光斑，用来校对摆放位置。
4. 确认发送。发送前会重新计算文件 SHA-256、与机器握手（要求 `$32=1`、读取 `$30` 功率标尺）、确认机器处于 Idle 且无急停/故障/门开信号；任一项不通过都不会写入加工指令。
5. 进度 100% 只表示所有行已被机器接收。系统随后发送 `G4 P0` 同步并等待 Idle，状态变为 `completed` 才算完成。
6. 中途要停：主页的红色 `0x18` 按钮是软件复位请求，不是物理急停。机器上的硬件急停按钮仍然必须可及。

SD 卡脱机模式：确认发送后文件先经 FluidNC 的 HTTP 上传到 SD 卡，比对固件计算的哈希后执行 `$SD/Run`，进度来自固件状态报告。固件未启用 HashFS 时仍会执行，但结果标记 `sd_hash_verified=false` 并给出警告。脱机作业进行中，机器只接受急停、暂停、继续等实时字节。

## 7. 手机或局域网访问

把 `LASER_WEB_HOST` 设为 `0.0.0.0` 并重启后，其他设备可以打开页面。非本机访问的数据接口需要一次性配对：

1. 在宿主机本机执行下面的请求获取 6 位配对码，配对码 5 分钟内有效。该接口只允许本机回环地址调用。

   ```powershell
   curl -X POST http://127.0.0.1:9666/v1/auth/challenge
   ```

2. 在手机端页面弹出的"局域网访问配对认证"窗口输入配对码。配对成功后浏览器保存 token。
3. token 只保存在宿主进程内存中，宿主重启后需要重新配对。连续输错会被临时锁定。

不要把服务暴露到公网。跨网访问必须由前置代理终结 TLS。

## 8. 小智语音

在 `.env` 写入 `MCP_ENDPOINT` 后，桌面端和 `moss-host serve` 启动时会自动连接小智，断线按 1 秒到 60 秒指数退避重连，日志中的 token 会脱敏。主页顶栏的"小智"胶囊与控制台抽屉的"小智 MCP 桥接状态"页显示连接状态和最近请求。

小智控制台的提示词与知识库文本以 [integration/xiaozhi-prompts/latest.md](../integration/xiaozhi-prompts/latest.md) 为准。语音只能触发预览、状态查询、受控安全动作和已确认的发送，底层门禁与网页相同。

## 9. 常见问题

- **串口列表为空**：列表只显示系统识别到的串口，插好设备后点右侧刷新；也可以选择"手动输入串口"。
- **握手报 `$32=1 固件激光模式门禁未通过`**：固件未启用激光模式，按 [FluidNC 固件侧配置](../laser/fluidnc-setup.md) 在 `config.yaml` 写 `Laser:` 段。
- **提示"机器正在执行 SD 脱机作业"**：固件会丢弃其他通道的顺序指令，先急停再操作。
- **报 `error:162 Read-only setting`**：FluidNC 的 `$30`、`$32`、`$100` 等是只读代理，改 `config.yaml` 后重启，上位机不会替你改。
- **端口被占用**：看控制台打印的实际端口，或在偏好设置里换端口。
- **页面提示需要配对码**：说明你不是从宿主机本机访问，按第 7 节获取配对码。
