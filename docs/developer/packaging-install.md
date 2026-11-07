# 安装包、安装目录与卸载规范

状态：通用 NSIS 已实施（2026-10-06，见 §2a），用户本机完成当时主程序安装 / 卸载测试；2026-10-10 新增 helper EXE sidecar 配置/脚本接线，源码/许可资源及新安装包验收仍待 NV18（见 §2b）。签名与干净虚拟机验收未做。本文定义安装、升级与卸载行为。与融合方案的关系见 [融合技术书](../integration/moss-desktop-fusion-technical-spec.md) §9.6。


## 0. 交付产物检查

阶段收尾构建后逐项确认文件为本次生成且非空，把时间、大小、SHA-256 写入 [构建回执](build-receipts.md)：

| 产物 | 路径 | 构建方式 |
| --- | --- | --- |
| Windows 主程序 | `target/release/moss-desktop.exe` | `cargo tauri build` |
| 普通 NSIS（不含 helper） | `target/release/bundle/nsis/MOSS_<版本>_x64-setup.exe` | 同上 |
| 带 helper 专用 NSIS | `target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_<版本>_x64-setup.exe` 及 `target/nv18-<构建ID>/release-manifest.json` | `scripts/build-nv18-sidecar-safe.ps1`（见 §2b、§2c） |
| Android APK | `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/` | `cargo tauri android build --target aarch64 --apk` |

普通 NSIS 和专用 NSIS 分别记录，普通包不能代表 helper 安装验收；只改动与 helper 无关时可不重建专用包，但要写明。

## 1. 目标

像市面上常见的 Windows 应用一样：

| 编号 | 要求 |
| --- | --- |
| I-1 | 双击一个安装程序（`MOSS_<版本>_x64-setup.exe`）完成安装，不要求用户装 Rust、Node 或命令行工具 |
| I-2 | 安装时**可以自定义安装目录**；默认目录合理（见 §3） |
| I-3 | 可选创建桌面快捷方式；开始菜单有 MOSS 文件夹 |
| I-4 | 在“设置 → 应用”/“控制面板 → 程序和功能”中可以卸载，显示名称、版本、发布者和图标 |
| I-5 | 卸载时可选择**保留或删除用户数据**（材料库、任务记录、设置）；默认保留 |
| I-6 | 覆盖安装新版本即可升级，用户数据不丢失 |
| I-7 | 安装、升级、卸载前若 MOSS 正在运行，先提示关闭；正在加工时不得被强行结束 |
| I-8 | 缺少 WebView2 时自动安装 |
| I-9 | Android 继续使用标准 APK 安装与系统卸载（系统不支持自定义安装目录） |

## 2. 现状核实（2026-10-05 读代码）

| 项 | 代码事实 | 对安装包的影响 |
| --- | --- | --- |
| 打包开关 | `tauri.conf.json`：`bundle.active=false`，无 `windows.nsis` 配置；`identifier=com.moss.laser`，`productName=MOSS`，`version=0.1.0` | 需开启打包并补全安装器配置 |
| 运行数据目录 | `laser-host` 调用 `RuntimeRoot::resolve(None, None)`，默认目录由 `RuntimeRoot::default_base()` 决定：Windows 为 `%APPDATA%\moss`，macOS 为 `~/Library/Application Support/moss`，Linux 为 `$XDG_DATA_HOME/moss` 或 `~/.local/share/moss`，均缺失时退回当前目录 `.runtime/`；`moss-host migrate` 不写 `--target` 时也用它 | **各桌面系统实际写入当前用户的应用数据目录，不在程序目录旁**。安装到 `C:\Program Files` 或 `/Applications/MOSS.app` 都不需要程序目录写权限，Finder 双击启动不会因只读 `/` 失败，覆盖升级也不会碰到用户数据 |
| `.env` 读取 | `config::load_env_files`：先读进程环境变量，再读**启动时当前目录**（向上查找）的 `.env`，最后读**运行目录**的 `.env`（Windows 为 `%APPDATA%\moss\.env`）；先加载的优先，后者不覆盖前者 | 已修复（PKG-02）：快捷方式启动时当前目录不确定也不影响，运行目录里的 `.env` 一定能读到。`settings.json` 的 `web_port`、`mcp_endpoint` 照常可用 |
| 实例锁 | 运行目录下 `moss.lock`（`fs4` 独占锁） | 安装器可据此判断是否有实例在运行 |
| 语音中心外部目录 | 技术书 §9.4：`xiaozhi-server`、`mcp-endpoint-server` 源码目录由用户自备，路径存于 `settings.json` | **卸载不得删除**这些外部目录；只删除 MOSS 自己的程序文件和（用户同意时）运行数据 |
| Android | APK 安装；运行目录在没有 `APPDATA` 时退回当前目录 `.runtime/` | Android 上实际可写位置需实机核对（建议改用应用私有目录），不在本规范的安装器范围内 |

