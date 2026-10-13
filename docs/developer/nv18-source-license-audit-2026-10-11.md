# NV18 helper 对应源码、发布工件与许可证初审（2026-10-11）

> **内部软件验收报告，非外部分发许可意见。** 只涉及本地代码、安装包和公开 Rust crates；未运行安装器、模型、硬件、登录或用户音频。

## 实际构建工件

| 工件 | 字节数 | SHA-256 |
| --- | ---: | --- |
| Windows 标准 `target/release/moss-desktop.exe` | 57,093,632 | `876515ab899bd4082261ad041404d478a634e16e78de48228960bfc2b1e7da1c` |
| Windows 标准 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 18,086,745 | `1a2dcc6faad1e5b7add1d88c3018dd381303aade82fd227a0b8094d2e3f24667` |
| Windows **专用 sidecar** `target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 23,624,916 | `5a662f052d95992f1089eb88f943fa193304bf462faf85c35d79c4d14892e036` |
| 专用 helper `moss-voice-local.exe`（包内实际字节） | 20,732,928 | `d8f8db26f10f3df26d205f427470451333cb714e13e68aefd4ae1c75577fa4b2` |
| Android arm64 release APK | 51,840,922 | `e8ab2693e08e3a86095843b11e4a5f69a6b29618246355718c4faf19cc48de3f` |

Android APK 内 `lib/arm64-v8a/libmoss_desktop_lib.so` 与本次已编译 Rust so SHA-256 **都为** `eee05d31a9aa94f3e20e32b0f4c444b2f451fc7598ce75337997ec6c84dc4571`。

专用发布清单：`target/nv18-24d485ff5a9f4bfb8e4d7c958dbb6fa6/release-manifest.json`，记录产品 0.1.0、helper 协议 3、数据库 schema 8、暂存对应源码 SHA-256 `fcfd47fc431ef5c4bb9e4ce7bae6b2e80c61d9867de8c784bc6c64a30839cbf3`，以及构建产物大小/哈希。

## NSIS 独立只读检查

- `scripts/verify-nv18-nsis-in-docker.ps1`：Ubuntu 官方 GPG 签名源 `p7zip-full`，仅在临时 Docker 镜像内安装，实际检查容器 `--network none --read-only --cap-drop ALL --security-opt no-new-privileges --user 65534:65534`。独立列出 NSIS 22 个 archive 条目，必须包含主程序、helper、COPYING、THIRD_PARTY_NOTICES、Cargo.lock、helper main.rs、协议 lib.rs；不允许把 .env/密钥/数据库/编译 target 缓存打包。
- 从 NSIS 内存解压为字节流并重新计算 SHA-256：helper 与清单 **完全一致**。桌面 EXE 与构建目录的 EXE 大小均为 **57,088,512** 字节，但包内 hash 为 `5ee249705fa5fdb99526a40fd8f01597b1dcc5064e63815f99d59900cfad6095`，源码目录对应的未打包 hash 为 `391023deda99f3a668c52502725c5ad538fc6cfcf0000fb562ffe893fc36a23b`。
- 通过 `cmp -l` 逐字节比对，**只差连续 3 字节**，位置在可确认的 Tauri 标记 `__TAURI_BUNDLE_TYPE_VAR_UNK`。NSIS 将 `UNK` 改为 `NSS`；在内存中仅按此规则修改原 EXE 后重新计算**完整 SHA-256**，与包内 hash 完全相同。其余字节一致，严格校验已通过，不应把 Tauri 合法 bundle-type patch 误报为二进制被篡改。不得把上述两个不同 hash 记为同一个文件。
- 未打开 GUI、未执行任何 Windows 安装程序；临时镜像、容器及文件已清理。

## Helper 对应源码可构建性

- 新的无损 `scripts/build-nv18-sidecar-safe.ps1` 按 GUID 隔离 staging，保留原先未跟踪 `bundle-resources/voice-local`，旧 `build-windows-installer.ps1` 入口已改为转发安全脚本，旧递归删除不再被触达。
- 安装包内对应源码包括 helper crate Cargo.toml、Cargo.lock、src、COPYING/README、协议 crate Cargo.toml/src、根 Cargo.lock 及初步第三方 notices。Cargo `metadata --offline --no-deps --locked` 成功；缓存初缺 `arbitrary 1.5.0`，补齐公开 crates 后，**完整** `cargo metadata --offline --locked` 也成功。
- 从**安装资源中的源码快照**单独执行 `cargo build --release --locked --offline --target x86_64-pc-windows-msvc` 成功，重新编译了 helper 和协议 crate。重新构建 helper 大小同为 **20,732,928** 字节、SHA-256 `71ca860a1aba08777eca56b5a28c0e7e7e35c8711fb53b32d177dc61fe1b1de5`，与正式打包的 `d8f8...` 不一致。**对应源码能编译 ≠ 完全可重现的逐字节相同构建**；后者仍未通过，不能推断差异原因。

## 第三方许可证初审结果及未解决问题

对**打包对应源码的 Cargo.lock**进行完整 Cargo metadata：**147 个 Rust packages，147 个有非空 `license` 声明，0 个缺声明**。关键 crate：`moss-voice-local` GPL-3.0-or-later、`moss-voice-local-proto` MIT、`sherpa-onnx` 1.13.8 Apache-2.0。常见声明包括 MIT OR Apache-2.0（81 项）、Unicode-3.0（18 项）、MIT（9 项），以及 BSD-3-Clause、ISC、其他 SPDX 复合许可证。

**未通过且需要在对外分发前关闭：**

1. 上述 Rust crate `license` 字段只是元数据；还需要对应分发方式下要求的完整许可证文本、版权/Notice 汇总，不可仅凭 SPDX 字符串宣称合规。
2. `sherpa-onnx` 的 native 静态包可能含 `espeak-ng`（GPL-3.0）和其他静态/转授权组件。须确认**本次确切 native 二进制版本、来源、许可证、对应原生源码及 build scripts**，再决定 GPL 相应源码提供义务。现在的 `THIRD_PARTY_NOTICES.md` 明确为 preliminary。
3. 干净 Windows 环境的安装、helper 启动/协议、旧版本升级/数据库迁移、卸载与文件保留未实测。安装文件内容核查不能代替安装验收；代码签名和外部分发需要另行授权。

## 2026-10-11 新增只读原生依赖许可证门禁（仍未通过）

从本机 Cargo 缓存核实 `sherpa-onnx-sys 1.13.8` 的真实 `build.rs`：Windows x64 静态链接表列出 13 个目标：`sherpa-onnx-c-api`、`sherpa-onnx-core`、`kaldi-decoder-core`、`sherpa-onnx-kaldifst-core`、`sherpa-onnx-fstfar`、`sherpa-onnx-fst`、`kaldi-native-fbank-core`、`kissfft-float`、`piper_phonemize`、`espeak-ng`、`ucd`、`onnxruntime`、`ssentencepiece_core`。源码中的预编译下载地址为 `https://github.com/k2-fsa/sherpa-onnx/releases/download/v1.13.8/...`。crate 的 Apache-2.0 声明**不能自动覆盖**全部静态链接组件。

