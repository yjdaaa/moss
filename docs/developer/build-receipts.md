# 交付构建回执

阶段收尾或交付时的构建证据：时间、大小、SHA-256 与核对说明。规则见 [`AGENTS.md`](../../AGENTS.md)。新回执追加在最前面；历史条目内嵌的 hash 仍保留在 `CHANGELOG.md` 原条目中。

## NV19 长期记忆、UI-3 语音中心、家居控制与声纹用户开启（2026-10-15）

**本次回执**：同一工作树合并两个会话的改动（NV19-01～08、UI-3 语音中心改版；家居控制与声纹改为用户开启、功能真实性审查修复）。`cargo test --workspace` **804 PASS / 7 ignored**，前端 `npm test` **124 PASS**，`npm run build` 成功，6 个语音浏览器测试通过，release `moss-host` 的 FakeGrbl E2E **32/32 PASS**；`cargo tauri build`（含前端构建）与 `cargo tauri android build --target aarch64 --apk` 均 exit 0。

| 本次产物（0.1.0） | UTC时间（2026-10-15） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 07:38:51 | 59,948,544 | `0807a82493c7a0fa6489a6caf488a493406714bb1da140882b37879cbc571933` |
| 普通 `target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 07:38:51 | 18,931,857 | `37f4b28353f33fafd171569e78d589a437f9b5b1ebc8a69dcc2ace0d3291fe17` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 07:42:46 | 52,870,898 | `02417a0701e3fed311134e0d9acd2dccbf04b4740c12b3d143b48b5d3b802dbc` |

APK 内 `lib/arm64-v8a/libmoss_desktop_lib.so` 与本次 `target/aarch64-linux-android/release/libmoss_desktop_lib.so`（47,149,800 B，07:41:34）SHA 一致：`d45eb4178ae47cb8712188723e2d15d493e3576b5fa0d86a2472f058a6a6745c`。未构建带 helper 的专用内部 NSIS；未签名、未安装、未发布；GUI（HW-15、HW-17、HW-18）与真实设备验收未做。

## NV11 六类供应商契约（2026-10-12）

**NV11 本次最终回执（2026-10-12）**：定向插件 **33/33 PASS**；最终workspace **710 PASS / 7 ignored**（含最新RSS坏尾部和Metaso应用码），前端 **109 PASS**，新release moss-host的FakeGrbl E2E **32 PASS**。实际React假native、前端build、普通Windows release/NSIS、Android aarch64 release APK全部exit0；5份文档/46本地链接与围栏检查、5份Markdown渲染无横向溢出通过。只读复核确认六类TLS夹具经过实际生产adapter、证书验证未关闭、DNS固定本机且无真实账号请求。配置存储故障原子性与真实供应商仍未验收，NV11整体保留partial。

| 本次产物（0.1.0） | UTC时间（2026-10-12） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 15:27:17 | 58,604,544 | `32e0e739f9bd68d282dab354754b8fbc66c19a993b34a7dc18fbedac80dfbf1e` |
| 普通`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 15:27:17 | 18,608,125 | `d7a468f5bde56fbc8f8c4e3f714fbe03711cbe9d24ac62587e428830cb2967e0` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 15:30:07 | 52,601,878 | `c3ba05878048983b2203c917352c8281c7469e605cb9f339235e18a5d310245f` |

三件交付文件及本次arm64 Rust `.so`再次独立读取核对hash；APK内嵌`.so`和`target/aarch64-linux-android/release/libmoss_desktop_lib.so`一致，SHA `c8105b21404dee4bb4ed2ee977d2c75844527f23c0f1a7f848c79c8930c69865`（46,880,784B，15:29:35 UTC）。本轮范围NV11，helper源码/协议及NV18资料未变，**未重新构建NV18专用包，也不以旧专用包代表本次应用代码**；普通NSIS不代表helper发行验收。无实机/真实账号/安装/签名/外部分发/提交推送，未删除既有缓存和用户文件。

## NV13→18 续做（2026-10-12）