## 2a. 实施状态（2026-10-06）

| 项 | 状态 |
| --- | --- |
| PKG-01 打包配置 | 已写入 `apps/desktop/src-tauri/tauri.conf.json`：`bundle.active=true`、`targets=["nsis", "app", "dmg", "deb", "appimage"]`（Tauri 按当前编译宿主自动筛选目标，Windows 仍只构建 `nsis`）、`icon` 含 `icon.png` / `icon.icns` / `icon.ico`、`macOS` 配置 `minimumSystemVersion="11.0"`、`signingIdentity="-"`（`hardenedRuntime=false`，确保 Tauri 二进制补丁后在 Apple Silicon 上完成内向外 ad-hoc 重签名；正式证书可通过 `APPLE_SIGNING_IDENTITY` 覆盖并开启 `hardenedRuntime=true`）与 `infoPlist="Info.plist"`（局域网 mDNS `_telnet._tcp` / `_http._tcp` 声明）；Windows NSIS 配 `installMode="both"`、简体中文 + 英文（不显示语言选择）、开始菜单文件夹 `MOSS`、`embedBootstrapper`、安装器图标与横幅 / 侧栏图、`installerHooks`。macOS 提供 `scripts/build-macos-installer.sh`，**默认**编译并内置 `moss-voice-local` sidecar、GPL 源码/许可证资源与 `release-manifest.json`（仅调试裸包时传 `--without-voice-sidecar`，待 Mac 实机验收）。`publisher`、`copyright` 未填写（等用户提供真实信息，不预填） |
| PKG-02 安装版配置读取 | 已实施（见 §2 `.env` 行） |
| PKG-03 安装/卸载钩子 | 已写 `apps/desktop/src-tauri/installer/hooks.nsh`：安装与卸载前用 `nsis_tauri_utils::FindProcess` 检测 `moss-desktop.exe`、`moss-host.exe` 与本地模型辅助程序 `moss-voice-local.exe`（2026-10-13 补），运行中只提示退出并取消。钩子位于 Tauri 模板 `CheckIfAppIsRunning` 之前，后者“询问后强行结束”的流程不会被触发。卸载时复用模板的“删除应用程序数据”复选框（`$DeleteAppDataCheckboxState`，默认不勾选），勾选且非更新模式时删除固定路径 `%APPDATA%\moss`（`APPDATA` 为空则跳过）。依据 Tauri `dev` 分支 `installer.nsi` / `utils.nsh` 核对；以用户本机 tauri-bundler 实际版本打包结果为准 |
| PKG-04 安装器图片 | 已生成 `installer/header.bmp`（150×57）与 `installer/sidebar.bmp`（164×314），曜石配色 + MOSS 红眼 |
| PKG-05 文档 | 快速开始“安装与卸载”已改为实际步骤；安装包产物检查见 §0 |
| PKG-06 签名 / PKG-07 干净虚拟机验收 | 未开始 |
| 导出目录（技术书 §9.7） | 卸载**永不删除**用户在偏好设置里选的导出目录及其中文件，即使勾选删除应用程序数据；钩子只删固定路径 `%APPDATA%\moss`，`settings.json` 中的 `export` 设置随之删除 |

## 2b. 内置语音辅助程序（NV18，2026-10-10）