实际本地预编译 archive `sherpa-onnx-v1.13.8-win-x64-static-MT-Release-lib.tar.bz2` **123,206,268 字节**、SHA-256 `56ffcf3c454c1f14f7bc9887286cc8143e7e542dc632804e1c447d5f8d534eaf`。解压目录有 **14 个 `.lib`**（上述 13 个及 `sherpa-onnx-cxx-api.lib`），按 LICENSE/COPYING/NOTICE/THIRD_PARTY 开头查找 **0 个许可证文本**；现有 `apps/moss-voice-local/THIRD_PARTY_NOTICES.md` 未逐项列出其中 **12 个**被静态链接的组件（只匹配 `espeak-ng` 名称）。这是本地所用包的事实，**不代表上游所有版本缺许可证**。

新增只读 `scripts/audit-nv18-native-licenses.ps1`：普通运行只输出证据，不写入文件。`-RequireComplete` 严格核对 archive 大小/hash、13 项静态链接库、原生许可证文本、Notice，以及人工审核清单中相应原生源码和许可证文件的**实际路径、SHA-256、非重解析点**。只写一个 JSON 哈希清单但不提供对应文件不能解锁。实测 **14 项阻塞**、`NV18_NATIVE_LICENSE_RELEASE_READY=False`，严格模式按预期拒绝（fail-closed）。

后续需收集相应原生组件的实际许可证/版权 Notice/构建说明/对应源码并逐项审查；即便软件证据齐全，对外分发的 GPL 等法律义务仍需相应审核。**现阶段不得宣称专用 NSIS 已具备外部分发许可合规性。** 本轮没有运行安装器、修改系统或控制设备；新增的只是审计脚本和文档，因此按 AGENTS.md 的测试/文档例外无需重新构建 Windows/Android。
**截至本记录**：NV18 软件构建、包内二进制一致性和对应源码可编译均通过；**尚未获得外部分发/许可合规/干净机安装/设备听音的通过结论**。原有项目未提交文件未提交、未丢弃、未重置。

## 本轮续做：实际原生与Rust源码/许可收集

新增 `scripts/collect-nv18-native-sources.ps1`，从固定上游CMake的公开来源实际下载10份归档，覆盖13个静态库。9份已有固定hash的归档均匹配；ORT官方v1.28.2源码实际278,677,335字节、SHA-256 `a5003dfd85d66a4d45bd2757c7f813f9e835142e9a936ac04877c788d6afe938`，作为候选源码记录，不能把二进制归档hash当源码hash。补充NOTICE逐库映射，提取真实LICENSE/COPYING/Unicode/uni-algo/ORT第三方文本及hash，落在helper的 `native-license-evidence/`，清单为 `native-source-evidence.json`。