**NV13→18 本次最终回执（2026-10-12）**：workspace **698 PASS / 7 ignored**，前端 **109 PASS**，使用本次release moss-host的FakeGrbl E2E **32 PASS**；前端build、普通Windows release/NSIS、Android aarch64 release APK、helper构建检查及专用内部NSIS全部exit0。构建流程中的Node工具测试10项通过，之后新增helper环境白名单测试并重跑最新 **11/11 PASS**，实际helper合成WAV诊断再跑通过；不冒充真人标定。7份文档/32本地链接/围栏检查与7份Markdown渲染无横向溢出通过，3份PowerShell脚本AST检查通过。当前文档入口为融合r13/内置语音n20/实施i3，NV19仅为后续规划。

| 本次产物（版本0.1.0） | UTC时间（2026-10-12） | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 12:45:57 | 58,601,984 | `32edbfbca3b6dce9dcd6a5a21d36b0de27b72c837faa3276c49dfbf0ed4420c9` |
| 普通`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 12:45:56 | 18,603,117 | `aa7f68d8cb2d13bd77198b74c1d79fafaed791afbce5184b380e6a6edceb4f84` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 12:48:10 | 52,601,878 | `85904055d55c0bbeec7c241084f89266482bd4f8949e561a9ce778e9cdbdfb52` |
| 专用内部`target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 12:57:31 | 734,429,264 | `e3d2f17ce6223d3ececd7c931c45098ba23bd1526403a93dfc63e9b63be7670c` |

本次独立只读解包核对专用NSIS **875条资源路径**，38份ORT候选归档逐份核对实际字节/hash，证据JSON SHA `f80798fb0746466ca3734c1cf9301dd524ed2302447c665e8c330e0c4b985a49`一致；90份许可证回执只证明收集和字节，不证明实际链接/递归闭环。清单为`target/nv18-690efb906d344d739048a9874fc240db/release-manifest.json`，协议4/schema9，源码树SHA `da26e438672991267d211c38174ad896bb166752c745ec1dfc48af86f7e33b1f`。包内helper20,828,160B/SHA `f8feb516ef7ad28247ddb0eeec4cf502dcce153a62dbb7957b977feeae4ff0e1`与构建检查产物一致；其源码未变、Cargo复用03:59:50 UTC二进制，不声称重新生成字节。包内desktop SHA `a0835b6e645aca55c567fd2601df9a06df8e35fd469ee6be6c64a72b00b0d52e`，仅唯一Tauri bundle标记3字节变化，内存校正后全文件一致。APK内嵌arm64 `.so`与本次编译产物SHA均为`ad2b58be73af74dc5a5ec30517e5eb2ccf283fc3e9ebb48acdb5e498ac27dbbd`。

本次Windows桌面EXE、普通NSIS和专用NSIS Authenticode均 **NotSigned**；SDK36 apksigner只读验证APK **v2签名有效（1个签名者）**，未验发行身份、升级兼容或签名验收。NV13内层沙箱仍blocked；NV15真实HA按用户决定暂缓；NV16真人标定/可信报告开放、NV17设备持钥/生产下载实现、NV18独立原生对应审核/递归许可闭环及安装升级仍partial/unverified。专用包因纳入源码资料增加到734.43MB，仅内部待验，不可外部分发；无实机、私人录音、账号、安装、发布、提交或推送，未清理既有缓存。

## 阶段B 最终交付（2026-10-12）

**阶段B最终交付回执（2026-10-12，当前代码）**：`cargo test --workspace` **698 PASS / 7 ignored**、`npm test` **109 PASS**、新release moss-host的FakeGrbl E2E **32 PASS**，实际React登记/摘要CAS/窄屏通过；前端build、普通Windows、Android、独立helper构建检查和专用内部NSIS全部exit0。8份修改文档/91本地链接与围栏结构检查，9份Markdown渲染无横向溢出；技术书引用已统一r13/n19/i2，历史版本保留。NV8a中间整句停止现有实现及本轮测试已同步，不再把final-only历史当当前行为。