详细执行步骤、资源目录、版本/hash 清单及验收见 [NV18 实施细则](../integration/moss-native-voice-implementation.md#nv18)；该部分不由 2026-10-06 的主程序安装记录替代。

| 项 | 当前事实与完成条件 |
| --- | --- |
| helper EXE 收集 | `scripts/build-windows-installer.ps1` 已独立构建 Windows helper，复制为 `binaries/moss-voice-local-x86_64-pc-windows-msvc.exe`；overlay `tauri.voice-sidecar.conf.json` 已声明 externalBin。配置/脚本存在不证明成品含该文件 |
| 常规与专用构建 | 常规 `cargo tauri build` 不自动构建/收集独立 GPL helper；带 helper NSIS 必须走专用脚本和 overlay。专用脚本已统一将 helper 输出写入根 `target/<triple>/release`，但原项目要求的 `target/release` 常规发布构建仍独立验收；专用 triple 产物与项目要求的 `target/release/moss-desktop.exe` 常规产物分别记录 |
| 安装位置与启动 | 成品应将 `moss-voice-local.exe` 放在 `moss-desktop.exe` 同目录，宿主按 current_exe 父目录寻找并做 hello 协议校验；不回退任意 PATH 同名程序。`apps/moss-voice-local/build.rs`（以及独立后端 `apps/moss-host/build.rs`）在 Windows MSVC 下与 `moss-desktop.exe` 一致静态链接 `libvcruntime` / `libcmt`，干净 Windows 新机无需预装 Visual C++ 运行库即可启动；老版 Win10 缺 WebView2 时由 `tauri.conf.json` 的 `embedBootstrapper` 在安装期间自动联网下载补齐。当前协议 4 |
| 源码与许可 | 脚本已暂存 helper 源码、path 依赖 `moss-voice-local-proto`、Cargo.lock、COPYING 和初版 THIRD_PARTY_NOTICES；overlay 已配置 `bundle.resources` 至 `licenses/voice-local/`。**上游 sherpa-onnx/espeak-ng 等静态依赖的完整分发所需对应源码/许可证和洁净机安装内资源验收尚待完成**，不能把初版 notices 当成合规证明 |
| 成品验证 | 脚本新增发布清单生成，但本次构建进程状态丢失时清单尚未产出，不能把新 NSIS 文件出现视为构建通过；须复测同一构建的产物与源码/许可资源。后续补组件版本、协议、schema、大小/hash；解包检查 helper/资源并验证源码构建；经授权在干净系统完成新安装、升级、错误 helper、正常退出和卸载。缺这类证据仍为 partial/unverified |

升级不得把桌面新协议与旧 helper 混装；当前可明确诊断缺 helper 与协议不符，架构错误按进程启动失败呈现，helper 文件 hash 校验及架构专用诊断属于 NV18 待实现/待验收项。数据库升级失败保留原始数据，软件回退先验证旧版支持的 schema 或恢复兼容备份，不能把旧 v4 兼容记录推广到 NV10 以后的所有迁移。卸载默认保留运行数据，用户自选音乐、Agent、外部服务和导出目录仍不属于安装器删除范围。

## 2c. NV18 专用安装包的软件验证（2026-10-11）

- 旧版构建脚本会删除整个未跟踪 `bundle-resources/voice-local/`，现改为调用 `scripts/build-nv18-sidecar-safe.ps1`。后者为每次构建生成唯一 `nv18-<构建ID>` 暂存目录及资源映射，不覆盖原工作区资源。独立 helper、Tauri 带 sidecar NSIS 及完整版本/大小/hash manifest 已成功生成，详细 SHA 见 CHANGELOG。
- 通过 `scripts/verify-nv18-nsis-in-docker.ps1` 在无网络、只读的临时 Docker Linux 环境中验证实际 NSIS 的 22 条 archive 目录，明确看见 `moss-voice-local.exe` 与其 COPYING/初步第三方说明/源码。检查脚本不会运行 installer 或连接实际设备；测试专用 Docker image 已清理。
- **分发阻塞**：尚未完成完整 sherpa-onnx/espeak-ng native 库许可和对应源码审核、干净系统安装/升级/卸载或 helper 协议启动实测。构建包仅供内部软件验收，不是可以对外分发的最终安装包。真正安装仍需用户授权，用户数据及自选音乐/Agent/导出路径不得由脚本删除。
完整 NSIS 内存抽取逐字节校验、独立源码复建与 147 个 crate 的许可证元数据盘点，另见 [NV18 源码和许可证审计（2026-10-11）](nv18-source-license-audit-2026-10-11.md)。**注意：嵌入 NSIS 的桌面程序有 Tauri 合法的3字节 `UNK`→`NSS` 标记补丁，不能把打包前与打包后 SHA-256 误记为相同；构建可用不等于 native 依赖许可已全部齐全。**

## 3. 方案

### 3.1 安装器类型

使用 Tauri 2 自带的 **NSIS 安装器**（`-setup.exe`），不使用 MSI：

- NSIS 安装器默认提供“选择安装位置”页面，满足 I-2；
- 支持当前用户 / 所有用户两种安装范围、多语言、自定义图标与横幅、安装/卸载钩子（`installerHooks`）；
- MSI（WiX）依赖 VBScript 可选组件，且定制卸载行为更繁琐，暂不提供。

### 3.2 安装范围与默认目录

| 配置 `bundle.windows.nsis.installMode` | 默认目录 | 是否需要管理员 | 结论 |
| --- | --- | --- | --- |
| `"perUser"`（Tauri 默认） | `%LOCALAPPDATA%\MOSS` | 否 | 普通用户无需管理员即可安装 |
| `"perMachine"` | `C:\Program Files\MOSS` | 是 | 多用户共用一台电脑时适用 |
| `"both"` | 安装时由用户选择 | 是 | **推荐**：用户先选“只为我安装 / 为所有用户安装”，再在下一页**自定义目录** |

运行数据始终在当前 Windows 用户的 `%APPDATA%\moss`，与安装目录无关；“为所有用户安装”时每个 Windows 用户各有一份自己的数据。

### 3.3 建议配置（实施时写入 `tauri.conf.json`，以 Tauri 当时版本的配置结构为准）

```json
{
  "bundle": {
    "active": true,
    "targets": ["nsis"],
    "publisher": "<发布者名称>",
    "copyright": "<版权信息>",
    "shortDescription": "MOSS 激光雕刻上位机",
    "icon": ["icons/icon.png", "icons/icon.ico"],
    "windows": {
      "webviewInstallMode": { "type": "embedBootstrapper" },
      "nsis": {
        "installMode": "both",
        "languages": ["SimpChinese", "English"],
        "displayLanguageSelector": false,
        "installerIcon": "icons/icon.ico",
        "headerImage": "installer/header.bmp",
        "sidebarImage": "installer/sidebar.bmp",
        "startMenuFolder": "MOSS",
        "installerHooks": "installer/hooks.nsh"
      }
    }
  }
}
```

- `webviewInstallMode` 选 `embedBootstrapper`（约 +1.8 MB，联网时自动装 WebView2）；需要离线安装时改 `offlineInstaller`（约 +127 MB），另出一个离线版安装包。
- `publisher`、`copyright` 用真实发布者信息，不在文档中预填。
- 横幅与侧栏图片放 `apps/desktop/src-tauri/installer/`，按新前端视觉（§9.5.5）制作。

### 3.4 安装流程（用户视角）

1. 语言（默认简体中文）→ 2. 安装范围（只为我 / 所有用户）→ 3. **选择安装目录**（可改）→ 4. 是否创建桌面快捷方式 → 5. 安装 → 6. 完成页“立即运行 MOSS”。

安装前检查：按进程名检测 `moss-desktop.exe`、`moss-host.exe`、`moss-voice-local.exe`，任一在运行即提示“请先退出 MOSS（托盘图标右键 → 退出）”并取消，不强行结束进程（I-7）。实例锁 `moss.lock` 由程序自身使用，安装器不读取它。

### 3.5 卸载流程

入口：开始菜单“卸载 MOSS”、Windows“设置 → 应用”、安装目录下的 `uninstall.exe`。

| 步骤 | 行为 |
| --- | --- |
| 1. 运行检查 | MOSS 正在运行时提示先退出；**不得强行结束正在加工的进程**（强杀会跳过 `ShutdownCoordinator` 的急停顺序） |
| 2. 数据选项 | 复选框“同时删除我的数据（材料库、任务记录、设置、上传文件）”，**默认不勾选** |
| 3. 删除程序 | 删除安装目录下的程序文件、快捷方式、开始菜单项、卸载注册表项 |
| 4. 删除数据（仅勾选时） | 删除 `%APPDATA%\moss`（含 `moss.sqlite`、`settings.json`、`.env`、`uploads/`、`previews/`、`generated_assets/`、`logs/`、迁移报告）；钩子只删除固定路径 `$APPDATA\moss`，`APPDATA` 为空时跳过，不拼接其他来源的路径，避免误删 |
| 5. 不删除 | 用户自备的 `xiaozhi-server`、`mcp-endpoint-server` 目录；用户导出的材料库 JSON；`LASER_MATERIAL_PARAMS_FILE` 指向的外部文件；小智设备与激光机上的任何配置 |

说明：Tauri 卸载器自带的“删除应用数据”选项针对的是以 `identifier`（`com.moss.laser`）命名的目录，**覆盖不到 MOSS 实际使用的 `%APPDATA%\moss`**，因此数据删除必须在 `installer/hooks.nsh` 的卸载钩子中显式实现（实施时以实际 Tauri 版本的卸载器行为为准再核对一次）。

### 3.6 升级与本机快速同步

- 新版本安装包直接覆盖安装：沿用上次的安装范围与目录，只替换程序文件，`%APPDATA%\moss` 不动。
- **日常每次代码改动后的正式版快速同步（`scripts/quick-update-desktop.ps1` / `scripts\start.bat -Update`，2026-11-01 用户决定）**：每次修复问题或新增功能通过测试后，默认执行 `powershell -File scripts/quick-update-desktop.ps1`（内部跑 `cargo tauri build --no-bundle`，未装 `cargo-tauri` 时回退 `npm run build` + `cargo build --release -p moss-desktop --features tauri/custom-protocol`；默认取 `tauri.conf.json` 与本地最新 `vX.Y.Z` 正式标签中的较新版本号写入 EXE 文件属性与 `MOSS_RELEASE_VERSION`，也可传 `-Version <ver>` 显式指定），直接产出最新正式版 `target/release/moss-desktop.exe`（已内嵌最新 `frontend/dist`；n67 起不再有单独的 `voice-private.html`），跳过 NSIS 压包与许可证暂存，并自动核对 `crates/moss-voice-local-proto/src/lib.rs` 的 `PROTOCOL_VERSION`，将协议匹配的 `moss-voice-local.exe` 同步到 `target/release/`（以及已存在的 `target/debug/`；本机尚无匹配协议产物时自动重编 `apps/moss-voice-local`）。若检测到本机已有安装目录（注册表或 `%LOCALAPPDATA%\MOSS` / `%ProgramFiles%\MOSS`，也可传 `-InstallDir <path>`），自动原地覆盖 `moss-desktop.exe`（若安装目录已含 `moss-voice-local.exe` 或传了 `-WithVoiceHelper` 也一并同步）并核对 SHA-256；若目标程序正在运行则绝不强杀（严守 I-7），自动将运行中的旧文件同目录改名为 `*.old-<时间戳>.exe` 挪开并写入新版（下次运行时自动清理已释放的旧文件），用户关掉重开即生效。涉及离线语音辅助程序内部实现（协议号未变）或独立宿主改动时追加 `-WithVoiceHelper`、`-WithHost`。阶段收尾与对外交付仍须按 §0 执行完整安装包构建。
- **macOS 本机快速同步（`scripts/quick-update-desktop.sh`，2026-11-05）**：与 Windows 脚本同参数语义（`--build-only` / `--install-dir <MOSS.app>` / `--with-voice-helper` / `--with-host` / `--launch` / `--skip-cache-sweep`，另有 `--dry-run` 与 `--version`）。跑 `cargo tauri build --no-bundle` 后原地替换 `/Applications/MOSS.app`（或 `~/Applications`）里的 `Contents/MacOS/moss-desktop`，语音辅助程序协议号不符或带 `--with-voice-helper` 时一并重编并替换 `moss-voice-local`。替换方式是先在同目录写临时文件并 ad-hoc 签名、再原子 `mv` 覆盖：运行中的 MOSS 继续使用旧文件，**从不结束进程**，退出重开即生效；默认取 `tauri.conf.json`、本地最新 `vX.Y.Z` 正式标签与已安装包 `CFBundleShortVersionString` 中的较新版本写入 `MOSS_RELEASE_VERSION` 并同步更新 `.app` 的 `Info.plist` 版本号，有替换或版本变更时重新签名整个 `.app` 并 `codesign --verify --deep --strict`。运行检测只比对进程可执行路径（`ps -o comm`），`--launch` 在 MOSS 已运行时拒绝。图标、许可证资源等非版本号资源仍需重新完整打包（§3.9）才更新。首次仍需从 DMG 安装一次。
- **桌面热重载开发窗口（`scripts/dev-desktop.ps1` / `scripts\start.bat -Dev`）**：自动确保 `target/debug/moss-voice-local.exe` 与当前 `PROTOCOL_VERSION` 对齐（`start.bat` 直接启动桌面版前同样核对同目录辅助程序协议号），拉起回环 Vite（`127.0.0.1:5177`）与 debug 版 `moss-desktop` 实时预览前端与语音中心，仅供开发调试，不替代正式版验证。
- 数据库 schema 由程序启动时自动迁移（`RuntimeDb` 只执行 `current < N` 的步骤）；降级到旧版本时旧程序也能打开新库（见技术书 §9.3）。
- **应用内更新与默认启动自动检查（2026-11-07 用户决定）**：`settings.json` 未显式设置 `auto_check_update` 时默认开启启动自动检查，发现新版后在主界面顶部显示不打断操作的更新提示条，点“立即更新”直接打开“偏好设置 → 软件更新”并填入检查结果；桌面版（Windows 安装版、macOS）可点“下载并重启更新”，Android 版可点“下载并安装更新”。实现见 `apps/desktop/src-tauri/src/app_update.rs` 与 `gen/android/app/src/main/java/com/moss/laser/MainActivity.kt`（`ApkInstaller`）：
  - 更新清单固定取 `https://github.com/yjdaaa/moss/releases/download/<标签>/latest.json`，标签只接受版本号字符；下载后用 `apps/desktop/src-tauri/updater-pubkey.txt` 里的公钥校验 minisign 签名，并强制核对签名 `trusted comment` 里记录的 `version:` 字段（`requireSignedVersion`，签名内版本必须与清单一致，防止旧版合法签名冒充新版）。
  - Windows 按主程序目录判断安装形态：有 `uninstall.exe` 且有 `moss-voice-local.exe` 取 `windows-x86_64-offline-voice`，只有 `uninstall.exe` 取 `windows-x86_64-lite`；没有 `uninstall.exe` 的免安装便携版不支持应用内更新。安装器以 `/P /UPDATE` 被动模式运行、完成后自动重启 MOSS；插件结束进程前先调用 `shutdown_desktop_services`（急停 → 停语音服务，与“完全退出”同序）。`installer/hooks.nsh` 在 `/UPDATE` 模式下最多等 15 秒让旧进程自己退出，仍未退出才提示并取消，绝不强杀。
  - macOS 用通用版 `MOSS.app.tar.gz` 原地替换 `.app`（`darwin-aarch64` / `darwin-x86_64` 两个键指向同一个包），安装后按完全退出的顺序收尾并重启。
  - Android 取 `android-aarch64`，下载上限 512 MiB，先把 APK 流式写入应用私有缓存目录 `cacheDir/updates/` 临时文件，在 Rust 侧完成带版本绑定的 minisign 签名校验后原子重命名为正式缓存文件，再经 JNI 调用 `ApkInstaller.installApk` 使用 `FileProvider` 拉起系统安装界面由用户确认覆盖升级（Android 8+ 首次未授权“安装未知应用”时自动打开本应用授权页，返回后再次点击直接复用已校验缓存包拉起安装器，无需重下）。
  - 设备正在下发、启动或停机待确认（`ApiService::device_work_in_progress`）时拒绝开始；下载完成、安装前再检查一次。命令 `app_update_status / app_update_install` 只认受信任主窗口（桌面回环后端来源或 Android 打包主窗口），updater 插件自带的前端命令不授权给任何页面。
  - **签名密钥**：正式 minisign 公钥已写入 `apps/desktop/src-tauri/updater-pubkey.txt`，并且必须同时写进 `tauri.conf.json` 的 `plugins.updater.pubkey`（同一行；tauri-cli 打更新包时用它核对签名私钥，为空会让三个桌面安装包全部失败，`app_update` 单测强制两处一致，2026-11-07）；私钥与密码仅保存在维护者本机 `~/.tauri/moss-updater.{key,pass}` 并注入 GitHub 仓库密钥 `TAURI_SIGNING_PRIVATE_KEY`、`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`，绝不入库。丢失私钥后已安装的版本无法再应用内更新，只能手动安装一次带新公钥的版本。
  - 只有配置了上述仓库密钥的发版才生成更新包（`createUpdaterArtifacts` 只在流水线覆盖配置里开启，本机快更与普通构建不生成），见 §7。在此之前发布的旧版本尚未内置该公钥，需手动覆盖安装一次首个带公钥的版本后，后续版本即可在软件内直接更新。

### 3.7 签名

未签名的安装包会被 Windows SmartScreen 提示“未知发布者”。正式分发前需要代码签名证书（OV / EV）对 `moss-desktop.exe` 与安装包签名；签名证书与密码不进仓库，由构建机本地或 CI 机密注入。首版内测可不签名，但需在下载页说明。

### 3.8 Android

- 保持 APK：首次从下载页获取安装，后续可在 App 内直接下载校验签名后的新版 APK 并拉起系统安装界面覆盖升级；系统卸载即删除应用与其私有数据；Android 不提供自定义安装目录。
- 正式分发与覆盖升级必须使用同一固定签名证书（Android 系统要求覆盖安装的 APK 签名证书一致）：维护者本机保存 `~/.tauri/moss-android-release.jks`（及 `.pass`），仓库密钥配置 `ANDROID_KEYSTORE_BASE64`、`ANDROID_KEY_ALIAS`、`ANDROID_KEY_PASSWORD`，本机调试通过已 gitignore 的 `gen/android/keystore.properties` 引用。此前由临时证书签名的旧版 APK 首次切换到固定正式证书时，需先卸载旧版再安装一次，之后所有新版均可原地保留数据覆盖升级。
- 运行目录位于应用私有数据目录 `app_data_dir`，更新包暂存在 `app_cache_dir/updates/`。

### 3.9 macOS 本机打包、安装与清理（2026-11-05）

**为什么会看到两个 MOSS**：`scripts/build-macos-installer.sh` 先生成 `target/<triple>/release/bundle/macos/MOSS.app`，再用它做成 `bundle/dmg/MOSS_<版本>_<arch>.dmg`。Spotlight 与启动台会收录磁盘上任何位置的 `.app`，所以装到 `/Applications` 后会同时看到“已安装的 MOSS”和“构建目录里的 MOSS”两个图标，两者是同一份程序，只有 `/Applications/MOSS.app` 才是安装件。打开测试一律用 `/Applications` 里的那个；构建目录里的 `.app` 只是做 DMG 的中间产物，每次构建都会重新生成。

**本机构建（与云端 Mac 版内容一致）**：

1. `native-license-evidence/`、`rust-license-evidence/` 未纳入 git，云端没有这两个目录；本机若存在，脚本会把约 760 MB 证据打进包里。构建前临时移出 `apps/moss-voice-local/`，构建后移回。
2. 设 `CI=true` 运行脚本：跳过 `bundle_dmg.sh` 用 AppleScript 让 Finder 排版窗口的步骤（未授予终端“控制 Finder”自动化权限时会报 `AppleEvent 已超时 (-1712)` 而失败）。
3. 需要与发布同号时用 `MOSS_VERSION_OVERRIDE=<X.Y.Z>` 注入版本（只改包与 `current_moss_version`，不改仓库文件；`moss-voice-local` 仍显示自身 crate 版本）；要支持 Intel 用 `MOSS_MACOS_TARGET=universal-apple-darwin`。

示例：`CI=true MOSS_VERSION_OVERRIDE=0.3.2 bash scripts/build-macos-installer.sh`

**日常改代码后不必重新打包**：首次从 DMG 安装后，用 `bash scripts/quick-update-desktop.sh` 原地打补丁（见 §3.6），约 1 分钟；只有改了图标、`Info.plist`、打包资源或要分发安装包时才重跑本节的完整构建。

**安装**：挂载 DMG，把 `MOSS.app` 拖到“应用程序”（命令行：`hdiutil attach -nobrowse -readonly <dmg>` → `ditto /Volumes/MOSS/MOSS.app /Applications/MOSS.app` → `hdiutil detach /Volumes/MOSS`）。本机构建的包没有隔离属性，可直接打开；从网上下载到其他 Mac 的包是 ad-hoc 签名、未公证，首次需右键“打开”。运行数据在 `~/Library/Application Support/moss`，卸载或覆盖安装 `.app` 都不会动它。

**清理（只保留最新）**：安装完成后可删除 `target/<triple>/release/bundle/macos/MOSS.app`（消除重复图标），以及 DMG 失败残留的 `bundle/macos/rw.*.dmg`。脚本每次构建都会新建唯一的暂存目录且从不删除（防止覆盖），会在 `target/nv18-<id>/`、`apps/desktop/src-tauri/binaries/nv18-<id>/`、`apps/desktop/src-tauri/bundle-resources/nv18-<id>/` 三处累积；构建结束后只需保留最新一组（其中 `release-manifest.json` 是该次构建的清单），旧的可直接删除，三处都已被 `.gitignore` 忽略。

## 4. 与其他功能的关系

- **语音中心**（技术书 §9.4）：安装包只含 MOSS 本体，不捆绑 Python 运行环境与 `xiaozhi-server`；语音中心的“服务端目录”仍由用户在向导中指定。卸载时不触碰这些目录，但若 MOSS 启动过服务端子进程，卸载前的“请先退出 MOSS”会连带按 §9.4.3 规则停止它们。
- **CORS 允许来源**（技术书 §6.5）：安装版 WebView 来源与开发版相同（`http://tauri.localhost`），无需额外配置。
- **端口**：安装版首次启动使用默认 9666；端口被占用时按现有规则回退，用户可在偏好设置修改（写入 `%APPDATA%\moss\settings.json`）。

## 5. 验收

| ID | 场景 | 期望 |
| --- | --- | --- |
| PK-01 | 全新电脑（无 Rust/Node）运行安装包 | 安装成功并能启动；缺 WebView2 时自动安装 |
| PK-02 | 自定义安装目录（含中文与空格路径，如 `D:\软件 工具\MOSS`） | 安装、启动、卸载均正常 |
| PK-03 | “只为我安装”与“为所有用户安装” | 前者无需管理员；后者需要管理员并装到所选目录；两种方式数据都在各自用户的 `%APPDATA%\moss` |
| PK-04 | 桌面与开始菜单快捷方式 | 均能启动；从快捷方式启动时端口、小智地址等设置能正确读取（不依赖当前目录下的 `.env`） |
| PK-05 | 系统“应用”列表 | 显示 MOSS、版本、发布者、图标；可从此处卸载 |
| PK-06 | 卸载默认选项 | 程序文件与快捷方式被删除，`%APPDATA%\moss` 保留；重装后材料库与任务记录仍在 |
| PK-07 | 卸载并勾选删除数据 | `%APPDATA%\moss` 被删除；外部 `xiaozhi-server` 目录、导出的 JSON 文件不受影响 |
| PK-08 | MOSS 运行中执行安装 / 升级 / 卸载 | 提示先退出，不强杀；加工中不得被中断 |
| PK-09 | 覆盖升级到新版本 | 设置、材料库、任务记录保留；数据库自动迁移 |
| PK-10 | 签名（正式版） | 安装包与 exe 带有效签名，SmartScreen 不提示未知发布者 |
| PK-11 | Android release APK | 签名后可安装、可系统卸载；运行目录位于应用私有目录 |

## 6. 实施要点（供实施清单引用）

1. 开启 `bundle.active`，`targets` 设为 `nsis`，补全 §3.3 配置与安装器图片。
2. 安装版配置读取：**已实施 (b)**——`LaserSettings` 额外读取运行目录的 `.env`（`%APPDATA%\moss\.env`），已记 CHANGELOG；界面设置 / `settings.json` 继续可用。后续若把默认连接方式、雕刻模式等也搬进界面设置（原方案 (a)），可在 UI-2 中再做，不影响本修复。`moss-host migrate` 不写 `--target` 时的默认目标也已改为同一运行目录。
3. 编写 `installer/hooks.nsh`：安装前与卸载前检测 `moss.lock`；卸载时按用户选择删除 `%APPDATA%\moss`，并做路径防误删校验。
4. 交付构建命令改为 `cargo tauri build`（产物增加 `target/release/bundle/nsis/MOSS_<版本>_x64-setup.exe`）；`AGENTS.md` 中 `target/release/moss-desktop.exe` 的检查保留，另增安装包检查。
5. 签名流程与证书管理（正式版）。
6. 同步文档：本文、[快速开始](../user/quick-start.md)（新增“安装与卸载”章节）、根目录 README 的安装说明、[架构总览](architecture.md) §6。

## 7. 类 Magpie 云端全自动四端并行发版工作流（2026-11-05）

对齐 `yetone/magpie` 的高频零阻塞发布架构，日常迭代与正式发版彻底解耦：

1. **本地 1 秒打标签脱手（`scripts/release-tag.ps1`）**：
   - 在工作区干净时运行 `powershell -File scripts/release-tag.ps1`（自动将最新 `vX.Y.Z` 补丁号 `+1`，也可显式传 `-Version 0.4.0` 或 `-DryRun` 预览），仅耗时约 1 秒即完成分支与标签推送，无需在本机排队串行编译、无需改 18 个 `Cargo.toml` 版本号使全量缓存失效。
2. **云端单次前端构建 + 四台机器并行编译（`.github/workflows/release.yml`）**：
   - `prepare` 仅执行 **1 次** `npm ci && npm run build` 产出共享 `release-frontend-dist`，并从 Git 标签提取语义化版本号注入各端 Tauri overlay（避免重复执行 4 次 4007 模块 Vite 构建）；
   - `windows-lite`（NSIS 轻量包 + `MOSS_<版本>_windows_x64_portable.exe` 免安装单文件便携版）、`windows-offline-voice`（带 `moss-voice-local.exe` 的瘦身离线语音安装包 + 独立 `moss-voice-local-<版本>-gpl-source.zip`）、`macos-universal`（Apple Silicon + Intel 通用版 `.dmg`）与 `android-apk`（`-Dorg.gradle.daemon=false --ci` 签名 `arm64` APK）在云端 **同时并行构建**；
   - `publish` 汇总全部产物，自动生成 `SHA256SUMS.txt` 与中文变更说明并发布至 GitHub Releases。
   - **应用内更新包（2026-11-07）**：配置了仓库密钥 `TAURI_SIGNING_PRIVATE_KEY`（及密码）时，两个 Windows 安装包、macOS `MOSS_<版本>_macos_universal.app.tar.gz` 与 Android `MOSS_<版本>_android_aarch64.apk` 均以 `--app-version` 生成带版本绑定的 `.sig`；`publish` 在桌面三份签名齐全时写出 `latest.json`（平台键 `windows-x86_64-lite` / `windows-x86_64-offline-voice` / `darwin-aarch64` / `darwin-x86_64`，Android 签名就绪时一并写入 `android-aarch64`）并随 `.app.tar.gz`、`.apk` 一起上传；签名只写进清单，不单独上传。缺桌面签名就跳过清单（流水线提示 notice），普通安装包照常发布。详见 §3.6、§3.8。
3. **安装包瘦身（`scripts/build-nv18-sidecar-safe.ps1`）**：
   - 默认将 `moss-voice-local` 与 `moss-voice-local-proto` 完整源码、许可证及上游哈希清单随包打包，并通过 `-EmitGplSourceZip` 导出独立源码压缩包，不再每次将 759 MB 的上游静态库原始归档重复压入 NSIS `.exe`（安装包体积由 ~702 MB 降至 ~35 MB；如需生成包含全量原始归档的内部审计包可显式传 `-IncludeFullNativeEvidence`）。