新增 `scripts/collect-nv18-rust-sources.mjs`，按helper当前Cargo.lock下载并验证146个registry源码归档的锁定checksum，初次按常规文件名提取277份实际许可/Notice文本，清单为 `rust-source-evidence.json`。初次扫描漏掉`r-efi` 5.3.0、6.0.0的AUTHORS许可正文，曾记为缺失；这是收集规则遗漏，**不是上游源码缺许可**，已在下述2026-10-12记录补正。包内LICENSES目录也检查，补足crc-catalog双许可证；不修改全局Cargo缓存。

`audit-nv18-native-licenses.ps1`现在在pending审核时也逐个核对实际源码/许可文件及所有附加许可的SHA、路径与文件类型。预编译archive内仍0份许可，但所需随包候选许可已独立收集；实测剩余**2类阻塞**：原生二进制/源码对应的独立审核，以及传递源码许可闭环（ORT递归依赖、上述缺失许可等）。`release_ready=false`，严格门禁继续拒绝。

所有记录均为 `review_status=pending`、空审核人，不自动制造审批。专用内部NSIS脚本已配置收集两套源码与证据目录；实际封包及hash依本轮最终构建回执。不得对外发布，源码资料齐备不自动证明合规或可逐字节复现。干净机安装、签名、真实模型/设备仍未验收。

## 2026-10-12 许可正文遗漏补正与锁定字节复核

### 2026-10-12 后续：ORT候选依赖与三个gitlink收集

新增`scripts/collect-nv18-ort-dependencies.mjs`，首先验证固定ORT根tar的完整SHA-256，解析其[官方deps.txt](https://github.com/microsoft/onnxruntime/blob/v1.28.2/cmake/deps.txt)全部41条声明；35份源码依上游SHA1验证，并额外记录实测SHA256，6个protoc主机二进制不冒充对应源码。使用有界流式下载、单件512MiB/总量1.5GiB、最多3并行，原子无覆盖暂存；不改全局Cargo或第三方缓存。

从v1.28.2 Git树解析三个gitlink，核对deps.txt/.gitmodules的Git blob SHA与固定tar一致，收集：onnx `2bb50465112feca9003e1ed654d77f01ff1415ca`、libprotobuf-mutator `7a2ed51a6b682a83e345ff49fc4cfd7ca47550db`、emsdk `c0bb220cb6e6f4e0fabb6f6db9efd53390ef5e56`。不再凭branch猜子模块版本；该双文件绑定仍不等于根源码全部内容或预编译构建配置已被独立证明。

最终38份候选归档/431,766,263字节、uncollected_sources为空，落在helper自有`native-license-evidence/ort-dependencies/`，证据清单为`native-license-evidence/ort-dependency-evidence.json`；提取真实许可正文及hash。审计重新核对全部候选源包/文本的实际字节与上游SHA1，保持pending、空审核人、release_ready=false。

匿名上游release检查未列出名称匹配SBOM/attestation/provenance/CMakeCache/build-log的附属文件；针对本次预编译archive digest的匿名attestation请求返回404。这只表示本轮未取得可核验构建证据，不能断言所有渠道不存在。38份包含大量可选平台源码，不能推定全部被链接；候选包内递归依赖、实际CMake选项与原生二进制对应仍未闭环。严格发行门禁继续拒绝两类阻塞，不得伪造批准或对外分发。

`r-efi`的[官方AUTHORS说明](https://github.com/r-efi/r-efi/blob/main/AUTHORS)把完整MIT许可及版权声明放在AUTHORS中。本轮直接从checksum已匹配Cargo.lock的5.3.0/6.0.0 `.crate`提取各自AUTHORS原文；未套用其他版本或手写许可。收集器仅在AUTHORS含LICENSE、MIT完整许可起句与COPYRIGHT段时将其计入许可证据，避免把普通作者列表当许可。现在146归档/279文本、`missing_license_texts=[]`，所有记录仍pending。

只读审计另核对helper Cargo.lock自身SHA、registry包集合及每个源码checksum、实际279文本的SHA和非重解析点。正常诊断通过字节核对；严格发行检查仍按预期拒绝2类：**原生二进制/源码对应的独立审核**与**传递依赖/许可闭环**。固定ORT v1.28.2源码中`.gitmodules`列onnx/libprotobuf-mutator/emsdk，`cmake/deps.txt`另含固定URL/hash的依赖归档；单个ORT源码tar不包含它们。不能在缺预编译构建选项与依赖对应证据时凭猜测宣称闭环完成。