| 产物（版本0.1.0） | UTC时间 | 字节 | SHA-256 |
| --- | --- | ---: | --- |
| `target/release/moss-desktop.exe` | 2026-10-12 09:57:05 | 58,601,984 | `fefd563d1c4900f8f2d6ce5dccaf95d06a950b1c043e6daf2b8fdf2de289805e` |
| 普通`target/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 09:57:05 | 18,603,192 | `a3b9142b2705670eafe8c0b00e2f6b855ad1876cc91adc7cfe6066fcb8743300` |
| `apps/desktop/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk` | 09:59:14 | 52,601,878 | `04e726e322a70149330e209f997ec661bd36e0e0d95fc28f098bf2ae23d8f336` |
| 专用`target/x86_64-pc-windows-msvc/release/bundle/nsis/MOSS_0.1.0_x64-setup.exe` | 10:05:08 | 366,144,877 | `044730bf3f37cec94539176ef2059c9fe824fe1db253657925b100a59cfb8f3d` |

APK内arm64 `.so`和本轮Rust产物完整SHA均为`3b11d0e37a075eea031afba9defd799ba51d08fda60235dcc2e0281219240a20`。专用包清单`target/nv18-a11f4d32a8d040e380d5c90495fab4f0/release-manifest.json`：协议4/schema9，source tree SHA `da26e438672991267d211c38174ad896bb166752c745ec1dfc48af86f7e33b1f`。独立7-Zip扫描749条资源路径，实际内存解包helper20,828,160B的完整SHA `f8feb516ef7ad28247ddb0eeec4cf502dcce153a62dbb7957b977feeae4ff0e1`与构建检查产物一致；helper源码本阶段未变，Cargo复用同一二进制（原时间03:59:50 UTC），**不声称重新生成了其字节**。本轮重新使用公开模型+合成PCM实际运行：192维/768B、44ms单次合成推理、错误请求和模型写入/替换拒绝；不代表真人准确率。

专用包desktop58,594,304B与构建产物仅Tauri `__TAURI_BUNDLE_TYPE_VAR_UNK`→`NSS`的3字节差异，在内存仅补该唯一标记后整文件SHA一致，包内SHA `7b7e1c263f03bcda90de2201049e637bb94cf4e51a6fc1a0cf4e82844a2721d2`。原生/Rust证据JSON及两版r-efi的AUTHORS完整字节/hash都与项目文件一致，未执行安装器。严格许可门禁仍拒绝2类阻塞；生产HA、声纹识别/按人模式、可信OTA下载和真实CLI沙箱开放均未通过，无实机动作、真人采集、账号调用、安装或外部发布。未清理既有缓存或无关产物。

## 阶段A 交付（2026-10-12）

**本轮阶段A交付回执（后续代码增量需重新构建）**：workspace686 PASS/7 ignored、前端109 PASS、FakeGrbl32 PASS；前端及Windows/Android/独立helper/专用NSIS均exit0。Windows `target/release/moss-desktop.exe`58,232,320B / `f60ffad372f47a7ee6baee5731d1e7e00062f598abcfdb3832a2c891c1c50907`，普通NSIS18,544,601B / `37148466b0e28996077acdc5aee5d8a115f4c4c69323f8adec10812f063535d2`（UTC 2026-10-12 04:32）。Android APK52,596,158B / `b5da0dc08a5bc30aa64a8e962951242b51729bab16f1fd30aa893d61d8af94cf`（04:43 UTC），内嵌arm64 Rust `.so`与当前产物同为 `3f4b68ea91c1cb08415cc4dc5e60669a0fe49bb0f5857096201d0ad3bf4e7f1f`。专用内部NSIS366,098,342B / `1de45451b9effbc4b8676089f01099ed09db2c8ac97203035f3ae572a8f093b5`（04:44 UTC），协议4/schema9清单 `target/nv18-6d11b13bef0a47a2b89e23b2fd3c9ae7/release-manifest.json`。Docker未运行，用官方7-Zip26.04 MSI（SHA `0b01334a418654293513449f61d6bfc99e5196ca371e3c3dc961eda57cd535c6`）只读MSI/CAB提取到任务临时目录，未安装；NSIS独立列748条路径，确认原生/Rust资料和清单，内存解包helper20,828,160B完整SHA `f8feb516ef7ad28247ddb0eeec4cf502dcce153a62dbb7957b977feeae4ff0e1`与构建完全一致。许可证严格门禁仍拒绝2类阻塞。随后新增NV15协调器内存fake+真实临时SQLite接线，7/7专项通过；测试入口仅cfg(test)，生产控制仍硬关闭，后续全量/交付回执单列。
