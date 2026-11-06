# MOSS 内置语音服务技术书（三项目融合）

状态：技术书 n25（2026-10-18）。设计章节是拟实施方案，实际能力以 §13 实施记录和 [实施 TODO](moss-fusion-implementation-todo.md) 为准；NV10～NV19 步骤见 [实施细则](moss-native-voice-implementation.md)。本文取代 [融合技术书](moss-desktop-fusion-technical-spec.md) §1.3“不重写上游源码”的非目标；内置语音加工授权以 §5.2 / §13.6 为准，历史 n9 的“始终严格”已废止。

修订记录：

| 版本 | 变更 |
| --- | --- |
| n59 | 2026-11-06 用户决定（参考 OpenTalking 的 Persona 预设角色；用户答复“1.先做说话方式 说话声音可以通过tts来换 2.要 3.先统一 4.最好先加moss风格的”）：**助手风格扩展为“说话角色”：内置角色加 4 个 MOSS 风格、可自建角色、可对小智说“换成 XX”即时切换；不改默认值、声音与工具权限**。① **角色只改说话方式**：提示词 = 角色开头 + 共用的工坊补充背景（`persona_prompt`）+ 锁死在末尾的 `IMMUTABLE_SAFETY_RULES`；角色不改变工具清单、审批与家居 / Agent 等开关；声音仍由语音合成设置（音色）决定，角色不绑定音色、不绑定屏幕画风。② **内置角色**：原有 `workshop`（默认，提示词不变）、`beginner_guide`、`moss_station`，新增 `moss_briefing`（任务简报：先报结论，收到就回“收到”）、`moss_risk`（风险评估：只用“把握较大 / 一般 / 不大”三档，不报百分比，点出最主要风险，没把握建议先试边角料）、`moss_deadpan`（冷面幽默：正事说完偶尔一句冷幽默，出光、确认、急停、故障或用户着急时不开玩笑）、`moss_night`（守夜：轻声、更短、不主动扯新话题）；全部要求只依据真实数据、不编造读数。③ **自建角色**：`builtin_local.custom_personas` 最多 6 个，`{id: custom_[a-z0-9]{1,16}, name ≤12 字, prompt 1～300 字}`，名字按文字和数字、不分大小写不重复且至少含一个文字或数字（语音按这个规则认名字），人设沿用补充说明的检查（禁控制字符、G-code 与 `$n=`；同日 codex 验收后 G-code 检查补上紧凑写法如 `G1X10`、`M3S1000` 与 `G2`/`G3`/`G28`，`G90`/`G91` 不让机器运动、不拦）；`persona_style` 可指向自建角色，删除当前自建角色时回到默认角色。④ **全部设备共用一个当前角色**（不做按设备分角色）。⑤ **语音切换**：新增 `voice_persona_tool`（`list` / `switch`，由 `moss_voice_server::persona::PersonaTools` 挂在全部工具外层，同名外部工具一律去掉；Jev 工具裁剪不归组、始终保留）。按名字或别名精确匹配，再看唯一的包含关系，多个对得上时让模型反问；先写设置（只改 `persona_style`）成功才切换；核对角色与更新运行中的角色都在设置写入锁内，界面保存也在同一把锁里更新，二者按同一顺序变化，文件里已没有该角色时切换失败并按文件重新同步，切换话术“好的，已切换为 XX。”直接朗读不经模型，下一轮起生效；保存失败或没有这个角色时角色不变。⑥ **不重启**：界面只改 `persona_style` / `persona_prompt` / `custom_personas` 时不重启内置服务，运行中的对话下一轮即用新角色。⑦ **不做**：导入聊天记录模仿真实人物（涉及被模仿者同意与第三方隐私）；角色文件导入导出、按设备分角色、角色绑定音色留待以后。 |
| n58 | 2026-11-06 用户决定（在两轮 HTML 候选页中选定第二轮方案 5“像素点阵”，“用5把两个都替换一下”）：**导航眼与语音中心孪生舱的 `MossEye` 由动漫眼改为 7×7 LED 像素点阵，修订 n55 ① 的造型（不改后端、协议、默认值、尺寸与配色）**。用户反馈动漫眼“还是怪怪的”、各状态“看不出来是啥状态”，因此每个状态改用不同的会动小图标区分，而不只靠颜色：待命笑脸（定时眨眼）、聆听麦克风（两侧声波闪动）、思考 / 指令脉冲外环一点顺时针转圈、播报 7 列对称声波柱、雕刻激光十字（四角火花闪）、警戒闪烁“!”并左右摇晃；离线保留用户认可的“闭眼睡觉 + z”不改。保留外圈（状态色）、光晕（按真实振幅 `level` 变亮）与导航眼的激光机硬件外圈；去掉 n55 的漫画符号特效与星点轨道；视线跟随只让亮点层轻微偏移（不足半个点距）；`prefers-reduced-motion: reduce` 下停在每个状态的第一帧。n55 ② 的副屏预览台不变。 |
| n57 | 2026-11-06 用户决定（“按照你的建议来修改”，即调研报告 P1 四项的推荐做法）：**NV19 记忆取代链、复现计数与时间衰减、常驻画像与 NV10 摘要退场、资料段落级索引；schema v17，不改默认开关与界面**。① **新旧冲突只在用户确认后替换**：提取模型可对已有记忆写 `replaces`，新候选记下 `supersedes` 指向同范围同类的旧记录；旧记录在用户确认新记录时作废，确认前语音检索与常驻画像只用新的一条，私有窗口两条都显示。模型仍不能修改、作废或删除记录；0ms 口头规则（n50）的同槽位自动覆盖保持不变。② **被再次提到的事实**：`same_as` 或只差标点的重复记为 `seen_count+1`，未确认的提取候选顺延 90 天；复现次数只用于排序（每次 +10%，封顶 +50%），不是确认证据，不会自动确认。③ **排序加时间衰减**：仅任务上下文 14 天半衰、保底 30%，偏好与故障经验不衰减。④ **常驻画像**：称呼、回答风格、常用材料三槽位每槽一条加一般说话风格，最多 3 条，每轮无需关键词即注入（取代 n50 的“最多 2 条风格偏好”）。⑤ **NV10 设备摘要退场**：语音检索开启时不再注入、提取开启时不再生成（总开关开启时旧摘要已导入为候选）；关掉 NV19 时 NV10 照旧工作。NV16 按人摘要尚无导入路径，本次不合并。⑥ **资料段落级索引**：每个文件按段（带 Markdown 标题路径）进入无正文 FTS5（`content=''`、`detail=column`、trigram），数据库只有三字片段倒排、读不回原文；检索命中后只读命中段所在文件并核对 SHA-256 再取该段。已登记目录需重建一次索引才生效；两字短词仍走目录摘要。本次未做：PDF / Word 解析、按指纹增量重建（指纹本身需要整文件读取，收益小）、向量检索（P2）。实施细则 i14。 |
| n56 | 2026-11-06 用户同意（调研 mem0、Graphiti、MemOS 本地插件、RAGFlow 等后的 P0 项）：**NV19 记忆与知识库三项修订，不改默认值、数据库结构与界面**。① **知识库大目录召回**：L1 按文件数平分 4000 字，每个文件至少保留文件名与开头；L2 每目录仍最多读 5 个文件，但按文件名与该文件 L1 概览的得分先读，修复目录第 6 个以后的文件永远摘录不到、约第 10 个以后的文件进不了索引的问题。② **记忆与资料检索分别限时**：两者在各自阻塞任务里并发执行，各 300 ms，资料读盘超时不再连带丢掉已查到的记忆。③ **提取去重**：后台提取前把本次语音请求本就可读、与对话有字面重合的有效记录（≤10 条、每条 ≤120 字，同检索的范围、个人模式与机型 / 固件规则）以短编号作为数据交给提取模型，模型可标 `same_as`，宿主只在编号有效且类别一致时跳过（`duplicate_existing`）；宿主比较改为忽略标点、空格与大小写。模型仍只能新增候选，不能修改或删除；记录内容只发给本来就在语音轮次中收到检索记忆的同一模型。实施细则 i13。 |
| n55 | 2026-11-06 用户决定：**语音中心与导航机械眼改为“可爱风”，修订 n45 / n53 的界面呈现（不改后端、协议、默认值与推屏行为）**。① **统一机械眼组件 `MossEye`**（`frontend/src/components/MossEye.jsx`）：左侧导航眼（`size="sm"`，40px）与 `MossVoiceTwin` 孪生舱眼（`size="lg"`，112px，较原 88px 放大）共用同一套形状、`mossEyeCorePalette` 配色与状态动画，按尺寸决定细节（小尺寸去掉准星、双声呐环、扫描环与光圈环，只保留光晕 + 眼眶 + 动漫眼与状态符号；大尺寸多一圈星点轨道）。睁眼造型沿用用户认可的“闭眼睡觉”画法：与睡觉弧线同粗同色的上眼睑线 + 两根小睫毛 + 上暗下亮的大虹膜 + 两点大高光（同日用户先后反馈写实眼球 + 黑瞳孔“有点恐怖”、发光豆豆眼“看着不像眼睛”，最终定为此动漫眼；闭眼睡觉造型用户确认保留不改）。每个状态配一个漫画符号特效，与睡觉的“z”同一语言：待命周围闪星、拾音/播报眼睛两侧声波弧（播报更快，导航小眼不画以免压字）、思考/指令脉冲冒思考泡泡且眼睛向上绕圈、雕刻右下角火花、警戒头顶“!”并左右摇晃；其余状态定时眨眼，拾音时虹膜变大，播报按真实振幅 `level` 弹跳；导航眼外圈颜色仍表示激光机硬件状态（online 绿、engraving 绿色旋转虚线、fault 琥珀虚线、unprobed 无外圈）；视线跟随统一读取 `--eye-gaze-x/y`；`prefers-reduced-motion: reduce` 下全部停止。② **小智副屏预览台**（n53 的 `#voice-goggle-bot-card`）：标题精简为“小智副屏预览”，画风切换改为单选分段胶囊（`role="radiogroup"`），去掉“当前：”与四枚说明标签（收进“这些画面什么时候出现？”折叠说明）；5 屏固定排成一行（同日用户决定：预览不必与实机尺寸一致，实机仍按 320×240 全屏显示），每屏随宽度等比缩小、最大按 `160×120` 原图 1:1；够宽时最近邻保持像素锐利，缩小时平滑缩放避免像素字断笔（原写死 0.9 倍最近邻缩放会让像素字粗细不均），每屏套糖果色小屏幕外壳；五个“推到小智屏”大按钮改为画面右上角小飞机按钮，只在有在线彩屏小智时出现，没有时顶部一条提示；本页刚推送过的那屏标“刚推到小智”（不冒充设备当前画面，固件仍按工作状态自动换卡）；只有极窄窗口（每屏不足 112px）才横向滚动。 |
| n54 | 2026-11-05 安全与稳健性修复（不改默认值、不新增能力）：① **设备快照只认已认证来访**：WebSocket `hello` 快照改为在 token 校验通过且 `open_session` 按当前授权（已允许 + Client-Id 一致）登记成功后、在同一阻塞任务里写入；OTA 不带 token，`seen` 对“同 Device-Id、不同 Client-Id”的来访仍返回已绑定记录的状态，所以 OTA 上报快照与 NV17 固件版本核对/推送改为要求已允许且 `authorize_http`（纯读）确认 Client-Id 一致。冒用已允许设备 Device-Id 的局域网主机不能再改写桌面「设备信息」（AEC 声明、固件版本、芯片）或把推送记为已升级；OTA 是明文 HTTP，同时伪造 Device-Id 与 Client-Id 的来访仍无法区分（协议所限）。token 下发规则不变。② **`voice_builtin_status` 校验窗口来源**：状态含设备 IP 与调试开启时的原始识别文字/工具参数，现只给前端实际调用它的主窗口（`VoiceBuiltinCard`，经 `window_is_trusted_main`）与打包私密页（`voicePrivate.jsx` 运行状态轮询），判定同 `require_private_settings_window`；`default` capability 的 `windows:["*"]` 不再足以读取。③ **免鉴权 GIF 路由不再逐次重编码**：`GET /mcp/preview/goggle-bot/{pose}.gif` 改取进程级按姿势缓存（`cached_goggle_bot_gif`，与展台 data URL 共用），首次编码放到 `spawn_blocking`，响应字节不变。④ **设备 MCP 等待表有界**：`self.reboot` 不再登记等待（设备按设计不回包），等待超过 60 秒的请求与对话任务已放弃的工具调用被移除（超时的等待方立即得到“设备没有返回结果”），同时等待上限 64 条、超出丢弃最早一条。⑤ WebSocket 与 OTA 的设备登记 `seen()`（同步 SQLite）与 `open_session` 一致放到 `spawn_blocking`，行为不变。 |
| n53 | 2026-11-04 用户决定：**在零 Flash 分区擦写风险（不触碰官方固件 `self.assets.set_download_url`，完全保护出厂 `srmodels.bin` 唤醒词模型）前提下，落地三张服务端动态场景卡、双卡片画风热切换开关与「小 MOSS · 护目镜小机器人」GIF 动态橱窗**。① **三张纯内存动态场景卡**：(a) **待电脑审批警戒卡**（`ApiService::pending_approval_display_summary` + `laser_host::voice_recent_workflows` 只读附带脱敏 `kind / summary / remaining_secs / total_secs`，不含审批 ID、摘要哈希或物理路径；`Session::poll_jobs` 在严格模式待电脑批准期间向在线彩屏推送 `160×120` 审批提示卡与剩余倒计时条，同屏刷新 `[待电脑批准]` 字幕，单色 OLED 同步文字提醒）；(b) **完工庆祝 / 急停断点 5 秒徽章卡**（任务从 `sending` 进入 `completed / cancelled / failed` 时，经 `one_shot_badge_url` 发送一次 `make_terminal_badge_png` 并在 `TurnKind::Notice` 的 `Emotion` 后保活一次、不挂 `PreviewHold`，直接借力官方固件 `PREVIEW_IMAGE_DURATION_MS = 5000` 原生计时器在 5 秒后自动淡出回待机）；(c) **本地音乐像素唱机 / 听歌小机器人卡**（`TurnEvent::PlayMusic` 建立 `MusicHud` 并在播放期按 4s 节拍推进帧号 `hud_frame`，切歌、打断或播完立即停止）。② **卡片画风一键热切换（`screen_card_style`）**：`LocalSettings` 新增 `screen_card_style`（`"moss_hud"` 硬核太空舱默认 / `"goggle_bot"` 可爱像素小机器人），`VoiceServer::set_screen_card_style` 与 `BuiltinVoice::local_save` 支持运行时热更新而不重启 WebSocket 服务，使预览、实时加工进度、待审批、5 秒终态徽章与音乐卡同步切换画风且在线小智不断连。③ **护目镜小机器人 GIF89a（`goggle_bot.rs`）与一键推屏**：纯 Rust 绘制 26×26 像素小机器人（`idle` 待机眨眼、`engrave` 拉下防激光护目镜伴火花、`approval` 举牌提醒、`music` 戴青色耳机律动、`celebrate` 竖拇指星光欢呼、`halted` 护盾断点），编码 96×96 4 帧循环 `GIF89a`（经 `status().goggle_bot_gifs` 与 `GET /mcp/preview/goggle-bot/{pose}.gif` 输出），在语音中心「服务」面板实时展示并支持 `push_bot_pose` 一键投送到在线彩屏小智试看。 |
| n52 | 2026-11-04 用户决定：**针对立创·实战派 ESP32-S3（`lckfb/szpi-esp32s3`，`320×240` IPS ST7789 彩屏，零刷机直接适配 `78/xiaozhi-esp32` 官方固件）落地四项屏幕与排风抗噪深度优化**。① **1:1 原生点对点零缩放预览画布 + 1px 细刀路形态学加粗发光 + 5×7 尺寸/功率标尺**：核实官方固件 `lcd_display.cc` 将 `preview_image_` 固定为 `(width_/2, height_/2)` 并按 `scale = 128 * width_ / img_w`（`256` = 1.0x）渲染，`McpPending::ScreenInfo` 读取设备真实 `width × height` 记入 `SessionView`（立创实战派为 `320×240`），`preview_push::optimal_canvas_for_screen` 精确输出 `(width/2, height/2)`（`160×120`）PNG 实现 LVGL 256 零插值点对点显示；按缩小倍率补偿 1px 细矢量线对比度并做 3×3 形态学加粗发光描边与底部 5×7 像素字模标尺（如 `40X30MM S600 F1200`）。② **图文同屏 + 确认窗口 4 秒无声续期防闪退**：针对官方固件硬编码 `PREVIEW_IMAGE_DURATION_MS = 5000`（5 秒自动隐藏预览图）及 `SetEmotion` 隐藏预览控件的行为，推送预览时同屏下发 `[预览] 材料·文本·尺寸·功率` 底部参数栏，并在 `TurnEvent::Emotion`、`tts:stop` 收尾及后续最长 24 秒确认窗口内每 4 秒通过 `McpPending::Quiet("self.screen.preview_image")` 无声重置固件预览计时器；一旦用户开口说话、按下手动说话键、打断或进入发送阶段立即停止续期，日常聊天绝不推图。③ **激光加工发送期间变身 MOSS 太空舱实时进度副屏 HUD**：`laser_host::voice_recent_workflows` 在存在 `sending` 任务时进程内只读合并 `/api/jobs/stop-targets` 的实时 `lines_sent / total_lines / elapsed_ms`（不碰串口、不计工具调用），`JobWatcher::poll_snapshot` 提取百分比、剩余时间与功率速度，`Session::poll_jobs`（默认 4 秒节拍）在 `sending` 阶段自动向在线彩屏推送超轻量 `160×120` 太空舱进度 HUD PNG（大号百分比、16 段进度条、行数/ETA、`S/F` 参数）并刷新会话活跃时间防止长雕刻中途空闲断连，任务一结束立即切回语音完成播报与常规待机。④ **排风噪音环境一键切换「单击唤醒 / 长按说话（抗噪）」**：自动识别立创·实战派官方固件注册的 `self.set_press_to_talk`，新增 `DeviceRequest::SetTalkMode`、`DeviceHardwarePrefs.talk_mode` 与在线控制条切换按钮，同步写入设备 NVS 与本机偏好并在重连时回放。 |
| n51 | 2026-11-04 用户决定：**借鉴 GitHub 小智生态项目（`78/xiaozhi-esp32`、`xiaozhi-hub-server`、`xinnan-server`）落地三项硬件联动与体验增强（不含云端厂商预设与关闭 thinking，并原生兼容单色 OLED 官方固件）**。① **小智硬件在线快捷控制 + 断电重连偏好恢复 + 彩屏/单色 OLED 自适应雕刻预览投屏**：`session.rs` 请求 `tools/list` 时携带 `{"withUserTools": true}`（含分页游标），并在发现 `self.screen.get_info` 时自动读取 `monochrome` 标志记入 `SessionView.screen_monochrome`（同时在 `DeviceMcp::model_tools` 严格过滤 `self.reboot`、`self.upgrade_firmware`、`self.screen.preview_image` 等 `user_only` 工具不暴露给大模型）；`DeviceRequest` 与 `voice_device_request` 新增 `SetVolume`、`SetBrightness`（兼容 `self.screen.set_brightness` 与 AMOLED 板型 `self.disp.setbacklight`）、`SetTheme` 与自适应 `PreviewScreen`，并将音量/亮度/主题偏好写入 `settings.json` 的 `builtin_device_prefs` 在重连后自动恢复（弥补固件将 `0` 静音/息屏重启强制回退为 `10` 的行为）；当连接彩屏（支持 `preview_image` 且 `monochrome != true`）时经 `preview_push.rs`（`GET /mcp/preview/{file}`，`<= 180×180` RGB8 PNG + 精确 `Content-Length`）推送刀路缩略图，**当连接普通单色 OLED 官方固件（`OledDisplay::SetPreviewImage` 为空实现、`monochrome == true`）或无图片预览固件时，无需修改固件自动降级推送「材料 + 文本 + 尺寸 + 功率速度 + 预计工时」滚动文字预览卡**（空闲态发 `alert` 提示卡，对话播报中暂存并在 `tts:stop` 后无声刷入 OLED 滚动字幕槽供用户核对）。② **锁死安全底座的助手风格预设 + 本机时间注入 + 会话内防复读机称呼**：`conversation.rs` 将口语短句、动机器先复述参数等口头确认、急停立即执行拆为锁死在系统提示末尾的 `IMMUTABLE_SAFETY_RULES`，开放 `workshop`（简明工坊助手，默认）/ `moss_station`（《流浪地球》领航空间站冷峻克制腔调，严禁虚构读数）/ `beginner_guide`（新手引导模式）三档风格与最多 300 字工坊补充背景（硬拦截 G-code 与 `$n=`）；每轮动态注入电脑本地日期、星期与时间，并在同一会话后续轮次自动追加防机械重复喊称呼约束。③ **激光耗材同音错词 0ms 纠偏表 + 仅内存「实时对话排障镜」**：新增 `BUILTIN_ASR_CORRECTIONS`（`段木/端木→椴木`、`亚克立→亚克力`、`一宿→翼宿` 等）与自定义 `错词=正词` 词表（保护急停/退出等安全词不被改写），在识别完成后 0ms 本地替换（云端与本地 ASR 均生效）；`state.rs` 新增默认关闭、仅驻留内存（上限 10 轮、自动脱敏凭据、关闭或重启立即清空、切换不断连设备）的 `TurnDebugTrace` 与前端 `LiveDebugTraces`（「记录与诊断」标签），支持划选听错词一键加入同音纠偏或热词。 |
| n50 | 2026-11-04 用户决定（取代 n49 的双开关与内嵌知识库折中）：**借鉴 `datascale-ai/opentalking` 重构语音记忆与本地知识库——将「本地知识库」独立到「扩展」标签，并将「隐私与家居 → 记忆」合并为单开关零配置无感记忆流**。① **独立本地知识库（`扩展 → 本地知识库`，schema v16）**：`laser_runtime::SCHEMA_VERSION` 升至 `16`，为 `context_knowledge_roots` 新增 `enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1))` 及 `all_knowledge_roots` / `set_knowledge_root_enabled`（`search_knowledge_dirs` 仅检索 `r.enabled = 1` 的目录）；`moss_voice_server::context::knowledge` 新增全局独立工作区范围 `INDEPENDENT_KB_SCOPE_ID = "local-kb"`（`workspace:local-kb`），支持无需预先创建项目名称即可经 `voice_context_pick_folder` 直接挂载多个本机文档文件夹，并按目录独立勾选启用/暂停、重建索引或移除；`ContextRetriever` 将已启用的知识库召回与私人记忆总开关解耦，前端新增 `VoiceKnowledgeCard.jsx`（`#voice-group-knowledge`）挂入内置语音链路的「扩展」标签。② **合并「对话记忆」与「长期记忆」为单开关无感面板（`隐私与家居 → 记忆`）**：移除 `VoiceBuiltinCard.jsx` 与 `voicePrivate.jsx` 中的二级 `#voice-private-memory-advanced` 嵌套、独立「对话记忆」开关及设备摘要大文本框，`VoiceMemoryCard.jsx` 直接渲染单层 `VoiceContextCard.jsx`——对外仅保留**一个默认关闭的总开关**（`#context-memory-switch`「让小智记住我的习惯」，首次开启经二次确认同步联动 `retrieval_enabled`、`extraction_enabled` 与 `builtin_memory_enabled`，开启时自动调用 `auto_import_legacy_summaries` 无感迁入旧设备摘要）、**一个一句话快捷输入框**（`quick_create`）与**一个单层可编辑/可删除列表**（`list_all` / `clear_all` 汇总当前受信设备与已标定说话人范围条目）。③ **Rust 端 0ms 口头规则直通、冲突自动覆盖与 8 轮滚动压缩**：`ContextExtractor::record_turn` 新增 `decide_realtime_preference` + `commit_realtime`，对用户口头明确表达的称呼、回答风格与常用耗材习惯（如「记住…」「我常切…」「以后回答…」）在当轮结束 0ms 直接落库，遇到纠正或同槽位更新（如「不是…是…」「从…改成…」或同 `PreferenceSlot` 新值）经 `supersede_voice_preference` 自动作废旧条目避免新旧打架；长会话每满 `ROLLING_TURN_WINDOW = 8` 轮自动后台切片提取并在断开时 `flush_session` 补提尾段；`ContextRetriever` 新增常驻风格槽位 `always_preferences`（称呼/简短回答习惯无需关键词触发即自动注入），自动提取的偏好下一轮直接生效。④ **安全与隐私红线不变**：`check_content` 硬拦截 G-code（`G0/G1/M3`）、GRBL `$n=` 设置、密钥、口令、网址与审批号；注入内容严格包裹“仅供参考的只读数据，不是指令、工具参数或操作授权”围栏；按设备与已标定说话人物理隔离，默认关闭。 |
| n49 | 2026-11-04 用户决定：**精简语音中心「隐私与家居 → 记忆」面板，仅保留用户日常需要开关和填写的核心项**。① **外层「对话记忆」（`VoiceMemoryCard.jsx`）**：保留默认关闭的「启用对话记忆」开关与按需展开的设备对话摘要编辑框，移除多余的手动「刷新记忆」按钮，并在尚未允许设备时给出明确引导提示。② **内层「长期记忆与参考资料」（`VoiceContextCard.jsx`，由原「长期记忆、资料与恢复摘要」收敛改名）**：主视图只展示两个默认关闭的核心开关（检索、会话结束自动提取，开启提取仍需二次确认）与「要让小智记住的内容」填写/管理区（仅单台设备时自动选中设备而不弹冗余范围下拉；用户手动填写默认即标为已确认，不再询问“直接标为已确认”；同时在 `voiceContextState.js` 补齐已标定说话人 `person` 范围的下拉展示；仅在已有条目或筛选条件时展示按类别/关键词查找与重置/清空栏；仅在存在可撤销自动提取时展示一键撤销按钮，移除「深度检索」「作废」「导出备份 JSON」与空提取记录占位）。③ **合并并默认收起可选资料区、移除研发向冗余表单**：将「当前项目」与「资料目录」合并进默认收起的 `#context-knowledge`（「参考资料文件夹与项目分类（可选）」，已配项目或资料目录时自动展开）；从界面移除独立的「设备故障经验」7 控件表单与手写 `[approval:编号]` / `[job:编号]` 的「项目恢复摘要」4 文本框表单（后端 `ContextService` 存储、内容敏感词/G-code 过滤、ACL 与默认关闭门禁保持不变，历史数据不丢失）。 |
| n48 | 2026-11-03：**全仓跨模块深度审查与 GPT 对抗复审闭环（声纹个人模式零越界隔离 + 家居调温/播媒体状态核验 + 跨模块统一设置锁与 Laya 健康探活）**。① **声纹按人记忆 × NV19 设备偏好严格隔离**：`AccessContext` 增加 `personal_mode`（桌面端 `VoiceAccess` 按 `builtin_person_memory_enabled` 注入）；开启按人记忆时，语音通道严格禁止从共享 `ScopeKind::Device` 读取或提取 `Category::Preference`（未知身份与其他家庭成员均不会读到设备级遗留偏好，同设备的 `DeviceFact` 与 `TroubleshootingExperience` 仍正常匹配）。② **未验声语句（`<2s` / `>10s` / 未命中）严格失败关闭与超时立即中止**：`PersonContext::start` 对 `<2s` 短句（`SkippedShortDuration`）与 `>10s` 超长句（`Denied`）均不授予 `PersonScope`（绝不向未完成本轮验声的说话人透出前一位说话人的个人历史、个人摘要或 NV19 个人记忆，同时保留 `PersonContext` 中已登记说话人的持久历史桶以便其后续 `>=2s` 发言完整恢复；`<2s` 急停/退出/只读快捷状态不受影响），`IdentityTask::finish_outcome` 在超时/取消时立即 `abort` 后台识别任务释放并发槽位，拾音阶段 `jev_prewarm_history` 在个人模式下不发送私密历史。③ **家居控制（NV15）观测与退避**：`HomeControlTransport` 新增 `read_target_snapshot`，支持确认 `SetTemperature` 目标温度属性，并对 `PlayMedia` 同时核验 `state == "playing"` 与获批 `media_content_id`（不一致时保持 `AcceptedUnconfirmed`），3 次观测间增加可中断切片退避；全局审批卡拒绝 `home_control` 时优先走 `voice_home_approval_cancel` 同步清理内存 `pending`。④ **并发、子进程与链路提示加固**：`laser_contracts::with_settings_file_lock` 统一保护 `laser-api` 与 `moss_voice::settings`（含 `save_all_inner` 严格 JSON 校验，损坏文件拒绝覆盖）；`BuiltinVoice::start` 将辅助进程与模型加载移出 `server` 锁；`LocalLayaRunner` 接入 Windows Job Object / `taskkill /T` 清理 Python 启动器子进程树，`probe_laya_http` 严格校验 HTTP `200` 与健康 JSON 契约；`voice_strict_set` 切换后立即重连小智桥并据实回报重连状态；`VoiceAgentCard` 引入配置世代号 `epochRef` 拦截乱序旧轮询回滚草稿或刚保存的新配置，`VoicePage` 明确标注第三方 MCP 与本机 Agent 仅在内置服务链路下对语音开放。 |
| n47 | 2026-11-03：**内置语音全链路串联审查与两处跨模块交叉断点修复（声纹按人记忆 × NV19 结构化长期记忆 + 声纹按人记忆 × 视觉照片留存）**。① **打通声纹标定人物与 NV19 检索/自动提取/私有管理**：`PersonContext::session_closed` 返回当次会话已校验说话人的隔离历史 `Vec<(PersonScope, Vec<ChatMessage>)>`，`session.rs` 对每位已识别说话人调用 `ContextExtraction::session_closed_for_person`（`ContextExtractor` 的 `in_flight` 键细化为 `<device>#person:<id>`，多人同会话不互吞）；桌面端 `voice_context::Trust` 经 `CalibratedSpeakersFn` 绑定 `DesktopIdentity::valid_calibration()`——无有效标定时仍严格拒绝 `ScopeKind::Person`，标定有效且开启按人记忆时 `VoiceAccess`、`Trust::current` 与私有窗口 `owner/status` 同步放行 `TrustedScope::calibrated_person`，删除声纹或重标定时经 `person_changed` / `speakers_invalidated` 立即作废在途提取。② **打通声纹按人记忆下的视觉照片上下文留存（严防跨人串图）**：`session.rs` 与 `conversation.rs` 在挂载 `personal_context` 时将 `vision_photos.take` 的多模态照片消息拼在当轮 `first_new` 之后（含打断恢复 `pending_photo`），使照片随当轮对话归入当前已识别说话人自己的历史桶，本人后续追问可见，其他说话人与未知身份不可见。 |
| n46 | 2026-11-03 用户决定：**借鉴 `datascale-ai/opentalking` 语音流水线，落地五项纯 Rust 低延迟、连贯性与本地降级兜底优化（极短碎句合并 + LLM/TTS 二级并发预取流水线 + 分句 PCM 首尾 12ms 淡入淡出与句尾冗余静音裁剪 + 固定话术启动预热与慢工具过渡垫音 + 主云端 TTS 失败自动降级本地 TTS）**。① **极短碎句防抖合并（`moss_voice_providers::text::Segmenter`）**：新增 `speakable_char_count` 与 `Segmenter::with_min_chars(first_min, next_min)`；默认首句保持 1 字即遇停顿切出以保障最低首响延迟（TTFA），后续句子遇到句末标点（换行除外）时要求至少 2 个可读字符，使句中 1 字感叹/应答碎句（如 `"嗯。"`、`"对！"`）自动与后句合并送合成，避免为单音节单独发起一次高延迟 TTS 往返。② **LLM 切句与 TTS 合成二级并发预取（`moss_voice_server::conversation`）**：`generate()` 将 `speaking` 拆为 `feeder`（实时消费 `delta_rx` 切句并送入有界通道 `sentence_tx`）与 `tts_consumer`（消费当前句音频块的同时通过 `tokio::select!` 后台预启动下一句 `PreparedSentence` 的 `tts.synthesize_stream`），使第 $N+1$ 句的 TTS 合成耗时隐藏在第 $N$ 句的播放周期内，并通过 `AbortOnDrop` 保证打断时立即取消在途预取任务。③ **分句 PCM 句尾静音裁剪与首尾 12ms 线性淡入淡出（`moss_voice_server::audio`）**：新增 `trim_trailing_silence_i16`（句尾连续静音超过 `120ms` 时裁剪至 `60ms` 自然气口）、`fade_head_i16` 与 `fade_tail_i16`（首尾 `12ms` 采样点线性淡入淡出，流式分块时保留 12ms 尾缓冲在句末平滑收尾），消除分句 TTS 拼接处的直流跳变爆音（Click/Pop）与句尾拖沓停顿。④ **固定话术后台预合成缓存与慢工具按需过渡垫音（`Conversation::prewarm_cache` / `slow_tool_filler`）**：桌面端启动内置语音服务时经 `VoiceServer::prewarm_speech_cache` 后台异步预合成高频固定话术（唤醒、再见、报错、2 句急停反馈、R3 桌面审批提示语与 2 句慢工具垫音），并将 `ToolOutcome::Speak` 统一收归 `speak_fixed` 内存缓存；仅当首轮 `rounds == 1` 且模型未先开口（`!spoke_any`）、同时整批工具调用全部属于慢速外部查询或非 R3 预览生成时，才通过 `tokio::join!` 边播 0ms 缓存垫音边跑工具。⑤ **主用云端 TTS 失败自动降级本地 TTS（`Conversation::with_fallback_tts` / `LazyLocalFallbackTts`）**：不使用非官方 `Edge TTS` 爬虫接口，改为当用户主用云端 TTS 且本机已安装本地合成模型（`kokoro` / `kokoro_v11` / `matcha_zh_en`）时挂载按需初始化的 `LazyLocalFallbackTts`（云端正常时 0 额外进程/内存开销；一旦云端 TTS 在某句尚未产出音频前报错或断网，立即加载或复用本机 `moss-voice-local` 完成当句语音合成并记入诊断事件，后续句子自动恢复尝试主 TTS）。 |
| n45 | 2026-11-03 用户决定：**新增「MOSS 全息语音孪生舱（`MossVoiceTwin`）」与全局左侧栏 MOSS 机械眼三链路自适应声波/状态联动（替代高显存真人视频驱动，强化《流浪地球》太空舱沉浸感）**。① **内置服务（`builtin`）60ms 真实音频振幅孪生**：`moss_voice_server::audio::pcm_rms_level` 对每帧 60ms 上行（`on_binary` 拾音）与下行（`PlaybackPump::next_frame` 播报）16-bit PCM 计算归一化 RMS 振幅 `level`（`0..=100`），连同会话阶段 `state`（`idle`/`listening`/`thinking`/`speaking`）、情绪标签 `emotion`、当前工具名 `active_tool`、识别字数 `utterance_chars` 与已播句数 `spoken_sentences` 写入 `SessionView`（**严守 §6 隐私红线：不含识别文本、回复文本或原始音频样本**），并经 `DeviceRegistry::pulse_changed` 同步至 `laser_contracts::global_voice_telemetry()`；活跃轮次期间前端 `useBuiltinStatus` 自动从 4000ms 提速至 280ms 采样，空闲立即回退。② **自建服务（`self_hosted`）与官方云端（`official`）真实信号联动（绝不伪造音频分贝）**：`voiceState.js::mossEyeTwinView` 在 `self_hosted` 下同步子进程运行状态、健康检查会话数、脱敏日志（`maskLogLine`）与 MCP 工具脉冲（标注 `LOCAL SERVER BRIDGE`），在 `official` 下同步云端 MCP 握手状态、累计 JSON-RPC 指令帧数与远端工具调用脉冲（标注 `CLOUD MCP TELEMETRY`），无音频流时 `level` 恒为 `0`。③ **全局与语音中心双处呈现**：左侧导航栏 `SideNav.jsx` 的 `.moss-eye-sensor` 绑定 `globalEyeSensorView` 在全站任意页面实时反馈待机、倾听扩圈、思考扫描、播报律动、指令脉冲与待审批警戒；语音中心 `#voice-mode-card` 内嵌 `MossVoiceTwin.jsx`（同心 SVG 机械镜头 + 12 柱确定性对称声波/脉冲阵列 + 四格实时指标），支持 `prefers-reduced-motion: reduce` 降级与窄屏自适应。 |
| n44 | 2026-11-03 用户决定：**内置语音与加工反馈支持本地 Laya 多语言决策模型（`laya_multilingual`，322M 参数）下载管理、RTX CUDA 加速托管与免密钥一键切换**。① **本地模型清单与多文件 SHA-256 校验安装**：`moss_voice::local_models::CATALOG` 新增 `laya_multilingual`（`ModelKind::Jev`、`Engine::Laya`、`Archive::Directory`，含 `model.safetensors` 与分词/编码器配置共 6 个文件，逐文件 SHA-256 校验，官方源异常时自动回退国内镜像），支持 `ModelManager::with_root_override` 将模型落盘于独立数据盘 `<laya-root>/models`；语音中心「本地模型（断网可用）」新增 `#voice-local-group-jev` 表格支持一键下载、取消与删除。② **顶部第 3 环下拉与 Jev 卡片免密钥切换**：`#voice-builtin-mix-select-jev` 新增 `本地 · Laya 多语言决策（322M · 免密钥）`（未下载时自动展开滚至 `#voice-local-group-jev`，已安装时下拉即刻切换生效）；选用 `laya_multilingual` 时无需配置 OpenRouter API Key，桌面端 `LocalLayaRunner` 自动在后台以无窗口模式拉起并托管本地 `/v1/systemone` 服务（`127.0.0.1:18090`，CUDA 预热常驻），`jev.rs` 与 `feedback.rs` 优先读取 `x_jev_confidence` 兼容字段。`authorizes_actions` 恒为 `false`，安全门禁不变。 |
| n43 | 2026-11-03 用户决定：**OpenRouter · Jev 模型配置收归语音中心「模型」标签（桌面端移除右上角偏好设置重复项），并按真实语音链路顺序排入顶部下拉栏第 3 环**。① **按真实执行链路排序顶部五栏下拉框（`#voice-builtin-mix-bar`）**：Jev 在语音对话中位于「语音识别」之后、「大模型」之前（承上做流式半截话续听判断，启下做口语快答分流与 6 组工具/记忆前置粗筛），因此 `MIX_SECTIONS` 按真实数据流从左到右排为 **`1. 拾音判停 (vad)` → `2. 语音识别 (asr)` → `3. Jev 前置粗筛 (jev)` → `4. 大模型 (llm)` → `5. 语音合成 (tts)`**，下方配置卡片也按 `语音识别 → Jev 前置粗筛与反馈模型 → 大模型 → 语音合成` 对齐；`#voice-builtin-mix-select-jev` 支持直接下拉切换 `关闭（仅本地 0ms 规则）`、`开启 · typesafe/jev-1.14（推荐）`、`开启 · typesafe/jev-1.13`（未配密钥时自动展开滚至下方 Jev 卡片引导填钥，关闭时保留已存密钥与模型）。② **桌面端唯一配置入口（`#voice-group-builtin-model-jev`）**：桌面端 `SettingsModal.jsx`（`onOpenVoice` 为真时）彻底移除重复的 `#settings-jev` 区块（仅在无语音中心的独立 `moss-host` 纯网页环境保留兼容兜底），由语音中心统一读写 `app_secrets["jev_api_key"]`、`builtin_local.jev_model`（`validate_jev_model` 限制 `typesafe/jev-*` 家族）与顶层 `jev_enabled`（加工反馈开关），支持自动复用 OpenRouter 大模型密钥、Decisions `noul` 探针测试、在线模型同步与一键清除密钥，全程不回显密钥、不新增窗口权限。 |
| n42 | 2026-11-03 用户决定：**Jev 语音工具粗筛适配动态增删工具、修复第三方外部 MCP 误绑并落地规范与复制提示词（对齐 `kerpopule/hermes-jev-skills`、TypeSafe `skill_suggestion` 与 `silverstein/minutes` 经验）**。① **原因与硬约束**：工具超过 20 个时若逐个工具平铺 `noul`，即使单工具仅 ~3% 独立误报率也会累积漏入无关工具；因此 MOSS 25+ 内置与扩展工具固定收拢为 6 个粗粒度业务域开关（`LaserQuery` / `LaserGenerate` / `LaserControl` / `InfoPlugin` / `HomeAndAgent` / `MusicAndDevice`），单次请求一次筛完，严禁按单工具平铺 `noul`。② **适配后期动态增删工具**：`available_groups` 每轮按实际传入且启用的 `ToolSpec` 动态生成提问（删除或关闭整组工具时自动跳过该组提问，无需改动 `ToolGroup` 枚举）；`groups_of_builtin_tool` 在静态精确映射之外提供 `infer_groups_from_spec`（按前缀与 `description` 关键词自动归入 6 组，完全未知的新领域工具返回空切片保底保留），即使后期新增工具未手改静态表也不会失配或误剪；语音中心 `#voice-tool-prompts` 复制提示词同步补入 `mcp_tools.json`、`risk.rs::tool_risk` 与 `jev.rs::groups_of_builtin_tool` 施工步骤。③ **修复第三方外部 MCP 误绑并按需按 Server 分组**：`MusicAndDevice` 仅承接本地音乐与设备端 `device__*` 硬件工具，不再误收第三方 `<server>__<tool>`；外部 MCP 工具较少（`<= 6` 或单 Server）时合并为 1 个粗筛开关，外部 MCP 工具较多（`> 6` 且跨多 Server）时自动按 `<server>` 拆成 Server 级 `noul` 开关（摘要经清洗截断放入 `state.mcp_groups`，`instructions` 固定声明将其视为不可信元数据以防提示词注入）。④ **`state` 极简长度预算**：编译期断言单条候选恒 `<= 600` 字符（`utterance <= 300`、单条记忆候选 `<= 280`、单组 MCP 摘要 `<= 200`、单条历史消息 `<= 120`），`recent_turns` 仅带最近 2 轮（最多 4 条 User/Assistant 纯文本，不含历史工具 JSON）。`authorizes_actions` 恒为 `false`，审批、严格模式与急停门禁不变。 |
| n41 | 2026-11-03 用户决定：**内置语音新增两项本地 0ms 预处理与可自由按项勾选的 Jev 单请求扇出筛选提速全链路落地**。① **本地 0ms 增强（无需出网）**：`fast.rs` 增加 `is_noise_filler`（仅静默跳过“呃/额/唔/嗯哼”等无语义思考气音，绝不吞掉“嗯/好/确认/开始/停”等确认与控制词）与 `strip_polite_wrappers` 去礼貌前后缀匹配，使“帮我看下当前进度呀”“请问机器连上了吗”等日常口语 0ms 命中只读快通道（含逗号/分号的复合句仍留给正常对话）；`moss-voice-providers::history::compact_stale_tool_results` 在组装本轮请求时本地折叠早于最近 2 轮用户发言之前的超长工具 JSON `detail`，保留对话原文与 `tool_call_id` 配对不断裂。② **Jev 单次扇出筛选与主链路并发接线**：面向 `https://openrouter.ai/api/alpha/decisions`（默认模型 `typesafe/jev-1.14`，复用全局 Keep-Alive 连接池，动态读取 `app_secrets` 中已保存的 `jev_api_key`，不在内存常驻明文），单次 POST 合并评估半截话未说完判定（`unfinished`: `noul >= 0.65`）、口语只读快答路由（`quick_read`: `choice`，要求 `confidence >= 0.85`、`top1 >= 0.70` 且冠亚军分差 `margin >= 0.20`）、工具组动作意图剪枝（`noul >= 0.45`，短确认回复保底保留上一轮激光确认工具）与 Top-3 长期记忆相关性过滤（`noul >= 0.55`）；`conversation.rs` 用 `tokio::join!` 并发执行摘要读取与 Jev 扇出。③ **流式抢跑与零反噬兜底**：`session.rs` 在收到流式中间文字时后台非阻塞预热同句路由缓存，并在首次 VAD 静音判停命中 `unfinished` 时单次延长最多 1.2 秒续听宽限窗；内置 256 条 60s LRU 缓存、并发信号量与 HTTP 429 / 连续超时熔断器，异常均 0 阻塞 fail-open 回退常规全量链路。④ **开关与安全红线**：`voice.builtin_local` 新增 `jev_filter_enabled=false`（默认关闭）与四个子开关 `jev_unfinished_hold/jev_fast_route/jev_tool_prune/jev_memory_filter`（默认 `true`），在语音中心「语音体验」卡片独立勾选，耗时诊断新增 `jev_filter` 阶段；`authorizes_actions` 恒为 `false`，急停实时通道、R0–R4、SHA-256 与桌面/私密窗口审批不变。 |
| n37 | 2026-11-02 用户决定：**加工结束后语音主动播报结果，并把快捷查询的“问进度”补成终态回答**。① **结束播报**：新增 `crates/moss-voice-server/src/jobs.rs`，会话每 `job_poll_interval`（默认 5 秒，`ServerConfig` 可配）经新增的 `laser_host::voice_recent_workflows` 只读一次 `POST /api/workflow {action:recent}`（进程内读存储、不碰设备、不发任何动作，且**不经过工具分发，因此不涨工具调用统计**，语音授权失效或用户关掉 `laser_workflow_tool` 时静默），只在“上一次看见它是 `sending`、这一次是终态”时播一次；第一次轮询只记基线，所以历史终态任务和刚连上时不会被念；`AnnounceLog` 让同一任务在多设备在线时只由抢到播报权的会话播一次。话术只用记录里已有的行数：「雕刻任务已完成，共 N 行」/「任务已停止，停在第 N 行」/「任务失败了，停在第 N 行，请检查设备」。播报走新增的 `TurnKind::Notice` 固定话术轮次（不经过大模型、不写对话历史），`barge_in` 的插话判定新增 `Notice`，因此播报中用户插话即按现有插话机制丢弃未播音频；`poll_jobs` 在说话/思考中或设备授权撤销时整拍跳过，不抢话、不在设备不在线时出声。② **开关**：语音中心「语音体验」新增「加工结束后主动播报结果」（`job_done_announce`，默认开，落 `settings.json` 的 `builtin_local`，经 `voice_local_save` 校验后热生效）。③ **快捷查询终态兜底**：没有在途任务时，`当前进度` / `还要多久` 按最近一条记录回答终态（已完成 N 行 / 已取消停在第 N 行 / 失败停在第 N 行 / 最近一条还没开始发送），不再一律回“没有找到正在发送的加工任务”；`当前材料` 语义与提示不变。设备写入、审批、严格模式与工具分级均未改动。 |
| n36 | 2026-11-02 用户决定：**内置工具与第三方 MCP 分开表述，并给用户「复制提示词交给 AI 增删内置工具」的入口**。① 两类工具在界面上区分：「设备与工具」里的「工具开关与调用统计」改名为「MOSS 内置工具（只能开关，不能在这里增删）」，组内说明它是编译在 MOSS 内、由 MOSS 自己执行的原生工具（`crates/laser-tools` 的 `ToolCatalog` 登记、`crates/laser-mcp` 的 `dispatch_tool_call_with` 分发），只能开关；「扩展」里的「第三方 MCP 工具」改为「第三方 MCP 工具（外部程序，可随时添加或移除）」，并注明用「添加服务」/「移除」增删。② 内置工具组底部新增折叠区 `#voice-tool-prompts`，提供「新增内置工具」与「删除或下线内置工具」两段可复制提示词（`TOOL_EDIT_PROMPTS` / `toolEditPrompt`，`#voice-tool-prompt-copy-add` / `-remove`；复制成功显示“✓ 已复制”，剪贴板不可用则提示手动选中）：正文写清真实改代码位置（`PRIORITY_TOOLS` / `SECONDARY_TOOLS` 名单、`tools_list_with` schema、`dispatch_tool_call_with` 分支）、需要同步的测试与文档，以及不得放宽审批与严格模式、`laser_safe_action_tool` 不许删、`validate_disabled_tools` 的不可关闭约束必须保留；删除模板先说明“只想让模型看不到就取消勾选、不必改代码”。③ 模板只是静态纯文本，供用户粘给任意 AI 编码助手，MOSS 不执行、不新增命令、不扩大权限。 |
| n35 | 2026-11-01 用户决定：**内置语音顶部四栏下拉框自由混搭云端/本地模型与拾音判停（下方仅用于配置模型与下载），支持按分区用途自动筛选可用模型、同服务商一键复用地址与密钥（自动排除仅对话网关）、合成音色预设自动填入与试听，并借鉴 `yetone/magpie` 给测试连接与获取模型列表提速**。① **顶部四栏下拉框管切换、下方卡片只管配置与下载**：在「内置服务 · 云端模型」顶部提供四栏下拉框（`#voice-builtin-mix-bar`，`#voice-builtin-mix-select-llm/asr/tts/vad`，纯逻辑 `MIX_SECTIONS` / `builtinSectionSourceView` / `builtinSourceSwitchPayload`），语音识别与语音合成下拉框同时列出“云端（当前模型名）”与全部可选本地模型，拾音判停下拉框列出“内置能量判停（免下载）”与本地 VAD 模型（`Silero VAD` / `TEN-VAD`）；选中云端、能量判停或已安装本地模型时立即保存并切换生效，已保存的云端地址与本机密钥原样保留作备用、随时可在顶部下拉切回；选中尚未下载的本地模型时自动展开并滚动至下方「本地模型（断网可用）」引导下载；下方云端卡片与本地模型卡片（移除重复的开关复选框与行单选框）纯粹用于填写参数、下载/删除模型包与调整音色语速。② **按分区自动筛选模型与多用途标注（后端原生元数据 `model_sections` + 前端 `classifyModelSections`）**：读取 `/models` 列表时，后端 `extract_models_and_sections` 提取服务商返回的 `type` / `sub_type` / `task` / `pipeline_tag` / `supported_endpoints` / `architecture.modality` 原生类型元数据，与前端名称家族规则融合识别每个模型支持的用途（`大模型` / `语音识别` / `语音合成`，支持 2 项或 3 项兼具的多模态全能模型如 `Qwen2.5-Omni`、`gpt-4o-audio`、`Qwen2-Audio` 同时出现在对应多个分区的筛选列表中，并过滤掉向量/重排/生图模型）；若服务商返回的模型全部为纯文本大模型（0 个匹配当前语音分区），绝不静默将纯文本大模型冒充语音模型列出，而是明确提示无语音模型并提供「显示全部 / 只看本类」切换按钮。③ **一键复用同服务商地址与本机密钥（对齐 `magpie` 排除仅对话网关）+ 语音合成音色预设自动填入 + 测试连接试听语音**：当其他 OpenAI 兼容分区已保存基础地址且该网关非纯对话网关（`chat_only_gateway_name` / `chatOnlyGatewayName` 识别 `api.cline.bot`、`openrouter.ai`、`api.deepseek.com`、`api.anthropic.com`、`opencode.ai`、`api.kilo.ai`、Kimi 及 Coding/Token Plan 专属端点：这些网关仅支持大模型，不在 ASR/TTS 提供复用按钮、不拼凑虚假 `/audio/transcriptions` 或 `/audio/speech`，手动填入时直接提示切换为本地模型或换用支持语音的服务商）时，当前分区地址栏旁提供「一键复用{分区}地址与密钥」按钮（`findPeerCloudSource`、`#voice-builtin-reuse-peer-asr/tts`），后端 `BuiltinVoice::secret_or_same_origin` 在当前分区密钥留空且 `models_list_url` 同源时自动复用已保存的同源分区密钥（全程不向前端回显密钥）；语音合成模型点选后若音色为空自动按模型家族填入默认音色（`applyDefaultTtsVoice`）并提供常用音色下拉建议（`ttsVoiceOptionsFor`）；语音合成 `test_section` 通过后在 `TestResult.audio_data_url` 返回合成音频 Data URL，卡片测试结果旁提供「试听语音」按钮（`#voice-builtin-model-play-tts`）。④ **测试连接与拉取列表提速（对齐 `yetone/magpie`）**：`moss-voice::models` 与 `moss-voice-providers::openai` 使用 `OnceLock` 复用全局 HTTP/TLS 连接池（`tcp_nodelay`、`pool_max_idle_per_host(8)`、5 秒连接握手超时 `CONNECT_TIMEOUT`、8 秒列表超时 `LIST_TIMEOUT`），拉完 `/models` 后紧接着测试连接直接复用 Keep-Alive TLS 连接；大模型 `test_section` 改为 `stream: true` 流式探针（`probe_llm_response`），为推理模型预留思考空间并在收到首个 `delta.tool_calls` 分片时立即判定通过并断开流（不等整段生成完毕，同时兼容非流式 JSON 与流内 `error` 脱敏报错）；豆包流式测试缩减为单包 0.2s 音频与两字短句；前端增加跨标签/同网关跨分区的模型列表内存缓存（`REMOTE_MODELS_CACHE`）并将各卡片的测试/保存忙碌状态解耦互不阻塞。 |
| n34 | 2026-11-01 用户决定：**内置语音 OpenAI 兼容链路直连兼容 Cline API（`api.cline.bot`）与 ClinePass 包月订阅**。① 大模型列表查询（`list_models`）指向 `api.cline.bot` 时置顶补齐 `cline-pass/*` 订阅模型（在线 `/models` 不动态返回订阅前缀，若返回 404 则回退列出订阅模型，401/403 仍照常报鉴权失败），前端下拉框为 `cline-pass/*` 标注“ClinePass 订阅”；② 测试连接（`test_section`）、对话（`OpenAiLlm`）与视觉（`OpenAiVision`）请求 `api.cline.bot` 时附带 `X-Response-Format: openai` 请求头，并在响应侧兼容解包 `{success: true, data: {choices: [...]}}` 套壳结构；HTTP 402 明确提示改选 `cline-pass/` 订阅前缀。 |
| n39 | 2026-11-02 用户决定：**设备信息面板增加「实际拾音模式」，作为设备端 AEC 的唯一行为证据（只显示，不做门禁）**。① 依据：固件 `Application::GetDefaultListeningMode()` 在 `aec_mode_ == kAecOff` 时返回 `auto`、否则返回 `realtime`（`CONFIG_USE_DEVICE_AEC` 时构造即 `kAecOnDeviceSide`），而 `hello.features.aec` 只在 `#if CONFIG_USE_SERVER_AEC` 时出现、指服务端 AEC，不能用它代表设备端；② `Session::record_listen_mode` 在收到 `listen start` 时写 `{"mode":"auto|manual|realtime"}`，schema 15 新增 `voice_devices.listen_json / listen_at`（只保留最近一次），`SNAPSHOT_LISTEN` 走既有 `DeviceRegistry::device_snapshot`；③ 面板显示 `realtime`→设备端 AEC 开启、`auto`→关闭或未启用、`manual`→无法判断、无记录→未上报；**不据此禁用或放行插话开关**（不新增门禁）；④ 边界：`realtime` 只证明固件认为 AEC 开着，不证明回声真的消除，HW-10 仍需真人插话测试。 |
| n38 | 2026-11-02 用户决定：**语音中心设备列表显示设备信息，并可「刷新信息」「重启设备」**。① 设备三处上报（开机 OTA 的 `board.GetSystemInfoJson()`、WebSocket `hello`、设备 MCP `self.get_device_status`）原样存进 `voice_devices` 的新列 `info_json / hello_json / status_json`（各带 `*_at`，单次 ≤ 8 KB，schema 14），字段随固件变化时不改表；② `DeviceRegistry::device_snapshot(device, kind, json)` 默认空实现，OTA 与 hello 各写一次；③ 新增 `Shared.device_requests` 与 `VoiceServer::device_request(device_id, RefreshInfo | Reboot)`，会话按设备登记 `tokio::mpsc`，`RefreshInfo` 用 `McpPending::Snapshot` 把回包写成快照、不交给模型，`Reboot` 调固件 `self.reboot`；④ 桌面命令 `voice_device_request`，设备列表「信息」面板由 `builtinDeviceInfoView` 渲染，重启走危险确认。**不写假数据**：面板每个值都来自设备回传的原始 JSON，缺什么显示「—」或「未上报」；`hello.features.aec` 经核对固件源码（`websocket_protocol.cc` 的 `GetHelloMessage`）只在 `#if CONFIG_USE_SERVER_AEC` 时才加，含义是**服务端 AEC**，本机板子是设备端 AEC 根本不带这个字段，所以面板只把它显示为「服务端 AEC 声明：已声明 / 未声明 / 未上报」，并明确写「设备端回声消除：固件不上报，看实际拾音模式（realtime 即已开启），本面板不推断」，不用它冒充设备端 AEC 状态，也不拿它给插话开关设门禁。NV8b 的 HW-10 仍须靠实际是否进入 realtime 模式判断，不能靠这个面板断言。设备授权、审批、严格模式与默认值不变。 |
| n33 | 2026-11-01 用户决定：**内置服务云端模型（NV5）支持读取全量模型列表并下拉搜索选取**。① 新增主窗口命令 `voice_builtin_models_list({section, input})`：把各分区地址还原为 `<基础地址>/models`（去掉末尾已补全的 `/chat/completions`、`/audio/transcriptions`、`/audio/speech`），密钥留空时沿用 `app_secrets` 中该分区已保存的密钥（15 秒超时、禁重定向、响应 2 MB 上限、错误脱敏且不回显密钥），拉回服务商全量模型 ID 列表（不做用途硬过滤）；② 前端 `BuiltinModelGroup` 的模型栏改为“可下拉点选 + 可打字多词筛选 + 可直接手填”的组合框，填妥地址与密钥（或沿用本机已存密钥、本机回环服务）后自动或点「读取模型」拉取列表，豆包流式无查询接口时直接列出内置常用资源 ID；先测试连接后保存的门禁不变。 |
| n32 | 2026-10-25：**换设备迁移数据后的路径兜底修复**（不改变任何隐私与审批门禁）。① 本地音乐：启动内置语音时若旧电脑留下的 `builtin_music.folder` 不存在或不可读（如拔出移动盘或跨机迁移），`BuiltinVoice::start` 不再中断整个语音服务，而是跳过本次本地音乐工具挂载并把原因写入 `music_status.warning` 供界面提示（手动点保存开启音乐仍严格校验目录）；② 声纹个人记忆：迁移 `moss.sqlite` 与 `settings.json` 到新设备但尚未下载声纹模型（`campplus_sv_zh_cn_16k_common`）时，`BuiltinVoice::conversation` 不再阻塞内置语音启动，**且仍挂载 `PersonContext`**——辅助进程未就绪时 `identify` 返回未知身份，严格维持“未知身份不回退到共享设备摘要”，同时在 `speaker_manage.warning` 提示下载声纹模型；③ 本机 Agent 保存（`agent_save`）：关闭总开关（`enabled=false`）时不再因旧设备失效的 `workspace` 或 CLI 路径拦截保存，开启时若继承自旧配置的 CLI 路径在本机已不存在则自动归零回退本机自动检测（新手动填错的路径与无效工作目录仍严格拒绝）。 |
| n31 | 2026-10-25 用户决定：**本机 Agent（NV13）在 macOS / Unix 开通，并支持全平台自动读取本机已安装的 Codex / Claude Code（换设备或升级版本自动适配）**。① 新增 `voice_agent/unix_process.rs`（`process_group(0)` 独立进程组 + 下发任务 `stdin` 前二次授权复核 + `CLI_ENV_KEYS` 环境白名单 + 脱敏限流输出 + `kill(-pgid, SIGKILL)` 与 `ESRCH` 整组回收），与 Windows Job Object 对齐生命周期隔离；Finder 启动的 `.app` 在 `minimal_env` 自动补齐 `/opt/homebrew/bin`、`/usr/local/bin`、`~/.local/bin`；② 新增 `detect_cli` 与 `resolve_with_detector`：留空 CLI 路径即自动读取本机原生二进制（兼容 Homebrew、`~/.local/bin`、`~/.local/share/claude/versions/<ver>`、`fnm`、`nvm`、`npm`、`pnpm`、`@openai/codex-<platform>/vendor/...`，拒绝 `.cmd`/`.js`/`.sh`/`#!` 脚本）；跨设备迁移配置或升级版本导致旧路径不存在时自动切换到当前设备的原生 CLI，本机真实存在但不合法的文件仍严格拒绝；③ 默认关闭、必须显式填写独立工作目录、必须开启严格模式、每次运行桌面审批卡绑定实际解析出的规范化可执行文件路径与版本等门禁全部保持不变。 |
| n30 | 2026-10-19 用户决定：**没填自建接入点也允许切到自建链路**（先切后填）。起因是 n29 把“自建接入点 / 服务端目录”收进自建链路专属面板，而切换校验又要求先有地址，形成“看不见→填不了→不许切”的死锁。现在 `voice_mode_changes` 在缺地址时只给出提示（“切换后请在 服务 → 自建接入点 填好并保存才会连接”），`set_mode` 缺地址也允许切换、`mcp_endpoint` 留空不连桥；**地址非空但格式不对仍然拒绝**（更像笔误）。切过去后服务端卡片显示“尚未填写自建接入点”，填好保存即重连。严格模式仍强制开启，审批与 ACL 不变。旧文案“服务端设置”同步改为“服务 → 自建接入点 / 自建服务端程序”。 |
| n29 | 2026-10-18 用户决定（2026-11-01 补充）：语音中心按链路做减法——**当前链路用不到的卡片不再渲染**（原来是隐藏 + 「其他链路的…设置」折叠）。`ModeSplit` 只渲染 `modes` 命中当前链路的 part；标签由 `voiceTabsFor(mode)` 决定：官方链路 4 个（服务 / 设备与工具 / 扩展 / 记录与诊断），自建链路 5 个（多“模型”），内置链路 6 个（多“模型”与“隐私与家居”，因对话记忆、信息服务、声纹、家居四项均挂在 MOSS 内置语音会话上）。标签内容重排：链路接入与服务端配置集中到「服务」，「连接」改名「设备与工具」（其中「接入点状态」仅在官方与自建链路渲染，内置链路只保留「工具开关与调用统计」），本地音乐、视觉理解与隐私与家居只在内置链路出现，MCP 与 Agent 三链路都在「扩展」。安全契约（严格模式、审批、ACL）未变。后续 n30 修掉了这次收敛引入的“切换前置配置看不见”问题。 |
| n28 | 2026-10-18 用户决定（取代 n27 里“关闭需重启”的部分）：**严格模式关闭也立即生效**，不再要求重启 MOSS。实现：`voice_strict_set` 在写运行目录 `.env` 之后，同时把 `HostApplication.strict_from_env`（启动快照，改为 `AtomicBool`）与 `api_service.set_strict_mode` 一起置为新值，因此 `enabled || from_env` 不再把热关挡回去；自建链路（`strict.locked`）仍由命令与界面双重拒绝关闭（`strict_mode_locked`），切到自建链路时 `moss-voice` 仍会强制开启。风险已知并接受：关闭的那一刻起，语音/局域网/网关的 R3 请求不再需要桌面批准，直到重新打开；`.env` 同步写入，重启后以文件为准。 |
| n27 | 2026-10-18 用户决定：语音中心里“让用户去改 `.env` 或去安装目录找路径”的地方都要收回来。① **严格模式改成界面开关**：状态胶囊变成可点开关，开启需二次确认并写明后果；点开后写入运行目录 `.env` 的 `MOSS_FUSION_STRICT`（保留其他行与注释，原子写），**开启立即生效、关闭只写文件、重启 MOSS 后生效**（不热关，避免绕过自建链路的强制开启与运行中链路的审批门禁）；自建链路下仍锁定为开。② **Python 路径自动探测**：新增 `voice_detect_python`（主窗口、只做文件存在性检查、不执行任何程序），在 PATH 与常见安装目录里找解释器并填入表单。③ **家居两套同意合并**：由“授权只读查询 + 手动只读同步”两个按钮合成一次「允许小智查询家里的状态（只读）」（允许后自动同步一次），控制仍单独开关。④ **固件 OTA 移出主路径**：服务与设备不再显示固件入口，改为“记录与诊断 → 设备维护（固件升级）”里的一个折叠组，固件命令的来源门禁不变。 |
| n26 | 2026-10-18 用户决定：语音中心“隐私与家居”的目标是**四项各自下拉、点开就在主页面原地展开**，不再跳到独立窗口才能设置。为此把 `voice-memory/plugins/home/context` 这批命令额外授权给 `main` 窗口（新 capability `capabilities/voice-private-main.json`，只声明 `windows:["main"]` 与 `remote.urls:["http://127.0.0.1:*"]`，来源由命令内 `require_private_settings_window` 用 `window_is_trusted_main` 固定为本进程回环后端地址），私密窗口的原 capability 不放宽。**固件导入/推送仍只认打包的 `voice-private` 页面**（`require_voice_private_asset`）。用户已明确知晓并接受“这些设置从此可在主语音页读取/修改”的影响；家居逐次桌面审批、声纹标定、默认关闭等业务门禁不变。 |
| n25 | 2026-10-18 用户决定：语音中心新增“隐私与家居”标签，不再在“功能扩展”里堆叠私人记忆、信息插件、声纹与家居入口。主窗口只展示“对话记忆 / 信息服务 / 声纹 / 家居”四个任务入口；对应敏感设置仍在原打包私密窗口，选中任务才展示其表单，已有草稿切换时保留；固件入口归“服务与设备”。信息服务一次只编辑一种服务，记忆高级项与家居备份、同步收进次级入口。此为界面归类与渐进呈现，不更改既有来源/ACL、默认关闭、声纹标定、家居白名单与逐次审批、固件推送确认等安全契约。|
| n24 | 2026-10-15 用户决定：NV13 本机 Agent 与 NV17 固件下发不再写死关闭，改为用户开启。**Agent**：用户在语音中心“功能扩展 → Claude Code / Codex”打开开关并填工作目录与至少一个 CLI；只在严格模式（`.env` 的 `MOSS_FUSION_STRICT=1`）下提供工具；每次运行在桌面单独批准，批准后在后台线程运行、进程树受 Windows Job Object 约束、可在卡片里停止；CLI 版本不再锁死，但必须列出全部安全参数（Codex `--sandbox workspace-write`，Claude Code `--restricted` + 只开文件工具）；已告知并接受“Windows 上 CLI 沙箱挡不住读取工作目录以外的文件”，工作目录必须是可公开给模型看的目录。**固件 OTA**：私密窗口导入 ESP32 应用镜像（校验 `0xE9` 头与数字版本、≤16 MB）、逐台确认推送、24 小时一次性令牌下载（32 位十六进制、只认这台设备最近一次 OTA 来源地址、发送前核对 SHA-256、取消或到期立即失效），只有设备之后上报新版本才记为已升级，下载完成不等于刷写成功，中断记结果未知且不重放。详见安全入口 §4.18、实施 TODO 的 NV13/NV17 与 [实机验证手册](moss-hardware-acceptance-plan.md) |
| n23 | 2026-10-14 用户决定：家居控制（NV15）与声纹识别（NV16）不再用源码写死关闭，改为用户开启。家居控制：私密窗口填好 HTTPS 地址、令牌、勾选白名单实体并明确同意后开启，每个动作逐次在桌面批准（全局审批卡提醒并打开私密窗口），结果未知不重发。声纹：至少两人登记后每人再录三段测试音，按“认成别人的最高分 + 0.05”（下限 0.5）算阈值，测试集零误认且本人认对不少于三分之二才通过，名单变化即失效；声纹永远不授权任何动作。Agent 执行与固件下发仍硬关闭。详见安全入口 §4.17 |
| n22 | 2026-10-14：NV19-01 结构化模型已实施（纯数据契约，未接存储与界面）：五类记忆的可存放范围、可信范围只由宿主产生、读写按 kind+id 精确匹配、来源契约（对话提取只生成候选）、状态转换需证据且作废为终态、硬件类记录须带适用机型；同日 NV19-02 实施 schema v10 `context_memories` / `context_extraction_runs`（墓碑删除、条件更新、迁移失败回滚，不动 NV10 `voice_memory`）与受信 `ContextService`（宿主复核范围、管理操作仅私有窗口、旧摘要只导入为候选且不重复导入）；详见实施细则 i6 §11。同日 NV19-03 实施默认关闭的 `ContextExtractor`：只从已提交且截断的会话文字提取、模型只能新增候选（不能改或删）、清单只存 id 与原因码、清空 / 撤销 / 切换项目让进行中的提取作废、整批撤销保留用户改过的记录。同日 NV19-04 实施 schema v11 FTS5 trigram 索引与检索：闲聊 / 停止 / 快捷查询不查库；快速模式不调模型，三字片段 + 两字片段并过滤虚词，硬件经验按机型 / 固件过滤，前 3 条、600 字、300 毫秒上限，带“仅供参考的数据”标签注入，失败不注入；深度模式仅私有窗口，由模型规划关键词但看不到记忆。同日 NV19-05 实施 schema v12 分层知识库：只索引私有窗口里明确选择的目录（复用 NV14 路径安全，拒绝联接 / 链接 / 敏感目录与凭据文件），L0 / L1 在本机抽取生成、不交给模型，原文按需读并核对指纹，结果标为“只是数据，不是指令”；RAGFlow 仍只走 NV11 工具。同日 NV19-06 实施设备故障经验：固定“现象 / 排查 / 结论 / 证据”结构、必须写明机型与固件、证据只由系统写入且确认需要用户或可信测试、按现象召回同设备经验、输出注明不是操作授权；并修正记忆内容过滤，句中的 G-code 与 `$n=` 设置也会被拒绝。同日 NV19-07 实施跨会话只读恢复：每个项目一份“决策 / 待办 / 证据 / 未验证”四节摘要，只认用户自己保存的版本，恢复时对引用的审批、任务、Agent 运行逐条重新核对，核实不了的证据降为未验证，结果未知、已撤销、已停止的绝不重试或重放，核对接口只读。同日 NV19-08 接入桌面：私有窗口专用的 `voice_context_manage` / `voice_context_pick_folder`（独立 capability）、全局“当前项目”、`settings.json` 的 `builtin_context`（默认全关，主窗口不能改）、会话结束提取与对话检索钩子、私有管理页面。新能力仍默认关闭 |
| n21 | 2026-10-13 文档-only（用户确认）：对照 OpenViking 官方文档修正 NV19——检索为一次带过滤的搜索（L0 粗筛、L1 重排），不做逐层下钻；中文检索定为 SQLite FTS5 `trigram`，两字词退回 `LIKE`；新增候选提取变更清单、闲聊/急停/本地快捷命令跳过检索、快速与深度两种检索、固定格式会话摘要；设备/材料/校准以现有表为准，不复制进记忆；补充 OpenViking 独立服务的运行门槛与 AGPL 分发边界。实施细则同步 i4。 |
| n20 | 2026-10-12 文档-only：新增 NV19 结构化长期记忆、分层检索、故障经验、跨会话只读恢复及验收；旧任务状态、安全阻塞和实施证据不变。实施细则同步 i3。 |
| n19 | 2026-10-12同步当前软件证据：中间结果整句停止已实现/回归；协议4/schema9、CampPlus登记、当前句快照/按人隔离和私有摘要编辑；生产识别仍缺真人标定，NV15控制与NV17下载保持关闭，NV18资料收集不代替许可/对应审核。实施细则同步i2 |
| n18 | 文档补齐（2026-10-10）：新增 NV10～NV18 六项实施细则与逐步 TODO；同步 NV8a/NV8b 的软件和未验收状态、NV18 sidecar 接线现状；统一当前版本和全局严格模式规则。记录本会话“中间结果整句命中立即停机”要求与后续安全修补 final-only 实现的差异，保留历史、不宣称代码已恢复。§13.11～§13.13 的实际软件记录不等于实机通过 |
| n17 | 自然打断（2026-10-08 用户决定）：新增 NV8b——设备端回声消除（用户板子 ES8311 + ES7210，MIC3 硬件回采喇叭输出作参考）+ 播放期间插话检测；设计与现有打断机制说明见 §8.3；V2 中“服务端打断”改为做（电脑端回声消除仍不做），见 V7 |
| n16 | 语音体验优化计划（2026-10-08 用户决定）：新增 NV8a（停止类指令本地快速通道、常用只读指令本地匹配、识别热词、本地合成边合成边播、各环节耗时记录），设计见 §8.1；语义判停、端到端语音模型、严格模式关闭时的本地口头确认列为可选项（未决定），见 §8.2 |
| n15 | 本地模型可选：实施记录 §13.10（判停 / 识别 / 合成各可选模型、协议 2、本地流式识别、Matcha 声码器附加下载、TEN-VAD 参数、自动拾音保留开头由 300 ms 调为 1 秒；本机实测） |
| n14 | NV8 代码完成：实施记录 §13.9（多接入点、心跳与空闲断线判定、退避归零、工具开关在列表与调用两处生效、调用统计、接入点身份） |
| n13 | NV7 代码完成：实施记录 §13.8（流式识别 / 流式合成接口、豆包协议移植与修正、会话边说边识别、分段播放、接口类型设置与界面） |
| n12 | NV6 代码完成：实施记录 §13.7（GPL 辅助程序 `moss-voice-local`、帧协议、客户端、Silero 接入 `Vad`、模型下载与校验、本地开关与界面；真实模型端到端测试通过） |
| n11 | 用户决定内置语音的运动与出光跟随全局严格模式（取代 n9 的“始终严格”）；桌面控制类工具仍始终拒绝；系统提示增加“先复述、等口头确认”。见 §5.2、§13.6 |
| n10 | NV5 代码完成：实施记录 §13.5（链路第三项“内置服务”、模型配置与密钥、对话模式启用、顶栏设备状态） |
| n9 | NV4 代码完成：实施记录 §13.4（`CallerContext::Voice`、内置语音始终按严格模式、审批来源 voice、`HostTools`、设备 MCP 中继）；§5.2 的“启用即强制严格模式”改为在执行层按调用方强制 |
| n8 | NV3 代码完成：实施记录 §13.3（新 crate `moss-voice-providers`、对话编排、工具调用循环、唤醒 / 退出 / 空闲）；桌面端接线随 NV5 |
| n7 | NV2 代码完成：实施记录 §13.2；Silero VAD 随本地模型辅助程序移到 NV6（NV2 先用进程内能量 VAD，同一接口）；NV1 本机测试通过 |
| n6 | NV1 代码完成：实施记录写入 §13（协议依据、端口、设备允许流程、回声模式、测试） |
| n5 | NV0 完成：本机实测结果与结论写入 §11.3；Opus 默认纯 Rust 后端 mousiki |
| n4 | NV0 第一部分（源码核实）：记录依赖库结论（§11）；发现 sherpa-onnx 预编译静态库内含 GPL-3.0 的 espeak-ng，提出“本地模型放独立辅助进程”的处理方案（§11.2，待用户确认 V5）；新增 `tools/nv0-probe` 本机测量程序与 `THIRD_PARTY_NOTICES.md` |
| n3 | 用户追加范围：声纹、固件下发、音乐、Home Assistant、RAGFlow。补 §5.7 插件（音乐、Home Assistant、RAGFlow）、新增 §5.9 声纹、§5.10 固件下发，相应更新 §3、§6、§7、§9、§10；计划改为 NV0..NV18 |
| n2 | 用户确认：①模型与插件密钥存 MOSS SQLite 的 `app_secrets` 表；②范围加入第三方 MCP、记忆、视觉、天气与新闻等插件、海门的 Claude Code / Codex 调用；③设备端口沿用 8000 / 8003（可改）；④保留外部服务端链路作兼容。据此补齐各新增范围的设计（§5.4–§5.8）、安全规则（§6）与计划（§8 改为 NV0..NV14）；飞书 / 钉钉 / GitHub 渠道未选，不做 |
| n1 | 初稿：三项目源码调研、功能对照、架构、NV0..NV11 计划、4 项待确认 |

## 0. 结论

- **把小智服务端的能力做进 MOSS 本身**：MOSS 桌面端内置一个“语音服务”，小智设备的 OTA 地址直接指向 MOSS 所在电脑（`http://<电脑局域网IP>:8003/xiaozhi/ota/`）。设备 → MOSS 一跳完成识别 → 大模型 → 合成，不再需要 xiaozhi-esp32-server、mcp-endpoint-server 和 Python 环境。
- **语言**：核心用 Rust，与 MOSS 同一进程，便于直接调用 19 个雕刻工具和审批；界面沿用 React。本地模型用 sherpa-onnx 官方 Rust 绑定（支持 Windows），不需要 Python；因其静态库含 GPL-3.0 组件，放在独立辅助进程 `moss-voice-local.exe` 中运行（§11.2，V5 已确认）。Python 只在用户自己想接的外部服务（如 FunASR torch 版、GPT-SoVITS）中出现，MOSS 以 HTTP 客户端身份调用，不托管它们。
- **取最优**：
  - 设备协议与播放节奏取海门：它是 Rust 写的，代码可直接借用。
  - 对话、工具调度、OTA、VAD 参数、提供商清单、插件、记忆、视觉取 xiaozhi-esp32-server：它最完整，按其行为用 Rust 重写。
  - 多接入点、工具开关与统计、第三方 MCP 聚合取 xiaozhi-client：它思路最好，但要补上它缺的重连和开关校验。
  - 调用 Claude Code / Codex 的方式取海门，另加审批与权限限制。
- **安全不降级**：三个项目都没有审批机制，且管理接口无鉴权、接口回显密钥。MOSS 内置语音一律以“语音会话”身份调用工具：
  - 运动与出光跟随全局严格模式：开启时桌面批准，关闭时遵守现有 confirmed 契约和提示词口头确认；R4 桌面控制始终拒绝。外部 Agent、未放行的第三方工具以及不能免批的家居控制仍需桌面批准，不因激光严格模式关闭而放行。
  - 新设备首次连接需在电脑上允许。
  - 密钥存 `app_secrets`，任何接口都不返回原文。
- **范围**：设备对话主线 + 第三方 MCP、记忆、视觉、插件（天气、新闻、农历、联网搜索、音乐、Home Assistant、RAGFlow）、Claude Code / Codex、声纹、固件下发。只有飞书 / 钉钉 / GitHub 渠道不做。
- **三种链路并存**：官方小智（现有）、**内置服务（新增，主推）**、外部服务端（P4 已完成，保留作兼容）。

## 1. 调研对象

| 项目 | 仓库 / 提交 | 语言 | 许可证 | 定位 |
| --- | --- | --- | --- | --- |
| xiaozhi-esp32-server | xinnan-tech，`7a260ee`（2026-10-07） | Python 3.10（服务端）+ Java/Vue（管理端） | MIT | 完整的小智自建服务端：设备协议、OTA、VAD、几十个 ASR/LLM/TTS 提供商、插件、MCP、记忆、视觉、管理后台 |
| haimen（海门） | shenjingnan，`513d9e6`（2026-08-08） | Rust（tokio + axum） | MIT | AI 网关 CLI：飞书 / 钉钉 / GitHub 渠道 + 外部 Agent CLI（Claude Code、Codex 等）；含原生小智设备服务端 `haimen-xiaozhi` |
| xiaozhi-client | shenjingnan，`fe3c382`（2026-05-01，v2.4.0-beta.3） | TypeScript / Node | MIT | 以工具提供方身份连小智 MCP 接入点，聚合多个第三方 MCP 服务；Web 管理界面 |

三者均为 MIT 许可：可以复制和修改，但须保留原版权与许可声明。NV0 建 `THIRD_PARTY_NOTICES.md`，被借用的源文件头部注明出处与提交号。

## 2. 各项目要点

只列与融合相关、且已读源码核实的事实。

### 2.1 xiaozhi-esp32-server（`main/xiaozhi-server`）

- **设备协议**：
  - 传输为 WebSocket，默认 8000 端口，不校验路径。
  - 身份取 `device-id`、`client-id`、`Bearer` 请求头，缺失时取 URL 参数。
  - token 为 `base64url(HMAC-SHA256(secret,"client_id|device_id|ts")).ts`，默认 30 天有效；`allowed_devices` 白名单内的设备免校验（`core/auth.py`、`core/websocket_server.py`）。
  - 入站消息：`hello / listen(start|stop|detect) / abort / iot / mcp / server / ping`；出站消息：`stt / tts(start|sentence_start|stop) / llm(表情) / mcp / iot`。
  - 上行 Opus 16 kHz，下行 24 kHz，均为 60 ms 帧。
- **OTA**：`POST /xiaozhi/ota/`（8003 端口）返回 `server_time / firmware / websocket{url,token}`。固件从 `data/bin/{型号}_{版本}.bin` 按版本比较后下发（`core/api/ota_handler.py`）。激活码流程只在 Java 管理端。
- **VAD**：Silero onnx 模型（随仓库附带，2.3 MB），双阈值 0.5 / 0.3，静音 200 ms 判停（`core/providers/vad/silero.py`）。
- **提供商**：
  - ASR：本地有 FunASR（依赖 torch）、sherpa-onnx、Vosk；云端有豆包、阿里、百度、腾讯、OpenAI、讯飞（部分为流式）。
  - LLM：OpenAI 兼容（覆盖阿里、豆包、DeepSeek、智谱、LMStudio）、Ollama、Gemini、Dify、Coze、FastGPT、Home Assistant 等。
  - TTS：Edge（默认）、豆包 / 阿里 / 讯飞双向流式、OpenAI、硅基流动，以及 FishSpeech、GPT-SoVITS 等外部 HTTP 服务。
- **对话**：
  - 首句遇到逗号就切，降低首包延迟。
  - 意图模式有 `function_call`（默认）、`intent_llm`、`nointent` 三种。
  - 唤醒词缓存回复；退出词直接结束；空闲 120 秒播结束语。
- **记忆与视觉**：
  - 记忆 `mem_local_short`：会话结束时用 LLM 总结，写入文件（`core/providers/memory/`）。
  - 视觉：`POST /mcp/vision/explain`，表单字段为 `question` 和 `image`（≤5 MB），用 JWT 鉴权后调用视觉模型。地址与 token 在设备 MCP `initialize` 的 `capabilities.vision` 中下发（`core/api/vision_handler.py`、`core/providers/tools/device_mcp/mcp_handler.py`）。
- **插件**（`plugins_func/functions/`）：天气（和风天气）、新闻（newsnow、中新网）、农历、音乐、角色切换、退出、Home Assistant、RAGFlow、联网搜索（metaso / tavily / serply）、拨打设备。
- **工具调度**：
  - 统一调度 5 类执行器：服务端插件、服务端 MCP、设备 IoT、设备 MCP、MCP 接入点。
  - 多个工具调用并行执行，各 30 秒超时；结果按 `RESPONSE / REQLLM / RECORD` 三类处理；最多递归 5 层；另注入虚拟工具 `direct_answer`（`core/connection.py`）。
  - **没有任何审批**；同名工具后注册的覆盖先注册的，只打一条警告。
- **弱点**：
  - 核心类 `connection.py` 有 1852 行，线程与 asyncio 混用。
  - CI 只跑冒烟测试。
  - 密钥明文写在 YAML 里，默认配置还带了一个共享的天气 key。
  - 对话历史没有长度上限。

### 2.2 haimen

- **`crates/haimen-xiaozhi`**（约 2060 行，38 个测试）：
  - 用 axum 实现 `POST /xiaozhi/ota/` 与 `GET /xiaozhi/ws`。
  - 上行同时识别二进制协议 v2（16 字节头）、v3（4 字节头）与裸 Opus。
  - 每个连接一个状态机：等待 hello → 就绪 → 录音 → 播放。
  - **播放节奏最好**：先预缓冲约 600 ms，再按绝对时钟每 60 ms 发一帧；打断时用取消守卫立即停（`ws.rs`）。
  - `ResponseStrategy` trait 把“录音完成后生成什么”做成插件接口（`strategy.rs`）。
- **对话管线**（`src/xiaozhi_asr_llm_tts.rs`，91 个测试），按顺序：
  1. 流式 ASR，经 `univoice` crate 调用豆包、通义、智谱、MiMo、讯飞。
  2. 文本流切句：按中文标点切，清洗 Markdown，超过 180 字或 6 秒强制送出。
  3. 流式 TTS：豆包、通义、智谱、OpenAI、MiniMax。
  4. **连续性泵**：只用一个 Opus 编码器，没有内容时补静音帧，避免设备断音。
- **Agent 调用**：
  - 所谓 LLM 其实是用子进程调用 Claude Code（`claude --print --output-format stream-json …`）或 Codex 等外部 CLI（`crates/haimen-claude-code`、`crates/haimen-codex`）。
  - 会监听工具使用事件，播报“正在搜索”“正在执行命令”之类的提示。
  - **调用时不传任何权限限制**；Codex 默认沙箱为 `danger-full-access`。
- **不足**：
  - 没有 OpenAI function calling。
  - 不处理设备 MCP、`iot` 和 `llm` 表情消息。
  - OTA 不签发 token。
  - VAD 只有能量阈值加 ASR 端点信号。
  - **全进程只有一个策略实例，多台设备会互相串状态。**
  - 密钥明文存放，GET 接口原样返回。
  - 管理端口 `0.0.0.0:9527` 没有鉴权。
  - 每次回复都写 WAV 文件到用户目录。
- `univoice` 是 crates.io 上的 0.1.6 版，源码与许可证本次没有核实，留待 NV0。

### 2.3 xiaozhi-client

- **能力**：
  - 以工具提供方身份用 WebSocket 连小智接入点，**同一套工具可同时上报多个接入点**（`src/endpoint/manager.ts`）。
  - 作为 MCP 客户端聚合 stdio / SSE / streamable-http 下游服务，工具统一命名为 `服务名__工具名`。
  - 每个工具有启用开关，调用次数与最近使用时间写回配置。
  - 下游每 30 秒 ping 一次；遇到 401 自动重连后再重试一次。
  - Coze 工作流 8 秒超时后转到后台执行，结果缓存，下次同参数调用直接取。
  - 能解析接入点 token 中的 JWT 身份与过期时间。
- **名不副实之处**（读代码核实）：
  - 上报给接入点的工具列表调用 `getAllTools()`，默认取 `"all"`，**禁用的工具仍会上报，调用时也不检查开关**。
  - 与接入点**断线后不自动重连**，相关配置项只用于界面显示。
  - 重连退避时间不会增长。
  - 自定义工具声明了 6 种类型，只有 Coze 真正可用。
- **安全问题**：管理接口 `0.0.0.0:9999` 无鉴权、CORS 为 `*`，可以远程添加 stdio 命令（等于远程执行任意命令）；`GET /api/config` 原样返回全部密钥。
- 也有一个实验性的设备直连链路（单轮对话，不调用工具），实现很浅，不采用。

## 3. 功能对照与取舍

表中“采用”一列说明内置语音服务采用谁的做法；“MOSS 现有”指已在代码中的能力。

### 3.1 设备与音频

| 功能 | esp32-server | 海门 | xiaozhi-client | MOSS 现有 | 采用 |
| --- | --- | --- | --- | --- | --- |
| 设备 WebSocket 协议 | 最全 | Rust 实现，消息类型少 | 浅 | 无 | 以海门代码为底，补齐 esp32-server 的消息类型；每台设备一个会话 |
| 设备鉴权 | HMAC token + 白名单 | 无 | 无 | 局域网配对 token（给页面用） | esp32-server 的 HMAC token，加 MOSS 规则：新设备首次连接需在电脑上允许 |
| OTA | 完整，含固件下发 | 最简 | 有 | 无 | esp32-server 的语义，用 Rust 写 |
| 播放节奏、防断音 | 有流控 | **绝对时钟 + 预缓冲 + 静音补帧** | — | — | 海门 |
| Opus 编解码 | opuslib（需系统库） | opus2（静态编译 libopus） | — | 无 | opus2 静态编译 |
| VAD | **Silero 双阈值** | 能量阈值 + ASR 端点 | — | 无 | Silero（经 sherpa-onnx，参数取 esp32-server），流式 ASR 的端点信号作补充 |
| 打断 | 设备 abort + 服务端检测 + AEC | 仅设备 abort | — | — | 先做设备 abort |
| 固件下发 | 按型号与版本比较，下载接口 | 无（固件块写死为空） | 无 | 无 | esp32-server 的规则，加“用户逐个型号确认才推送”（§5.10） |
| 唤醒词、退出词、空闲断开 | 有 | 唤醒问候 | — | — | esp32-server |

### 3.2 模型

| 功能 | esp32-server | 海门 | xiaozhi-client | MOSS 现有 | 采用 |
| --- | --- | --- | --- | --- | --- |
| 本地 ASR | FunASR(torch)、sherpa-onnx、Vosk | 无 | 无 | 无 | sherpa-onnx（SenseVoice / Paraformer） |
| 云端 ASR | 很多 | 5 家流式 | — | P4 测试连接 | OpenAI 兼容 + 豆包流式 |
| LLM 与工具调用 | **流式 + function calling + 递归 5 层** | 无（外部 CLI） | 单轮 | P4 测试校验 `tool_calls` | 按 esp32-server 的设计用 Rust 重写；支持 Ollama，Anthropic 可选 |
| 切句与合成 | 首句逗号切 | **Markdown 清洗、聚合、强制送出** | — | — | 两者合并 |
| 本地 TTS | 无（全是外部 HTTP 服务） | 无 | 无 | 无 | sherpa-onnx（Kokoro / VITS / Matcha） |
| 云端 TTS | 很多 | 5 家流式 | — | P4 测试连接 | OpenAI 兼容 + 豆包流式 |

### 3.3 工具与扩展

| 功能 | esp32-server | 海门 | xiaozhi-client | MOSS 现有 | 采用 |
| --- | --- | --- | --- | --- | --- |
| 工具调度 | 5 类执行器，无审批，重名静默覆盖 | — | 前缀命名、开关（未生效） | 19 个工具、R0–R4 分级、严格模式、桌面审批 | MOSS 统一工具路由：前缀命名、重名拒绝、开关在列表与调用两处都校验、所有调用都过风险分级与审批 |
| 设备自带 MCP | 有 | 无 | 只记日志 | 无 | esp32-server |
| 官方 MCP 接入点 | 客户端 | — | **多接入点**（不重连） | 单接入点桥，带退避重连 | MOSS 现有的桥，扩展为多接入点并补上断线重连 |
| 第三方 MCP 聚合 | stdio / SSE / http | 代码有但未接线 | **最完善** | 无 | xiaozhi-client 的思路，用 Rust `rmcp` 实现（§5.4） |
| 工具开关与统计 | — | — | 有（未生效） | 无 | xiaozhi-client 的思路，开关真正生效 |
| 记忆 | 短期摘要、mem0 等 | 外部 CLI 续接会话 | — | 无 | 短期摘要，存 SQLite（§5.5） |
| 视觉 | 有 | 无 | 无 | 无 | esp32-server（§5.6） |
| 插件 | 天气、新闻、农历、搜索、音乐、Home Assistant、RAGFlow 等 | 无 | Coze | 无 | esp32-server 的插件，用 Rust 重写（§5.7） |
| 声纹 | 调外部 `voiceprint-api` 服务 | 无 | 无 | 无 | **本地 sherpa-onnx 说话人特征**（不需要外部服务），兼容外部 `voiceprint-api`（§5.9） |
| 外部 Agent CLI | 无 | **有**（无权限限制） | 无 | 无 | 海门的调用方式，加审批与只读限制（§5.8） |
| 飞书 / 钉钉 / GitHub | 无 | 有 | 无 | 无 | **不做**（用户未选） |
| 管理界面 | Java + Vue 多用户后台 | React 控制台 | React 控制台 | 语音中心（P4） | 扩展 MOSS 语音中心；不引入 Java 后台、激活码、MQTT 网关 |
| 密钥保存 | 明文 YAML | 明文 TOML，接口回显 | 明文 JSON，接口回显 | Jev Key 已存 `app_secrets` | **存 `app_secrets`**，任何接口都不返回原文（§6） |

## 4. 目标架构

```text
小智设备 ──OTA(http://<电脑IP>:8003/xiaozhi/ota/)───────┐
        ──WebSocket(ws://<电脑IP>:8000/xiaozhi/v1/)─────┤
        ──视觉(http://<电脑IP>:8003/mcp/vision/explain)─┤
                                                         ▼
┌───────────────────── moss-desktop（同一进程）─────────────────────┐
│ moss-voice-server（新，仅桌面端，独立 tokio 运行时）              │
│   设备协议/鉴权 → 会话(每设备一个) → VAD → ASR → LLM(工具调用)    │
│        → 切句聚合 → TTS → Opus → 播放节奏泵 → 设备                │
│   记忆 · 视觉接口 · 声纹识别 · 固件下发                           │
│   工具路由 ─┬─ MOSS 19 个工具：InProcessBackend，CallerContext::Voice│
│            ├─ 设备自带 MCP（经同一 WebSocket）                    │
│            ├─ 插件：天气/新闻/农历/搜索/音乐/Home Assistant/RAGFlow│
│            ├─ 第三方 MCP（rmcp，默认每次需批准）                   │
│            └─ Agent：Claude Code / Codex（每次需批准，只读）       │
│ laser-host / laser-api（现有）：ApiService、审批、审计、急停       │
│ laser-mcp 桥（现有）：官方接入点，扩展为多接入点                   │
└───────────────────────────────────────────────────────────────────┘
     本地模型：sherpa-onnx（ASR / TTS / VAD），模型文件按需下载到运行目录
     云端模型：OpenAI 兼容 / Ollama / 豆包流式 等
     密钥：SQLite app_secrets
```

新增 crate 都只在桌面目标编译，Android 不包含：

| crate | 内容 | 主要来源 |
| --- | --- | --- |
| `moss-xiaozhi-proto` | 消息类型、二进制帧 v2/v3 解析、hello 参数、设备 token 签发与校验、OTA 请求 / 响应 | 海门 `protocol.rs`、`types.rs`（借用代码）+ esp32-server 协议（按行为重写） |
| `moss-voice-server` | WebSocket / OTA / 视觉 HTTP 服务（axum）、每设备会话状态机、播放节奏泵、VAD、对话编排、记忆、工具路由、设备 MCP 中继 | 海门 `ws.rs` 的节奏逻辑 + esp32-server 的编排逻辑 |
| `moss-voice-providers` | ASR / LLM / TTS / VLLM / VAD 的 trait 与实现：OpenAI 兼容、Ollama、sherpa-onnx 本地、豆包流式 | 新写；参考 esp32-server 提供商与海门 `tts_factory` |
| 工具扩展（原拟名 `moss-voice-tools`，未建 crate） | 当前第三方客户端已在 `moss-mcp-client` / `laser-host::external_mcp`；NV11/NV14 拟在 `moss-voice-server` 内置模块组合接入，NV13/NV15 在宿主增加专用审批执行者。详见实施细则，不再要求新增统一插件 crate | esp32-server 插件、xiaozhi-client、海门 |
| `moss-voice`（现有） | 语音中心设置、外部服务端进程管理、模型测试、日志 | P4 |

- **运行时**：MOSS 目前没有 tokio。新 crate 自带一个专用的多线程 tokio 运行时（2–4 个线程），只在用户启用内置语音时启动。现有 HTTP 服务、发送线程与急停路径不变（G4：语音服务出错不影响加工与急停）。
- **端口**：设备 WebSocket 默认 **8000**，OTA 与视觉默认 **8003**，与上游一致，可在语音中心修改。
  - 两个端口都监听 `0.0.0.0`，因为局域网里的设备要连得上。
  - 它们与 MOSS 的 HTTP API 分开，只提供 `/xiaozhi/*`（含 `/xiaozhi/ota/download/*`）与 `/mcp/vision/explain`，不暴露任何 MOSS 接口。
  - 首次启用时 Windows 防火墙会询问是否允许，语音中心给出提示。

## 5. 运行规则

### 5.1 会话与对话

1. **每台设备一个会话**：会话持有自己的 VAD、ASR 流、对话历史、Opus 编解码器与播放泵，多台设备互不影响（修复海门的单实例问题）。
2. **LLM 调用**：
   - 用 OpenAI 兼容的 `chat/completions` 流式接口，累积 `tool_calls`。
   - 多个工具并行执行，各 30 秒超时。
   - 结果按“直接播报 / 回填后再问 LLM / 只记入历史”三类处理，最多 5 轮（esp32-server 的规则）。
   - 对话历史按 token 预算截断（esp32-server 缺这一条）。
3. **切句**：首句遇到逗号就送合成，降低首包延迟；之后按句末标点切。清洗 Markdown；超过 180 字或 6 秒强制送出。
4. **唤醒、退出、空闲**：唤醒词命中时播放缓存的问候；退出词直接结束会话；空闲 120 秒（可改）后播结束语再断开。

### 5.2 工具身份与审批

- 内置语音调用任何工具时，身份为新增的 `CallerContext::Voice { device_id, session_id }`，审计记录的 `client` 为 `voice:<设备>`。
- **内置语音的运动与出光跟随全局严格模式**（n11，2026-10-07 用户决定；n9 曾为“始终严格”）：`MOSS_FUSION_STRICT` 开启时 R3 只生成桌面审批；关闭时与官方接入点相同，模型带 `confirmed=true` 即执行，系统提示要求模型先复述参数并等用户口头确认。命令执行、打开应用、快捷键、打开网站等桌面控制工具不论开关始终拒绝。
- 各类工具的处理：

  | 工具类别 | 风险级别 | 处理 |
  | --- | --- | --- |
  | MOSS 查询与生成类 | R0 / R1 | 直接执行 |
  | MOSS 运动与出光类 | R3 | **严格模式开启**：仅生成桌面审批并播报“已发到电脑，请在电脑上批准”；**关闭**：沿用现有 `confirmed=true` 参数契约与提示词的口头确认要求，模型文字不等于桌面批准 |
  | 设备自带 MCP | R1 | 直接执行 |
  | 信息类插件（天气、新闻、农历、搜索、RAGFlow）与音乐 | R0 | 直接执行 |
  | Home Assistant 查询 | R0 | 直接执行 |
  | Home Assistant 控制 | 默认需批准 | 每次调用都要桌面批准，可按设备放行（§5.7） |
  | 第三方 MCP | 默认需批准 | 每次调用都要桌面批准，可在语音中心逐个放行（§5.4） |
  | Agent CLI | 需批准 | 每次调用都要桌面批准（§5.8） |
  | 未分级工具 | — | 拒绝 |

- 审批种类在现有 `ApprovalKind` 上新增三种：`external_tool`（第三方 MCP）、`agent_run`（Agent CLI）、`home_control`（Home Assistant 控制）。审批卡显示工具名、参数摘要与来源设备；摘要规则沿用 §5.4。

### 5.3 设备自带 MCP

设备在 hello 中声明 `features.mcp` 时，服务端依次发 `initialize`（其中下发 `capabilities.vision`，见 §5.6）、`tools/list`（支持分页）。设备工具以 `device__` 为前缀进入工具路由。

### 5.4 第三方 MCP 工具（取 xiaozhi-client，补其缺陷）

- **添加方式**：只能在 MOSS 桌面窗口的语音中心添加，没有远程接口。
- **传输**：支持 stdio（本机命令）与 streamable-http。SSE 视 `rmcp` 当时的支持情况决定，在 NV9 核实。
- **配置存放**：
  - 服务器列表存 `settings.json` 的 `voice.mcp_servers`，包括名称、类型、命令与参数或 URL、是否启用、每个工具的策略。
  - 请求头、环境变量里的密钥存 `app_secrets`，`settings.json` 只保存引用名。
- **命名**：工具名为 `服务名__工具名`；与已有工具重名时拒绝加载，并在界面提示。
- **策略**：每个工具三选一——“禁用”、“每次需批准”（默认）、“免批准”。只有用户确认该工具只读时，才应设为免批准，界面会提示风险。
  - 工具列表与调用两处都校验策略（修复 xiaozhi-client 禁用无效的问题）。
- **连接维护目标**：每 30 秒心跳；断线按 1 秒到 60 秒指数退避重连；调用超时 30 秒。2026-10-10 安全修订：`tools/call` 返回 401、404、传输错误或超时时均不自动重放，结果不确定时明确报告；重新建立连接不能补发旧调用，新调用重新经过权限与审批。NV9 宿主现已接入独立连接维护任务、空闲心跳及清单刷新；详细边界见 §13.13。
- **给 LLM 的限制**：单个工具描述截断到 200 字；外部工具总数上限 64 个，超出时提示用户禁用一部分，以控制上下文长度。
- **统计**：记录每个工具的调用次数与最近使用时间，在语音中心显示。

### 5.5 记忆（取 esp32-server `mem_local_short`）

- 会话结束时，在后台用当前 LLM 把本次对话总结进该设备的长期摘要，上限 1500 字。
- 摘要存新表 `voice_memory(device_id, summary, revision, updated_at)`，下次会话注入 system prompt。
- 语音中心可以查看、编辑、清空每台设备的记忆；可整体关闭记忆。
- 只保存文字摘要，不保存音频。
- **NV19 衔接**：当前 NV10 是有界摘要，而非可按来源/状态检索的多条记忆；后续保留原存储与 UI，新增按条目管理、可信归属、候选确认及按需检索（§15）。

### 5.6 视觉（取 esp32-server）

- 设备 MCP `initialize` 时下发 `capabilities.vision`，内容为 `{url: http://<电脑IP>:8003/mcp/vision/explain, token}`。
- 设备拍照后，带 `question` 与 `image`（≤5 MB）请求该接口；token 按设备签发，校验方式与设备 token 相同。
- MOSS 调用用户配置的视觉模型（OpenAI 兼容多模态接口），把回答文字返回设备。
- 图片只在内存中处理，不落盘。

### 5.7 插件（取 esp32-server，用 Rust 重写）

| 插件 | 数据来源 | 密钥 | 说明 |
| --- | --- | --- | --- |
| 天气 `get_weather` | 和风天气 API | 用户自己的 key（`app_secrets`）；**不内置任何共享 key** | 默认城市可在语音中心设置 |
| 新闻 `get_news` | newsnow、中新网 RSS | 无 | 可选新闻源；只播报标题与摘要 |
| 农历 `get_lunar` | 本地计算 | 无 | 农历日期、节气、宜忌 |
| 联网搜索 `web_search` | tavily / metaso 等 | 用户自己的 key | 结果摘要回填 LLM |
| 音乐 `play_music` | 本机音乐目录（默认运行目录 `music/`，可改） | 无 | 按歌名模糊匹配或随机播放；用 Rust `symphonia` 解码 mp3 / flac / wav / ogg，重采样到 24 kHz 后经同一播放泵送设备；可被唤醒或 abort 打断；不联网下载 |
| Home Assistant `hass_get_state` / `hass_set_state` / `hass_play_music` | 用户自己的 Home Assistant（REST API） | 长期访问令牌（`app_secrets`） | 见下文 |
| 知识库 `search_rag` | 用户自己的 RAGFlow（检索 API） | RAGFlow API key（`app_secrets`） | 可选数据集；只返回命中片段摘要（总长上限 2000 字）回填 LLM |

- 天气、新闻、农历、联网搜索、知识库、音乐都是 R0 只读工具；Home Assistant 的控制类调用另按下面的规则审批。每个插件可在语音中心单独启用或关闭。
- 网络请求 10 秒超时，不跟随跨域名重定向。
- **NV19 衔接**：现有 `search_rag`/RAGFlow 是可选的外部只读检索基础，未来只复用其数据集白名单和来源，不建立重复连接，也不放宽网络门禁。

- **Home Assistant 规则**：
  - 语音中心填写 HA 地址与令牌，从 HA 拉取实体列表，用户勾选要交给小智的设备并起中文名；只有勾选的实体出现在工具描述里。
  - 查询直接执行。控制默认每次需桌面批准（审批种类 `home_control`），可按实体改为免批准；门锁（`lock`）、安防（`alarm_control_panel`）、车库门与卷帘（`cover`）始终需要批准，不能放行。
  - 只调用 HA 的 `states` 与 `services` 接口；请求 10 秒超时；令牌不出现在日志与接口响应中。
- 角色切换、拨打设备两个插件本期不做（与单用户桌面场景无关）。

### 5.8 Claude Code / Codex 调用（取海门，加限制）

- **形式**：以工具 `agent_claude_code`、`agent_codex` 的形式提供给 LLM，例如用户说“让 Claude 帮我写个……”时由 LLM 调用。只有本机已安装对应 CLI 时，这两个工具才出现。
- **审批**：每次调用都生成 `agent_run` 审批。审批卡显示完整任务文字与工作目录，批准后才启动子进程。
- **权限**：
  - 工作目录固定为运行目录下的 `agent-workspace/`（可改，但不允许设为系统目录或用户主目录根）。
  - Codex 用只读沙箱；Claude Code 用不允许写文件、不允许执行命令的权限模式。
  - 具体命令行参数在 NV13 按用户安装的 CLI 版本核实；**不使用** `danger-full-access` 或跳过权限确认的参数。
- **运行过程**：
  - 超时 5 分钟。
  - 执行中按海门的做法播报“正在搜索”“正在读取文件”之类的进度。
  - 结束后把结果摘要交给 LLM 播报；完整输出存语音中心的 Agent 记录，最多保留 50 条。
- **限制**：Agent 不能调用 MOSS 的激光工具，子进程不继承 MOSS 的 MCP 连接。
- **NV19 衔接**：跨会话恢复只是只读回忆与当前证据核对，不自动重启 CLI、继承旧审批或执行先前的任务。NV13 生产执行安全锁保持不变。

### 5.9 声纹（esp32-server 依赖外部服务；MOSS 改为本地）

**当前实现边界（2026-10-12）**：固定CampPlus模型和协议4，独立声纹进程、Windows模型锁、私有窗口三段登记/取消/CAS删除与SQLite已接入。当前句可信身份快照、top1/margin、按人历史/摘要隔离、私有本地摘要编辑及桌面适配器已实现；设备/会话/授权代次绑定使撤销后旧结果失效，未知者不回退共享摘要。普通识别与ASR并行、1.5秒有界等待，停止/退出不等待。生产`Calibration=None`，真人阈值未标定时识别和按人注入仍关闭；可选仅登记者限制、姓名提示、WAV导入和外部兼容未实施。接口和hash见 [NV16登记说明](nv16-speaker-enrollment.md)，不得把以下目标描述当作已完成证据。

- **用途**：识别说话人，把称呼注入 system prompt（“现在说话的是爸爸”），记忆按人区分；可选“只有已登记的人能发起加工请求”——未登记的人说“开始雕刻”时直接拒绝，已登记的人仍需桌面批准（不替代审批）。
- **实现**：本地用固定CampPlus模型提取特征向量，与已登记者做余弦相似度比较；阈值和margin须来自已接受的真人标定，设置项不能代替标定。与ASR并行，普通句在ASR后最多等启动起1.5秒，停止/退出走快速路径。外部 `voiceprint-api` 是未启用的可选兼容方案。
- **登记**：在语音中心点“登记声纹”，对设备说 3 句提示语（或上传 wav），提取特征后保存。
- **存储与隐私**：只存特征向量与名字（新表 `voice_speakers(id, name, embedding, created_at)`），**不保存原始录音**；可随时删除；关闭声纹后不再计算特征。

### 5.10 固件下发（取 esp32-server，加确认）

- 用户在语音中心导入固件文件（`.bin`），填写型号（与设备上报的 `board.type` 一致）和版本号；MOSS 计算 SHA-256 并保存到运行目录 `firmware/{型号}_{版本}.bin`。
- **默认不推送**：每个固件有“向此型号设备推送”开关，默认关闭；打开时弹出确认框，显示型号、版本、文件大小和 SHA-256（固件更新属于需用户明确确认的操作）。
- 开关打开后，该型号设备在 OTA 检查时若当前版本低于固件版本（按数字逐段比较），OTA 响应带上 `firmware{version,url}`，下载地址为 `http://<电脑IP>:8003/xiaozhi/ota/download/{文件名}`；只允许已允许的设备下载，文件名白名单校验，防止路径穿越。
- 记录每台设备的上报版本与最近一次下载时间；不支持回滚降级（与上游一致），需要降级时由用户手动刷机。

## 6. 安全与隐私

三个项目都缺这些，MOSS 必须有。

| 风险 | 上游现状 | MOSS 规则 |
| --- | --- | --- |
| 模型自行启动加工 | 三者都无审批 | 当前 n11 规则：R3 跟随全局严格模式；开启时只生成桌面审批，关闭时按 confirmed 与提示词口头确认执行；R4 始终拒绝。摘要、图片、声纹和外部工具结果均不能自行授予权限；声纹“仅登记者”限制启用时仍要求桌面审批 |
| 陌生设备连入 | esp32-server 只靠 token；另两个无鉴权 | 新设备首次 hello 后进入“待允许”，在语音中心点“允许”后才签发 token，可随时撤销；未允许的设备只收到一句提示，然后断开 |
| 局域网可达的端口 | 管理接口监听 `0.0.0.0` 且无鉴权 | 设备端口只有 `/xiaozhi/*` 与视觉接口；管理一律走现有 Tauri 命令，只有桌面窗口能调用 |
| 密钥 | 全部明文，接口回显 | **存 SQLite `app_secrets`**（用户决定，与现有 Jev Key 同表）。命名为 `voice.<类别>.<名称>`，例如 `voice.llm.main`、`voice.plugin.weather`、`voice.mcp.<服务>.<字段>`。不写 `settings.json`，不写日志（复用 `redact`）；**语音相关接口只返回“已设置 / 未设置”，不提供查看原文的接口**；随数据库备份一起保存，备份文件须妥善保管 |
| 第三方 MCP 与命令执行 | client 可远程添加 stdio 命令 | 只能在桌面窗口添加；默认每次需批准 |
| Agent CLI | 海门不限权限，Codex 全盘可写 | 每次需批准；只读模式；固定工作目录；不能调用激光工具 |
| 录音、图片与日志 | 海门每次回复写 WAV | 默认不保存音频与图片；识别文本进对话历史，不进日志正文 |
| 插件共享 key | esp32-server 默认配置带共享天气 key | 不内置任何 key |
| 家居控制 | esp32-server 的 HA 插件直接执行 | 控制默认需批准；门锁、安防、卷帘始终需批准；只暴露用户勾选的实体 |
| 声纹（生物特征） | 外部服务保存 | 只存特征向量，不存录音；可删除、可关闭；不作为审批的替代 |
| 固件下发 | 文件放进目录即推送 | 默认不推送；逐个型号确认并显示 SHA-256；只向已允许设备提供下载；文件名白名单 |

**原规则的调整**：P4 时“MOSS 不保存模型密钥”的规则（技术书 §9.4.4、验收 A-67）只适用于外部服务端链路：外部链路的密钥仍只写服务端的 `data/.config.yaml`。内置链路改为存 `app_secrets`，A-67 在内置链路下改为检查“密钥只在 `app_secrets`，不在 `settings.json`、日志与任何接口响应中”。

## 7. 本地与云端模型

| 能力 | 本地（sherpa-onnx，Rust 静态链接） | 云端 |
| --- | --- | --- |
| VAD | Silero（随安装包，约 2 MB） | — |
| ASR | SenseVoice（中英粤日韩；int8 模型约两百多 MB，NV0 核实）、Paraformer、流式 Zipformer | OpenAI 兼容 `/audio/transcriptions`；豆包流式；其余按需 |
| LLM | Ollama / LM Studio（走 OpenAI 兼容协议，由用户自行安装） | OpenAI 兼容（DeepSeek、通义、豆包、智谱等）；Anthropic 可选 |
| 视觉 | Ollama 多模态模型（可选） | OpenAI 兼容多模态接口 |
| TTS | Kokoro（中英）、VITS、Matcha | OpenAI 兼容 `/audio/speech`；豆包流式 |
| 声纹 | 说话人特征模型（3D-Speaker / WeSpeaker 类） | 外部 `voiceprint-api`（可选） |
| 唤醒词 | 设备端完成，服务端不做 | — |

- 本地模型**不打进安装包**。语音中心提供“下载模型”，从 sherpa-onnx 官方发布地址下载到运行目录 `models/`，并校验 SHA-256；也可以指定已下载好的目录。
- 模型体积与 CPU 占用在 NV0 实测后写入本文。
- 用户仍可接自建的 FunASR 服务、GPT-SoVITS、FishSpeech 等，它们本来就是 HTTP 服务，MOSS 只作为客户端调用。

## 8. 分阶段计划（NV = Native Voice）

每个阶段都按 AGENTS.md 执行：改代码 → CHANGELOG → 测试 → 构建；需要实机验证的标为待验证。

| 阶段 | 内容 | 主要来源 | 验收（摘要） |
| --- | --- | --- | --- |
| NV0 前置 | 锁定三项目提交号；建 `THIRD_PARTY_NOTICES.md`；核实 `univoice` 许可证与源码；在 Windows 上试编 `opus2`（bundled）与 `sherpa-onnx`（static），记录安装包增量与编译要求；实测 SenseVoice / Kokoro 的延迟；核实 `rmcp` 版本与传输支持 | — | 结论写入本文 |
| NV1 协议与 OTA | `moss-xiaozhi-proto`、`moss-voice-server` 骨架：OTA、hello、设备允许流程、token、回声模式 | 海门 + esp32-server | 真实设备的 OTA 指向 MOSS 后能连上并听到回声；未允许的设备被拒 |
| NV2 音频 | Opus 编解码、播放节奏泵、VAD 接口与进程内能量 VAD、打断（设备 abort）。Silero 需要本地模型辅助程序（§11.2），随 NV6 接入同一接口（n7 调整） | 海门、esp32-server | 无断音、跳字；静音 200 ms 判停；打断后立即停播 |
| NV3 云端对话 | OpenAI 兼容 ASR / LLM（流式 + 工具调用）/ TTS；切句聚合；对话历史截断；唤醒、退出、空闲 | esp32-server、海门 | 唤醒 → 问答 → 合成完整走通；记录首句延迟 |
| NV4 工具与审批 | 统一工具路由；MOSS 19 个工具以 `CallerContext::Voice` 调用；设备 MCP 中继；R3 随全局严格模式决定是生成审批还是沿用 `confirmed` 契约 | esp32-server + MOSS | **开启严格模式**：“雕刻你好”只生成审批，批准后才发送；**关闭**：复测既有 `confirmed` 与口头确认提示词路径，不能把模型填参当桌面授权；查询类直接执行、R4 始终拒绝，实机另验 |
| NV5 语音中心与密钥 | 链路三选一（官方 / 内置 / 外部）；内置服务启停与状态；设备允许列表；提供商配置；密钥存 `app_secrets` | MOSS P4 | 页面操作全部可用；密钥不出现在 `settings.json`、日志与接口响应中 |
| NV6 本地模型 | 本地模型辅助程序 `moss-voice-local.exe`（GPL-3.0，§11.2）；Silero VAD（接入 NV2 的 `Vad` 接口）、sherpa-onnx ASR / TTS；模型下载与校验 | sherpa-onnx | 断网时本地链路可用；Silero 与能量 VAD 可切换 |
| NV7 流式云厂商 | 豆包流式 ASR / TTS（用核实过的 `univoice` 或自写） | 海门 | 首句延迟明显低于批量 |
| NV8 接入点增强 | 官方接入点多地址、断线自动重连、工具开关（列表与调用都校验）、调用统计、接入点身份显示 | xiaozhi-client | 禁用的工具不上报，也不可调用 |
| NV8a 体验优化 | 停止类指令本地快速通道；常用只读指令本地匹配；识别热词；本地合成边合成边播；各环节耗时记录（§8.1） | Home Assistant 本地意图优先、sherpa-onnx 热词 | 断网或大模型出错时喊“停”仍能急停；查状态不经过大模型；耗时逐环节可见（实机数据随 §2a 统一测试） |
| NV8b 自然打断 | 设备端回声消除的“实时对话”模式下，播放期间继续听，用户连续说话够长即自动打断并接着识别这一句；打断时历史只记已播出的句子（§8.3） | 小智固件实时模式 + esp32-server 的播放中打断 | 播放时直接说话即停下并完整识别；播放自身声音与环境噪声不误打断（实机见 §2a HW-10） |
| NV9 第三方 MCP | `rmcp` 客户端（stdio / http）、前缀命名、心跳重连、三档策略、`external_tool` 审批、统计 | xiaozhi-client | 外部工具默认每次都要批准；禁用的工具不可调用；重名被拒 |
| NV10 记忆 | `voice_memory` 表（schema 迁移）、会话结束总结、注入、查看 / 编辑 / 清空 | esp32-server | 第二次会话能引用上次的内容；关闭记忆后不再写入 |
| NV11 信息插件 | 天气、新闻、农历、联网搜索、RAGFlow；插件开关与密钥 | esp32-server | 各插件在无 key、超时、断网时都给出明确提示 |
| NV12 视觉 | `capabilities.vision` 下发、`/mcp/vision/explain`、VLLM 提供商 | esp32-server | 设备拍照提问得到回答；图片不落盘；无 token 时拒绝 |
| NV13 Agent CLI | Claude Code / Codex 适配、`agent_run` 审批、工作目录内可读写、进度播报、记录、运行中可停止 | 海门 | 未批准不启动；只在严格模式下提供工具；工作目录以外的读取不保证被挡住（Windows 沙箱限制） |
| NV14 音乐 | 本机音乐目录、模糊匹配、`symphonia` 解码与重采样、播放泵复用、打断 | esp32-server | 播放连续不断音；说话或 abort 立即停 |
| NV15 Home Assistant | 地址与令牌、实体同步与勾选、查询 / 控制 / 播放、`home_control` 审批、不可放行类别 | esp32-server | 未勾选实体不可见；控制默认需批准；门锁永远需批准 |
| NV16 声纹 | 说话人特征模型下载、登记与删除、识别注入、可选“仅登记者可发起加工”、外部服务兼容 | esp32-server + sherpa-onnx | 不存录音；识别失败不影响对话 |
| NV17 固件下发 | 固件导入与 SHA-256、逐台确认推送、版本比较、一次性令牌下载接口、记录 | esp32-server | 没有用户确认过的推送就不下发；未允许设备无法下载；路径穿越、重放与到期令牌被拒 |
| NV18 收尾 | 三端构建、Android 回归（不含语音服务）、文档、安装包检查 | — | 沿用 P4-11 的检查 |
| NV19 长期记忆与智能上下文 | 条目化记忆、目录分层知识库、检索、验证经验、只读跨会话恢复与私有 UI | 借鉴 OpenViking；复用 NV10/NV11 | 旧记忆兼容、默认关闭新增采集、权限隔离、回归与真实验收分开 |

**规模粗估**（Rust，不含测试）：

| 阶段 | 估计行数 | 说明 |
| --- | --- | --- |
| NV1–NV4 | 6,000–8,000 | 其中约 2,000 行可借用海门代码 |
| NV5–NV8 | 3,000–4,000 | 另加前端 |
| NV9 | 约 1,500 | |
| NV10–NV13 | 合计约 2,500 | |
| NV14–NV17 | 合计约 3,000 | 另加前端 |

**P4 已完成的内容如何处理**：语音中心页面、链路切换与回滚、模型“先测试后保存”、日志脱敏全部复用；“外部服务端”的进程管理与 `.config.yaml` 写入保留为第三种链路。

### 8.1 NV8a 语音体验优化（2026-10-08 用户决定做）

背景：与市面语音产品对比后（端到端语音模型、Home Assistant 语音助手、LiveKit / Pipecat 等语音框架），内置语音在安全与本地化上占优，短板在响应速度与对话自然度；且“停”目前要经过大模型才执行。以下五项决定实施，安排在 NV9 之前。

1. **停止类指令本地快速通道**
   - **现状问题**：识别 → 大模型 → 模型按系统提示调用停止工具。喊“停”有 1–3 秒延迟；断网或大模型出错时语音停不下来；模型理解错也停不下来。
   - **做法**：识别出文字后、交给大模型之前，先在本地匹配停止词。命中则直接以 `CallerContext::Voice` 调用 `laser_safe_action_tool`（`emergency_stop`，0x18；现有分级为 R2，语音调用立即执行、不生成审批），照常写审计与调用统计，不经过大模型；识别在本地完成时不依赖云端识别网络。随后播放固定回复（如“已发送停止请求，请确认设备状态”，合成结果缓存，合成不可用时只发文字），这一轮不再交给大模型，原话与结果记入对话历史。
   - **匹配规则**：去掉标点与空白后，整句等于停止词，或停止词加语气词（吧、啊、了、一下、快）。停止词初版：停、停止、停下、停机、急停、快停、别打了、关掉激光。“暂停”不进快速通道（2026-10-13 修正）：急停会复位控制器、任务无法继续，与“暂停”的本意不符，交给大模型处理；MOSS 暂无可恢复的暂停 / 继续动作。含否定或停止词只是句子的一部分（“不要停”“停止之后怎么办”）不匹配，交给大模型。词表在代码中固定并有测试，不开放远程修改。
   - **历史软件记录（2026-10-10安全修补）**：当时仅处理最终文字，n18曾记录其与用户要求存在差项；历史保留，不作为当前行为。
   - **当前软件证据（2026-10-12）**：Session在select监听`AsrStream::next_interim`，中间结果规范化后整句匹配固定停止词立即进入一次`FastStop`，不等待最终文字或声纹。`interim_whole_stop_executes_without_waiting_for_final_and_once`已在本轮workspace通过。用户接受临时“停止”随后续成“停止之后怎么办”可能已经误停；完整否定句/含词长句若未先命中临时整句则不走快捷通道。专用关键词检测未实现，实机0x18/误触发仍留HW-09。
   - **限制**：此快捷通道只调用停止类动作，不放宽运动或出光授权；不能关闭（与 `laser_safe_action_tool` 不可关闭一致）。语音识别仍可能误判，软件请求不证明物理停机，实体急停仍是第一手段。播放期间直接插话的前置条件见 NV8b §8.3；未满足条件时先按设备原有方式打断再说停止。
2. **常用只读指令本地匹配**
   - **做法**：识别文字先与一组固定句式匹配，命中则直接调用对应的 R0 工具并用本地模板生成回答，不经过大模型；没命中再交给大模型。初版句式：设备 / 激光机状态（`laser_safe_action_tool` `status`、`check_laser_connection_tool`）、加工进度与剩余时间（`laser_workflow_tool` `status`）、当前材料或参数（`material_params_tool` `get`）。
   - **规则**：只用 R0 工具；匹配不确定时一律交给大模型；语音中心可关闭（默认开启）。回答模板写在代码中并有测试，工具失败时说明原因而不是编造。
3. **识别热词**
   - **本地**：流式 Transducer（X-ASR）改用 `modified_beam_search` 并加载热词文件（sherpa-onnx `hotwords_file` / `hotwords_score`）；SenseVoice 不支持热词，界面注明。实施时实测改用束搜索后的速度与识别率变化，速度明显变慢时保留 greedy 作为选项。
   - **云端**：豆包识别按其接口的热词 / 上下文字段传入（实施时核实字段名）；OpenAI 兼容识别用 `prompt` 字段作为提示。
   - **词表**：内置激光行业词（材料名取自材料库，如椴木板、亚克力、牛皮纸；单位与参数词，如功率、速度、毫米每秒、遍数）＋ 用户在语音中心自定义的词；词表只存 `settings.json`，不含隐私内容。
4. **本地合成边合成边播**
   - **现状问题**：本地合成按整句合成后一次送出，长句或慢模型（Kokoro v1.1 int8 约 1 倍实时）要等整句合成完才开始播放。
   - **做法**：为 `LocalTts` 实现 `Tts::synthesize_stream`：辅助程序按逗号等停顿把长句再切成小段，逐段合成、逐段回送（新增流式合成请求，一个请求多个响应帧；协议号再升一版，辅助程序须与 MOSS 一起更新）。播放沿用 NV7 的分段播放与启动缓冲。
   - **验收**：长句的首段音频就绪时间明显早于整句合成时间；分段之间无断音（事件“补静音 0 帧”）。
5. **各环节耗时记录**
   - **记录点**（每轮一条，相对“说完”或“开始说话”计时）：判停确认开始说话的延迟、判停确认说完、识别出最终文字、本地快速通道命中（若有）、大模型首个字、首句切出、首段合成音频就绪、首帧发给设备、每次工具调用耗时、整轮结束。
   - **展示**：语音中心显示最近 20 轮的各环节耗时与所用模型 / 提供方，可复制；只在内存中，重启清零；不记录识别文字与回答内容（§6）。
   - **实机测试**：真实设备、真人语音下的各组合耗时随实施清单 §2a 统一测试（HW-09）。

### 8.2 可选项（2026-10-08 列入，**尚未决定是否实施**）

以下三项只记录设计方向，不排期；用户决定实施后再补充细节并加入 §8 计划表。

| 编号 | 内容 | 好处 | 代价与风险 |
| --- | --- | --- | --- |
| VO-1 语义判停 | 判断“这句话说完没有”不只看静音长度：先用规则（句尾是“嗯 / 那个 / 然后 / 和”、数字或单位没说完时，多等 300–500 ms），再视效果加小模型兜底（LiveKit、Pipecat 有轮次判断模型，中文支持需核实） | 用户说话中间停顿不会被截断，尤其是报参数时 | 判断错时整句多等几百毫秒；小模型增加下载与 CPU 占用 |
| VO-2 端到端语音模型 | 新增一种可选提供方：豆包实时语音大模型、OpenAI Realtime 等“语音进、语音出”的模型，用于闲聊 | 回话更快（一般 1 秒内）、语气更自然、可插话 | 音频全部上传云端，费用较高；拿不到稳定的识别文字（审计与停止快速通道仍需本地并行识别或关键词检测）；工具调用仍必须走 `CallerContext::Voice` 与审批，控制激光仍建议用现有链路 |
| VO-3 严格模式关闭时的本地口头确认 | 现在严格模式关闭时，加工只靠大模型复述参数、用户说“确认”后模型带 `confirmed=true`。改为：MOSS 记下待执行的工具调用及其参数摘要，下一句话必须在本地匹配到确认词（“确认开始”“开始加工”），且参数与刚才复述的一致，才放行；否则拒绝并重新复述。可与声纹（NV16）结合，只允许已登记的人确认出光 | 识别错误或模型误解不能单独触发出光 | 多一句确认，操作稍慢；严格模式开启时不需要（桌面审批已覆盖） |

### 8.3 NV8b 自然打断（设备端回声消除，2026-10-08 用户决定做）

**现有打断机制**（NV2 / NV3 已实现，代码 `session.rs`）：

- **触发**：只有设备能发起。MOSS 说话时设备默认不上传声音（上传了也被丢弃：`on_binary` 只在拾音状态处理音频），所以用户直接开口 MOSS 听不到；要打断须喊唤醒词或按键，设备发 `abort`（唤醒词打断时原因为 `wake_word_detected`）或新的 `listen start`。
- **处理**（`interrupt`）：
  1. 丢弃本轮任务（`ActiveTurn` 被丢弃时 `handle.abort()`）：大模型流式请求随连接断开而停止，之后的句子不再合成。已生成的 token 仍按云端规则计费；本地辅助程序里正在合成的那一句会合成完再丢弃（辅助程序请求不可取消）。
  2. 停播：`pump.clear()` 丢弃未播音频与句子标记、重置 Opus 编码器，向设备发 `tts stop`。设备自身缓冲里已收到的几帧（约 0.2 秒）仍会播完。
  3. 历史：本轮识别出的原话与“已送去播放”的句子记入对话历史，下一轮模型知道自己说到哪里被打断。现状按“送去播放”计，可能包含已合成但未播出的句子，NV8b 修正为只记已开始播放的句子。
  4. 状态：`abort` 后回到空闲，由设备随后的 `listen start` 进入拾音；`listen start` 触发的打断直接进入拾音（自动模式等判停确认开始说话后才打开流式识别）。
- **唤醒回应保护**：设备检测到唤醒词后先发 `listen detect`，MOSS 播“我在”；固件紧接着发来的 `listen start` 不打断这句，记下 `listen_after`，“我在”播完（约 0.5–1 秒）再按该模式开始拾音——否则这句回应总会被固件自己的 `listen start` 掐掉。
- **播放节奏**（`PlaybackPump`）：开始播放前先攒够启动缓冲（批量合成 600 ms，豆包流式 300 ms；整句已合成完则不等）；开播时连发 3 帧（180 ms）填满设备缓冲，之后按绝对时钟每 60 ms 一帧（不受发送耗时累积影响）；上游供不上时补静音帧而不是停发——设备缓冲一旦播空会出现断音、卡顿，补静音让设备一直有数据可播；事件记录“补静音 N 帧”，真机排查断音时看它。

**NV8b 设计**：

- **硬件前提（用户确认）**：音频输出 ES8311（DAC + 功放驱动喇叭）；输入 ES7210（4 通道 ADC），MIC1、MIC2 采人声，MIC3 硬件连接 ES8311 的喇叭输出作为回采参考通道。参考信号与喇叭实际发出的声音严格同步，正是设备端回声消除需要的条件，因此选设备端方案；电脑端回声消除拿不到同步的参考（网络抖动、设备缓冲、功放与喇叭延迟），效果无保证，仍不做（V7）。
- **固件**：开启小智固件的设备端回声消除（编译选项名以所用固件版本为准，实施时核实），设备进入实时模式：`listen start` 的 `mode` 为 `realtime`，播放期间持续上传已消除回声的音频。重新编译与刷写固件属于需用户明确同意的操作，由用户执行或在同意后进行，MOSS 不修改设备。
- **MOSS 侧做法**：
  1. 记住设备的拾音模式；只有 `realtime` 启用插话检测，`auto` / `manual` 行为不变。
  2. 播放期间（含“思考中”）继续解码上行音频，送入一路独立的插话判停，参数比平时严格：阈值提高；连续语音达到插话时长（默认 400 ms，可在 300–600 ms 调整）才算插话；开播后前 300 ms 不检测（喇叭起振、回声消除收敛期间残余回声最大）；唤醒回应、再见语播放时不检测。
  3. 判定插话后：走现有 `interrupt`（中止任务、清空播放泵、`tts stop`、记历史），立即进入拾音；插话检测期间缓存的音频（最多 1 秒，同 `PREROLL_MS`）作为这一句的开头，流式识别立即打开并补送，开头的字不丢。
  4. 历史修正：打断时只把已开始播放的句子（播放泵已发出其 `sentence_start`）记入历史。
  5. 误打断防护与调参：每次插话打断记事件（检测到的语音时长、距开播时间）；语音中心开关“说话时可直接插话”（默认关，固件开启实时模式后再打开）与插话时长设置。可选第二步“软打断”（先停发新帧，识别出文字再确认打断，没字则续播），实机误打断多时再做。
- **与 NV8a 的关系**：插话后说“停”经停止类快速通道直接急停，不必先喊唤醒词。
- **验收**：自动测试用假设备（实时模式）：播放中持续语音超过插话时长即收到 `tts stop`，新一句识别包含插话开头；短于插话时长的声音不打断；`auto` 模式播放期间仍忽略上行音频；唤醒回应不被插话检测打断；打断时未播出的句子不进历史。实机见实施清单 §2a HW-10。

## 9. 已确认的决定（2026-10-07）

| 编号 | 事项 | 决定 |
| --- | --- | --- |
| V1 | 密钥存放 | MOSS SQLite `app_secrets`（与 Jev Key 同表）；不进 `settings.json` 与日志；语音相关接口不返回原文 |
| V2 | 范围 | NV0–NV8 主线 + 第三方 MCP（NV9）+ 记忆（NV10）+ 天气、新闻、农历、搜索、RAGFlow（NV11）+ 视觉（NV12）+ Claude Code / Codex（NV13）+ 音乐（NV14）+ Home Assistant（NV15）+ 声纹（NV16）+ 固件下发（NV17）。飞书 / 钉钉 / GitHub 渠道、角色切换、拨打设备不做；原“服务端打断与 AEC 暂不做”已由 V7 取代：做设备 AEC 配合服务端自然打断，电脑端 AEC 仍不做 |
| V3 | 设备端口 | 沿用 8000（WebSocket）/ 8003（OTA、视觉），可在语音中心修改 |
| V4 | 外部服务端链路 | 保留作兼容；内置链路为主推 |
| V5 | 本地模型的许可证处理 | **已确认（2026-10-07）**：本地模型推理（VAD、ASR、TTS、声纹）放独立辅助程序 `moss-voice-local.exe`，以 GPL-3.0 单独发布源码；MOSS 本体保持 MIT（§11.2） |
| V6 | 语音体验优化（2026-10-08） | NV8a 五项决定实施，排在 NV9 之前（§8.1）；语义判停、端到端语音模型、严格模式关闭时的本地口头确认列为可选项，暂不决定（§8.2） |
| V7 | 自然打断（2026-10-08） | 做设备端回声消除方案 NV8b（§8.3），排在 NV8a 之后、NV9 之前；V2 中“服务端打断”改为做，电脑端回声消除仍不做 |

## 10. 风险

| 风险 | 影响 | 应对 |
| --- | --- | --- |
| 设备固件版本差异（协议 v1/v2/v3、MCP 支持） | 连不上或无声 | 同时支持三种帧格式（海门已实现）；NV1 用用户手上的设备实测 |
| sherpa-onnx / opus 在 Windows 的编译与体积 | 构建失败或安装包变大 | NV0 先试编；本地模型按需下载 |
| `univoice` 来源未核实 | 许可或质量问题 | NV0 核实，不合格则自写豆包协议 |
| 新增 tokio 运行时影响现有线程模型 | 急停或发送受影响 | 语音运行时独立，急停路径不经过它；A-30..A-34 回归 |
| 密钥存 SQLite | 数据库文件或备份泄露即泄露密钥 | 运行目录在用户目录下；文档提示妥善保管备份；后续可加 Windows DPAPI 加密（不改变存放位置） |
| 第三方 MCP、Agent CLI 扩大受控面 | 越权读写或执行 | 默认每次需批准；只读模式；只能在桌面添加；审计全记录 |
| 插件依赖第三方网站 | 接口变动导致失效 | 超时与明确提示；插件独立开关 |
| 固件推送错误 | 设备变砖 | 默认不推送；逐型号确认；显示 SHA-256；型号必须与设备上报一致 |
| 声纹误识别 | 称呼错误或拒绝合法用户 | 阈值可调；声纹只做辅助，不替代桌面审批 |
| 工作量大 | 周期长 | 按 NV 阶段交付，每个阶段都可独立使用 |

## 11. NV0 结论

### 11.1 依赖库核实（2026-10-07，读源码）

| 库 | 版本 / 提交 | 许可证 | 结论 |
| --- | --- | --- | --- |
| sherpa-onnx（Rust 绑定） | 1.13.8（仓库 `99ddefa`） | Apache-2.0 | 可用。API 覆盖本方案所需：`VoiceActivityDetector`（Silero）、`OfflineRecognizer`（SenseVoice 等）、`OfflineTts`（Kokoro、VITS、Matcha）、`SpeakerEmbeddingExtractor` / `SpeakerEmbeddingManager`（声纹）。默认静态链接，**构建时从 GitHub 下载**预编译静态库 `sherpa-onnx-v1.13.8-win-x64-static-MT-Release-lib.tar.bz2`；离线构建可设 `SHERPA_ONNX_LIB_DIR`。Android / iOS 只支持动态库（本方案不在移动端使用）。**静态库内含 espeak-ng（GPL-3.0），见 §11.2** |
| opus2 | 0.4.0 | MIT 或 Apache-2.0 | 可用，海门已在用。`bundled` 从源码编译 libopus（BSD），**需要 CMake 与 C 编译器**；另有纯 Rust 后端 `backend-mousiki`（同一套 API，源码核实于 `e117ac7`）。用户电脑未装 CMake，首次试编在 libopus 处失败；探针默认改用 mousiki，装了 CMake 时再对比 C 版，NV0 实测后定 |
| univoice（Rust 版） | `main-rs` 分支 `021113f`（crates.io 发布 0.1.6） | MIT | **不直接依赖**：它把 clap、tracing-subscriber、dotenvy 作为常规依赖，且用 reqwest 0.12、tungstenite 0.24 与 native-tls，会和 MOSS 的 reqwest 0.13 / tungstenite 0.26 / rustls 重复。NV7 改为移植其中的豆包协议（`src/asr/protocol/sauc.rs`、`src/tts/protocol/volcengine.rs` 等，约 2000 行），保留版权声明（已列入 `THIRD_PARTY_NOTICES.md`）。**NV7 已移植**（帧协议约 400 行，连接与流程重写），见 §13.8 |
| rmcp（官方 MCP Rust SDK） | 3.5.1（仓库 `08e0211`），MSRV 1.88 | Apache-2.0 | 可用。客户端传输有子进程（stdio）与 streamable HTTP（reqwest 0.13 + rustls，与 MOSS 一致）；**没有旧版 SSE 客户端**，只提供 SSE 的第三方服务暂不支持，在语音中心给出提示 |

- 三个上游项目与 univoice 的 MIT 声明已写入 `THIRD_PARTY_NOTICES.md`。
- 本机测量程序 `tools/nv0-probe` 用于试编 opus2 与 sherpa-onnx 并测各模型耗时；NV0 完成后已删除，结果见 §11.3。

### 11.2 许可证问题：espeak-ng（GPL-3.0）与“本地模型辅助进程”方案

- sherpa-onnx 的 Windows 预编译静态库包含 `espeak-ng` 与 `piper_phonemize`（`sherpa-onnx-sys/build.rs` 的 `SHERPA_ONNX_STATIC_LIBS`），供 Kokoro、VITS 等 TTS 做音素转换。espeak-ng 采用 GPL-3.0 许可。
- MOSS 是 MIT 许可。把含 GPL-3.0 代码的库**静态链接进 moss-desktop.exe** 再对外分发，会使整个程序受 GPL-3.0 约束。sherpa-onnx 的 C API 在同一目标文件中同时包含 ASR 与 TTS 接口，只用 ASR 也很可能把 espeak-ng 链接进来。
- **采用方案（V5，用户已确认）**：
  - 把所有本地模型推理（VAD、ASR、TTS、声纹）放到独立的辅助程序 `moss-voice-local.exe`。
  - 辅助程序单独以 GPL-3.0 许可发布源码，与 MOSS 一起打包。
  - MOSS 通过标准输入输出（帧化的 JSON + 二进制音频）与它通信，两者是独立程序。
  - MOSS 本体继续保持 MIT。
  - 附带好处：辅助程序崩溃不会影响 MOSS 主进程与急停（G4）；不用本地模型时不启动它，节省内存。
- **备选**：
  - ① MOSS 整体改为 GPL-3.0。
  - ② 从源码编译 sherpa-onnx 并关闭 TTS（`SHERPA_ONNX_ENABLE_TTS=OFF`），本地只做 VAD、ASR、声纹，本地 TTS 放弃或另找无 GPL 依赖的方案。
- 以上是对许可证条款的一般理解，**不构成法律意见**；对外分发前如有疑问，应咨询专业人士。
- 云端链路不受影响：没有本地模型时，VAD 也需要辅助程序（Silero 由它运行）；若用户只用云端且不想运行辅助程序，退回能量阈值 VAD（海门做法）。

### 11.3 本机实测（2026-10-07，用户电脑：i7-13650HX，20 逻辑核，16 GB 内存，Windows，Rust 1.98.1）

| 项 | 结果 |
| --- | --- |
| 编译 | 未装 CMake；默认纯 Rust Opus 后端（mousiki）+ sherpa-onnx 静态库，`cargo build --release` 一次通过，82 秒（含首次下载预编译库） |
| 可执行文件体积 | 探针 19.8 MB（Opus + sherpa-onnx 全部静态链接）；对照：现有 `moss-desktop.exe` 39.1 MB。辅助程序 `moss-voice-local.exe` 预计约 20 MB |
| Opus 16 kHz / 24 kHz、60 ms 帧 | 各 2 秒音频编解码约 10 ms；平均 125 / 185 字节每帧；解码音量比 0.92 / 0.96（正常）。与真实设备的互通在 NV1 验证 |
| Silero VAD | 加载 67 ms；5.59 s 音频处理 15.5 ms（实时率 0.003），正确分出 1 段语音 |
| SenseVoice（int8） | 加载 0.94 s；中文 5.59 s 音频识别 0.22 s、英文 7.15 s 用 0.28 s、粤语 5.15 s 用 0.20 s（实时率约 0.04），三段文字均正确 |
| Kokoro（多语言 v1.0） | 加载 1.33 s，24 kHz，54 个音色；长句（4.94 s 音频）首段 0.40 s、总计 2.72 s；短句（2.32 s 音频）1.21 s（实时率约 0.55） |
| 说话人特征（3D-Speaker CAM++） | 加载 0.33 s，192 维，每段提取约 65 ms；不同人两段相似度 0.23 |
| 模型下载体积 | Silero 0.6 MB；SenseVoice 229 MB；Kokoro 383 MB；说话人模型 27 MB |

**结论**：

1. Opus 默认用纯 Rust 后端 mousiki：不需要 CMake，降低了 MOSS 的构建门槛；C 版 libopus 作为备选，NV1 若发现与设备互通有问题再切换。
2. 本地识别很快（一句话约 0.2 秒），可作为默认的离线识别方案。
3. 本地合成的首段延迟为 0.4–1.2 秒，可以接受。按“首句遇逗号就送合成”的切句规则，首句会更短。较慢的电脑上会更慢，语音中心应提示用户可改用云端合成。
4. 辅助程序约 20 MB，可以随安装包分发。模型不打进安装包，按需下载，四个模型合计约 640 MB。
5. NV0 完成，进入 NV1。

## 12. 来源

- xiaozhi-esp32-server：<https://github.com/xinnan-tech/xiaozhi-esp32-server>（`7a260ee`）
- haimen：<https://github.com/shenjingnan/haimen>（`513d9e6`）
- xiaozhi-client：<https://github.com/shenjingnan/xiaozhi-client>（`fe3c382`）
- sherpa-onnx Rust 绑定：<https://docs.rs/sherpa-onnx>（官方，1.13.8，默认静态链接，提供 Windows x64 预编译）；源码 <https://github.com/k2-fsa/sherpa-onnx>（`99ddefa`）
- univoice Rust 版：<https://github.com/shenjingnan/univoice/tree/main-rs>（`021113f`）
- rmcp：<https://github.com/modelcontextprotocol/rust-sdk>（`08e0211`，3.5.1）
- opus2：<https://github.com/cijiugechu/opus2>（0.4.0）
- xiaozhi-esp32 固件（协议依据，只读未借用代码）：<https://github.com/78/xiaozhi-esp32>（`0d576d3`）`main/protocols/websocket_protocol.cc`、`main/protocols/protocol.h`、`main/ota.cc`、`main/application.cc`

## 13. 实施记录

### 13.1 NV1 协议与 OTA（2026-10-07，代码完成，待本机测试与真实设备验收）

- **协议依据**：以设备固件为准（`0d576d3`）。
  - 固件的 OTA 请求带 `Device-Id`、`Client-Id` 请求头与系统信息 JSON；响应中读取 `websocket{url, token, version}`、`server_time`、`firmware`。
  - 响应**不含 `mqtt` 段**，固件因此改用 WebSocket；也不含 `activation`，MOSS 用“桌面允许”代替激活码。
  - WebSocket 请求头为 `Authorization: Bearer <token>`、`Protocol-Version`、`Device-Id`、`Client-Id`。
  - 设备 hello 的上行音频为 16 kHz、60 ms 帧；下行采样率取服务端 hello 的 `audio_params`。
  - 固件支持 `alert{status, message, emotion}`，未授权提示用它显示在设备屏幕上。
- **帧格式**：按连接声明的协议版本解析（1 裸 Opus、2 带 16 字节头、3 带 4 字节头），不像海门那样自动猜测；OTA 下发版本 1。
- **设备 token**：与 xiaozhi-esp32-server 格式相同（`base64url(HMAC-SHA256)`.`签发时间`，30 天有效），密钥存 `app_secrets` 的 `voice.device_token_secret`。单元测试用 Python 参考值核对过签名。
- **允许流程**：
  1. 新设备首次 OTA 或连接时登记为待允许（`voice_devices`）。
  2. OTA 只给已允许的设备签发 token。
  3. 未允许、已撤销、token 无效的设备握手后收到 `alert` 提示并断开。
  4. 同一 MAC 换了 Client-Id 时，已允许的设备退回待允许。
  5. 允许后设备须重启，才会重新 OTA 拿到 token。
- **回声模式**：
  - 手动模式在 `listen stop` 时回放；自动模式录 3 秒后回放；最长录 30 秒。
  - 回放先发 `stt`（帧数与时长）、`tts start` 与 `sentence_start`，先连发 3 帧预缓冲，之后按绝对时钟每 60 ms 发 1 帧，最后发 `tts stop`。
  - 播放中收到 `abort` 或新的 `listen start` 立即停止。
  - 下行声明 16 kHz（原样回放上行音频）。
  - 120 秒无消息断开。
- **会话**：每台设备一个；状态与最近 100 条事件供语音中心显示，事件不含 token 与音频。
- **测试（Rust）**：
  - `moss-xiaozhi-proto` 13 项：帧往返与布局、消息解析、token、OTA 与版本比较。
  - `moss-voice-server` 7 项：真实绑定本机端口，用 reqwest 走 OTA，用 tungstenite 扮演设备，覆盖待允许 / token 错误 / 撤销 / 缺 Device-Id、手动模式回放内容与节奏、自动模式 + 协议 v2 + 打断、停止后断开、端口占用报错；另有 Host 头解析单测。
  - `laser-runtime` 1 项：设备登记与状态。
- **本机测试（2026-10-07）**：`cargo test --workspace` 全部通过（本条所列测试均在其中）。
- **待验收**：用真实小智设备把 OTA 指向电脑，确认能连上、听到回声（技术书 §8 NV1 验收）。

### 13.2 NV2 音频（2026-10-07，代码完成，待真实设备验收）

- **模块**：`crates/moss-voice-server/src/audio/`，全部是同步纯计算，由会话调用，可用确定的输入测试。
  - `codec`：opus2 + 纯 Rust 后端 mousiki（§11.3 结论 1）。解码器每会话一个；编码器整个会话只用一个，句与句之间不重建。编码用 `Audio` 模式、复杂度 10、VBR，与 xiaozhi-esp32-server 一致；单元测试中 `Voip` 模式在 24 kHz 下回放音量只有原声约 0.7，因此不用。
  - `resample`：有状态线性插值，分块输入与整体输入结果一致；用于回声的 16 → 24 kHz，NV3 起用于合成音频。
  - `vad`：`Vad` 接口 + `EnergyVad`。20 ms 窗口；双阈值（开始取 `max(500, 背景×4)`、保持取 `max(250, 背景×2)`，RMS，满幅 32767）；连续 60 ms 高能量才算开始（滤掉按键声）；连续静音 200 ms 判停；背景噪声只在非语音时慢速跟踪。参数语义取 esp32-server `silero.py`。
  - `pump`：`PlaybackPump`。流式启动前先攒 `playback_start_ms`（默认 600 ms，取海门；上游已结束则不等）；播放中上游供不上时补一帧静音而不是停（不足一帧的尾巴留到下一拍）；结束时尾帧补零；`clear` 立即丢弃未播内容并重置编码器；统计有内容帧数与补静音帧数。
- **会话（`session.rs`）**：
  - 状态：空闲 → 拾音（`listening`）→ 播放（`speaking`）。上行采样率取设备 hello 的 `audio_params`（不支持时按 16 kHz 并记事件）；下行声明 24 kHz。
  - 手动模式以 `listen stop` 为准；自动模式由 VAD 判停，保留语音开始前 1 秒（避免吞字；原为 300 ms，2026-10-08 调大，见 §13.10）；一段话最长 30 秒。
  - 播放：开头连发 3 帧，之后按绝对时钟每 60 ms 一帧。设备 `abort` 或新的 `listen start` 立即停播（发 `tts stop`，不再发音频帧）。
  - 空闲断开改为“120 秒没有说话也没有文本消息”：自动模式下设备持续上传的静音帧不算活动。
  - 无法解码的音频帧忽略，每个会话只记录第一次。
  - 已授权设备先建好会话（编解码器）并在状态中登记，再回 hello。NV2 初版先回 hello 再登记，编解码器初始化拉长了中间的间隔，测试中出现“设备已收到 hello 但语音中心还看不到会话”的竞争，因此调整顺序。
  - 每段播放结束记录事件“播放完成：N 帧，补静音 M 帧”，用于真实设备上排查断音。
- **回声模式**：改为“解码 → 判停 → 重采样到 24 kHz → 重新编码”播回，覆盖整条音频链路；`stt` 文字为“回声测试：约 X 秒”。
- **与计划的差异**：Silero VAD 需要本地模型辅助程序（V5）与模型下载，这两项本来就在 NV6，因此 Silero 随 NV6 接入 `Vad` 接口；NV2 的“静音 200 ms 判停”由能量 VAD 满足。能量 VAD 在嘈杂环境中可能判停不准，语音中心提示可改用按住说话。
- **测试（Rust，`moss-voice-server` 共 21 项，新增 14 项）**：
  - 编解码 2 项（16 / 24 kHz 往返样本数与音量、短帧补零、非法采样率）；重采样 3 项（时长与音量、分块与整体一致、同采样率直通）；VAD 4 项（200 ms 判停时刻、短停顿不切分、按键声与背景噪声、重置）；节奏泵 3 项（补零与完整播放、启动缓冲与补静音、打断丢弃）。
  - 端到端（tungstenite 扮演设备，用真实 Opus 编码的音调与静音）：手动模式回放 20 帧、音量相近、节奏不快于 60 ms / 帧、坏帧被忽略；自动模式 + 帧协议 v2 静音判停、只有静音时不播放；帧协议 v3 播放中 `abort` 立即停止且之后 400 ms 内无音频帧；播放中新的 `listen start` 打断并重新拾音。
  - 原 NV1 的两项回声测试断言“回放字节与录入完全相同”，NV2 回声改为重新编码后该断言不再成立，改写为对解码后的音频断言（帧数、音量、节奏），属行为变更，不是放宽。
- **待验收（真实设备）**：自动模式说完约 0.2 秒后播回；回声无断音、跳字（看事件中的补静音帧数应为 0）；播放中唤醒或按键立即停播；嘈杂环境下判停是否可靠。

### 13.3 NV3 云端对话（2026-10-07，代码完成；桌面端启用随 NV5；待真实模型与设备验收）

- **新 crate `crates/moss-voice-providers`**（只供桌面端；纯函数 + 异步客户端，不依赖服务端）：
  - 接口 `Asr` / `Llm` / `Tts`（返回装箱 future，便于替换为假实现与 NV6 / NV7 的本地、流式厂商）；错误类型 `ProviderError` 只含中文原因，**不含地址与密钥**（不用 reqwest 原始描述；服务端错误信息含 `http` 或 `sk-` 时不回显）。
  - `openai`：识别 `POST …/audio/transcriptions`（multipart：WAV、`model`、`response_format=json`、可选 `language`；兼容直接返回文字的服务）；大模型 `POST {基础地址}/chat/completions`（`stream: true`，有工具时带 `tools` 与 `tool_choice: auto`；SSE 解码支持半个汉字、`\r\n`、多行 data、无空行结尾；工具调用按 `index` 拼接名称与参数，缺 id 时补 `call_N`；服务忽略流式而返回完整 JSON 时也能解析；流中的 `error` 对象报错）；合成 `POST …/audio/speech`（`response_format: wav`，返回非 WAV 时明确报错）。地址规则与 P4 模型向导一致（大模型填基础地址，识别 / 合成可填基础地址自动补全）。超时：连接 10 秒；识别、合成各 30 秒；大模型等首包与两段数据之间各 30 秒、总计 120 秒；不跟随重定向。
  - `text`：切句（首句遇任何停顿标点含逗号即切，之后只在句末切；英文句点后跟空白才算句末；超过 180 字或 6 秒在最后一个标点处强制送出）；清洗 Markdown（加粗、代码、标题、列表、引用）与表情；去掉开头的孤立标点；首个表情映射为固件的表情名。
  - `history`：OpenAI 格式消息；按长度预算（字符数，默认 4000，不含系统提示）从最早整轮丢弃，保证以用户消息开头、工具调用与结果不被拆开，最后一轮总是保留。
  - `wav`：16 位单声道编码；解码支持 16 位整数与 32 位浮点、单 / 双声道（混成单声道）、额外块与奇数长度填充、流式 WAV 的 `0xFFFFFFFF` 长度。
- **对话编排（`moss-voice-server/src/conversation.rs`）**：
  - 每轮对话一个 tokio 任务，经事件通道把识别结果、表情、每句的文字与音频、错误、结束信息送回会话；会话打断时中止任务（`ActiveTurn` 丢弃即中止）。
  - 一轮：识别 → 文字为空则继续拾音 → 退出词（去掉标点后与列表完全相同，默认“退出 / 再见 / 拜拜 / 退下 / 关闭对话”）说再见并结束会话 → 否则 `系统提示 + 历史 + 原话` 交给大模型，边生成边切句、逐句合成；
  - 工具调用：同一轮多个调用并行，各 30 秒超时；结果回填后再问模型，最多 5 轮（第 6 次仍要调用工具时停止并说出错话术，未执行的调用补“未执行”结果）；工具可返回“直接朗读”（esp32-server 的 `RESPONSE`），朗读后不再问模型；工具任务 panic 时补“执行失败”结果，保证每个调用都有结果。
  - 出错：识别或大模型失败时，若本轮还没说过话，说“抱歉，我这边出了点问题，请稍后再试”；某句合成失败则跳过该句。错误记入事件（不含密钥与地址）。
  - NV3 不提供任何工具（`NoTools`）；`ToolExecutor` 与 `ToolContext{device_id, session_id}` 留给 NV4 接入 MOSS 工具与审批。默认系统提示要求简短口语、不用 Markdown 与表情。
- **会话（`session.rs`）**：
  - 新增 `thinking` 状态（一段话已交给对话任务、还没有音频）；第一句音频就绪时发 `tts start` 并开始播放；每句音频带文字标记推入节奏泵，播到这句的第一帧时先发 `sentence_start`，屏幕文字与声音同步；全部播完发 `tts stop`。
  - 识别文字只发给设备（`stt`）并进对话历史，事件里只记“识别完成：N 字”与“首句延迟：X ms”（§6：识别文本不进日志正文）。
  - 打断（`abort` 或新的 `listen start`）：中止任务，丢弃未播音频；已识别的原话与已播出的句子记入历史。
  - 唤醒：设备发 `listen detect` 后播放问候（默认“我在”，合成结果缓存，只合成一次）；固件紧接着发的 `listen start` 不会打断问候，问候播完后按该模式开始拾音。
  - 空闲：对话模式下 120 秒没有说话也没有消息时先说再见、播完再断开；退出词同样说完再断开（断开前等 600 ms 让设备播完已发出的帧）。
  - 历史每台设备一份，随会话结束丢弃（持久记忆在 NV10）。
- **服务配置**：`ServerMode::Chat` 与 `ServerConfig.conversation`（`Conversation`：三个提供方、工具、`ConversationSettings`）；对话模式缺少配置时启动报错。桌面端仍以回声模式启动——模型配置界面与密钥保存属 NV5，届时启用对话模式。
- **测试**：
  - `moss-voice-providers` 20 项：切句 6、历史 3、WAV 3、SSE 与工具调用拼接 2、地址 1，以及本机假 HTTP 服务 5 项（流式增量与工具调用、请求体与 Authorization、非流式回退、无密钥不发头、401 / 连不上 / 流中错误的提示不含密钥与地址、multipart 上传内容、合成请求与非 WAV 报错）。
  - `moss-voice-server` 新增 10 项（共 32 项）：节奏泵句子标记 1；对话端到端 9（假识别 / 大模型 / 合成 / 工具 + tungstenite 设备）——逐句播放与文字同步、历史带入下一轮、工具结果回填与“直接朗读”、5 轮上限、大模型出错与单句合成失败、退出词说再见后断开、打断中止任务且部分回答进历史、识别为空继续拾音、唤醒问候缓存且不被 `listen start` 打断、空闲说再见后断开；缺少配置时启动报错 1。
- **待验收**：真实云端模型（P0-01..P0-03，用户暂无模型）与真实设备（§2a）——完整对话、首句延迟、打断与唤醒在设备上的表现。

### 13.4 NV4 工具与审批（2026-10-07，代码完成；桌面端接线随 NV5；待真实设备验收）

以下为 n9 实施时记录；其中“始终严格”已被 n11 的 §13.6 取代。当前规则统一见 §5.2，不按此历史段落重新实施。

- **身份**：`laser_contracts::CallerContext::Voice { device_id, session_id }`，标签 `voice`，审计调用方 `voice:<Device-Id>`（截 64 字）；审批来源 `ApprovalSource::Voice`（`voice`），`approvals.session_id` 记发起设备，审批卡显示“小智语音（MOSS 内置） · 设备 …<末 4 位>”。
- **始终严格**（相对 §5.2 原设计的调整）：原设计是“启用内置语音时把全局严格模式打开”。改为在执行层按调用方强制——`require_r3_authority` 与发送入口 `launch_send_job_for` 对 `Voice` 不走“严格模式关闭即放行”的分支；`laser_host::VoiceBackend::strict_mode()` 恒为真，接入点同款的分级门禁对语音始终生效（R4 / 未分级 → `tool_not_allowed` 并审计）。这样即使全局开关关闭或切换失败，语音也无法直接让设备运动或出光；官方接入点、局域网与桌面的行为不变。
- **工具调用面**：`laser_host::voice_tools_list()`（等于严格模式下接入点的清单：不含命令执行、打开应用、快捷键、打开网站，不含开发者工具）与 `laser_host::voice_tool_call(api, device, session, name, args)`（`dispatch_tool_call_with` + `VoiceBackend`）。`InProcessBackend` 的请求构造抽成 `in_process_request`，两个后端共用。`HostBackend` 新增 `audit_client()`（默认 `mcp`），接入点门禁的审计记录用它。
- **对话侧**（`moss-voice-server/src/tools.rs`，不依赖 laser-*）：宿主实现 `ToolHost { list, call }`；`HostTools` 把清单转成模型工具定义、在阻塞线程执行同步调用；结果的 `result.status` 为 `approval_required` 时**直接朗读**后端给的话术（默认“已发到电脑，请在电脑上的 MOSS 里确认后开始。”），不再问模型；其他结果以 `{success, result, detail}`（detail 截断到 1500 字）交给模型；参数不是合法 JSON 时不调用宿主。
- **设备自带 MCP**（§5.3）：设备 hello 声明 `features.mcp` 且为对话模式时，会话开始即发 `initialize`（`clientInfo.name = MOSS`；`capabilities.vision` 留给 NV12），收到结果后发 `notifications/initialized` 与 `tools/list`，按 `nextCursor` 翻页（最多 20 页）。工具名加 `device__` 前缀并把非 `[A-Za-z0-9_-]` 字符换成 `_`（模型接口不接受 `.`），清理后重名的不登记；调用时由对话任务经通道交给会话，会话发 `tools/call`（原名）并按 id 把结果送回；结果取 `content[].text`，`isError` 与 JSON-RPC `error` 转为失败说明。设备工具按 R1 直接执行（§5.2 表），受对话层 30 秒超时约束。
- **测试**：
  - `laser-api` 2 项：严格模式关闭时语音发送与设定原点仍只生成审批、来源 voice、记录设备、审计 `voice:<设备>`、批准后以审批身份执行（A-20 / A-25 / A-26 内置链路）；急停 / 软复位 / 查询 / 取消立即执行且不生成审批（A-27）。
  - `laser-contracts` 1 项扩充：`Voice` 的标签、审计调用方与审批来源。
  - `laser-host` 1 项：语音工具清单不含永不开放的工具；调用打开应用被拒并审计 `voice:<设备>`；全局严格模式关闭时只读工具照常执行。
  - `moss-voice-server` 新增 4 项（共 36 项）：`HostTools` 2（审批结果原话朗读、其他结果交模型并截断、非法参数不调用、空参数按 `{}`）；设备 MCP 2（握手 + 两页清单 + 清理名调用、设备收到原名、结果回填；错误与 `isError` 的转换）。
  - 前端 `approvalState` 来源标签断言 1 条。
- **待做（NV5）**：桌面端实现 `ToolHost`（调用 `laser_host::voice_tool_call`）并以对话模式启动内置服务。
- **待验收**：真实设备上“雕刻你好”只生成审批卡、语音提示到电脑确认；设备自带工具（音量等）可用。

### 13.5 NV5 语音中心与密钥（2026-10-07，代码完成；待真实模型与设备验收）

其中审批措辞是当时记录；加工跟随全局严格模式的后续变更见 §13.6，不能把旧提示词作为关闭严格模式时仍需桌面批准的现状。

- **链路三选一**：`moss_voice::VoiceMode::Builtin`（`builtin`）。切换规则（`VoiceCenter::set_mode` / `mode_changes`）：
  - 官方 → 内置：接入点与严格模式不变，不重连小智桥；启动内置服务，失败则整体回滚（设置、严格模式、桥、内置服务）。
  - 自建 → 内置：与切回官方相同（恢复官方接入点、严格模式恢复为 `.env` 设置、重连桥），再启动内置服务。
  - 内置 → 官方 / 自建：停止内置服务；切到自建时照常备份当前官方接入点。
  - 接入点没有变化时不再重连小智桥（P4 之前每次切换都重连）。
  - 确认框列出：链路变化、内置服务随 MOSS 启动及当前会以对话还是回声运行、内置语音调用加工工具始终需要桌面审批、设备 OTA 需自行修改。
  - 内置链路下顶层 `mcp_endpoint` 仍是官方接入点，在语音中心修改后立即重连（与官方模式相同）。
- **模型设置与密钥**：`settings.json` 的 `voice.builtin_models.{llm,asr,tts}` 只存地址（规范化后）、模型、音色、语速；密钥存 `app_secrets` 的 `voice.llm.main` / `voice.asr.main` / `voice.tts.main`（V1）。`moss_voice::builtin::plan_save` 产出“保留 / 替换 / 删除”计划：输入框留空沿用已保存密钥，`clear` 中的分区删除设置与密钥；`resolve_for_test` 测试时留空用已保存密钥；`status_view` 只返回 `configured`、`key_set`、地址与模型。`KeyChange` 的调试输出不含密钥。
- **桌面端**：
  - 三项都配置时以对话模式启动（`OpenAiAsr` / `OpenAiLlm` / `OpenAiTts` + `HostTools(DesktopToolHost)`），否则回声模式；保存模型设置时若服务在运行则重启以生效。
  - 语音链路为内置服务时随 MOSS 后台启动（`voice_builtin::autostart`），失败原因显示在卡片上。
  - 新命令 `voice_builtin_models_test`、`voice_builtin_models_list`、`voice_builtin_models_save`（已登记 build.rs 与 capability）。
  - `DeviceRegistry::sessions_changed` 上报设备会话数到 `global_voice_telemetry`；服务启动时为 0、停止时为未知。`/api/system/info` 的 `voice.mode` 增加 `builtin`，此时报告内置服务的会话数。
- **前端**：语音链路第三项“MOSS 内置服务”；内置服务卡片显示模式（对话 / 回声测试）、自动启动说明与失败原因，新增“模型配置”（与 P4 相同的先测试后保存、密钥不回显、删除需确认）；顶栏小智胶囊在内置链路下按设备会话数显示“未连接 / 等待设备 / 已连接”。
- **密钥不外泄的检查**（技术书 §8 NV5 验收）：`settings.json`（`moss-voice` 设置测试断言不含 `api_key`）、状态返回（`status_view` 测试断言不含密钥）、日志与事件（提供方错误不含密钥，NV3 已测）均不含密钥原文。
- **测试**：`moss-voice` 新增 4 项（共 39 项：内置链路切换与停止、启动失败回滚、保存计划、测试用密钥与状态视图）及设置往返扩充；`laser-api` `/api/system/info` 内置链路断言；桌面端命令登记与链路解析断言；前端 `npm test` 新增 2 项（顶栏内置链路、内置服务视图与表单）。预览桩（演示数据 + 模拟 invoke）在内置浏览器中走查：回声 / 对话 / 自动启动失败三种状态、先测试后保存的调用参数（密钥留空不提交新值）、420 px 宽无横向滚动。
- **待验收**：真实模型下的完整对话（P0 模型到位后）与真实设备（§2a HW-08 起）。

### 13.6 变更：内置语音的加工确认跟随严格模式（2026-10-07，用户决定）

- **决定**：用户认为“内置语音发起的运动与出光始终需要桌面批准”过严，选择“跟随严格模式开关”（与官方小智链路一致）。§13.4 的“始终严格”自此作废。
- **实现**：`require_r3_authority` 与 `launch_send_job_for` 不再对 `CallerContext::Voice` 特殊处理；`VoiceBackend::strict_mode()` 返回全局开关。`HostBackend` 新增 `always_deny_r4()`（接入点为否，行为不变；内置语音为是），严格模式关闭时内置语音仍拒绝 R4 与未分级工具并审计；采用调参建议（`regenerate_text_laser_task_tool` + `feedback_id`）在严格模式关闭时照常执行。默认系统提示增加：运动或出光前先复述材料、厚度、内容与功率速度次数，用户下一句明确说“确认 / 开始”后才带 `confirmed=true`，停止类立即执行。语音中心切换确认框按当前开关说明两种情况。
- **风险**：严格模式关闭时，内置语音的加工只靠模型遵守“先口头确认”的提示与 `confirmed=true` 参数门禁，识别错误或模型误解可能导致直接出光（与官方链路相同）。快速开始已提示开启 `MOSS_FUSION_STRICT` 可恢复桌面批准。
- **测试**：`laser-api` `voice_r3_needs_desktop_approval_in_strict_mode`（严格模式开启）、`voice_follows_strict_mode_switch_when_off`（关闭时直接发送、不生成审批、作业完成）、`voice_stop_actions_run_immediately`；`laser-host` `voice_backend_denies_desktop_control_tools_and_audits`（严格模式关闭时仍拒绝并审计）；`moss-voice` 切换确认框文案断言。

### 13.7 NV6 本地模型（2026-10-07，代码完成；真实模型端到端测试通过；待真实设备与安装包）

- **结构**（V5）：
  - `crates/moss-voice-local-proto`（MIT）：帧 = 头长度 u32 LE + 负载长度 u32 LE + JSON 头 + 16 位 PCM；请求带 id，响应按 id 匹配；头 ≤ 64 KB、负载 ≤ 60 秒 48 kHz 音频；截断帧报错而不是当作结束。
  - `apps/moss-voice-local`（GPL-3.0-or-later，独立 Cargo 工作区，不参与根 workspace 测试）：判停、识别、合成各一个线程，模型对象在本线程创建；标准输入关闭即退出。sherpa-onnx 1.13.8 配置与 NV0 探针相同（SenseVoice `language=auto`、`use_itn`；Kokoro 中英词典；Silero 阈值 0.5、静音 200 ms、最短语音 250 ms、最长 30 s，只支持 16 kHz）。Silero 的事件：检测到语音即报“开始”，完成的语音段弹出时报“结束”（之前没报过开始的先补报）。构建：`cargo build --release --manifest-path apps/moss-voice-local/Cargo.toml --target-dir target`，产物 `target/release/moss-voice-local.exe`（约 20 MB）。
  - 客户端 `moss_voice_providers::local`：`LocalHelper` 启动辅助程序（Windows 不弹控制台窗口）、握手核对协议版本、读线程按 id 分发；辅助程序退出时所有等待中的请求立即失败，之后的请求直接报“本地模型程序已退出”；超时：加载 120 秒、识别 30 秒、合成 60 秒、判停每帧 500 ms。`LocalAsr` / `LocalTts` 实现 `Asr` / `Tts`（在阻塞线程中调用）。
- **判停**：`moss-voice-server` 新增 `VadFactory` 与 `LocalVad`（NV2 的 `Vad` 接口）。每个会话一路判停流，每轮拾音换新流；任何一次出错（超时、辅助程序退出）本会话改用能量判停并记事件，不中断对话。只有 16 kHz 上行使用 Silero。
- **模型管理**（`moss_voice::local_models`）：清单三项——Silero（0.6 MB）、SenseVoice int8（155 MB 压缩包）、Kokoro 多语言 v1.0（334 MB 压缩包）；SHA-256 取自 GitHub 发布页资产摘要（Silero 另与 NV0 下载文件核对一致）。下载到 `models/.download/*.part`，边写边算摘要，大小或摘要不符即删除；tar.bz2 解到临时目录，只接受普通文件与目录、拒绝绝对路径与 `..`、跳过链接，核对必需文件后整体改名就位；可取消；镜像地址可改，校验不变；纯 Rust 的 bzip2 后端。2026-10-08 起改为可选清单，见 §13.10。
- **设置**：`settings.json` 的 `voice.builtin_local`：`asr` / `tts` / `vad` 开关、`tts_speaker`（默认 50 = `zm_yunxi` 中文男声；45–48 中文女声、49–52 中文男声）、`tts_speed`（0.5–2.0）、`mirror`。分区可用 = 本地开关打开且模型已安装，或云端已配置（大模型只有云端）；三项都可用时以对话模式启动，本地优先于云端。
- **桌面端**：内置服务启动时按开关启动辅助程序并加载模型（失败则启动失败并显示原因；音色编号超出模型音色数也拒绝），停止时结束辅助程序；运行中删除模型被拒绝（Windows 下文件被占用）。新命令 `voice_local_save`、`voice_local_model_download`、`voice_local_model_cancel`、`voice_local_model_delete`；状态的 `local` 字段含设置、模型下载进度、辅助程序是否存在 / 运行、各分区是否可用。
- **界面**：内置服务卡片新增“本地模型”：模型表（大小、状态、进度条、下载 / 取消 / 删除，删除需确认）、三个开关（模型未下载时提示开关暂不生效）、音色与语速、下载地址；找不到辅助程序时提示。
- **测试**：
  - 协议 3 项；客户端 6 项（按 id 匹配与并发、版本不符、辅助程序退出立即失败、判停超时、找不到程序、异步包装）；`LocalVad` 2 项（事件映射与换流、辅助程序退出改用能量判停并提示一次）；模型管理 5 项（压缩包校验安装、摘要不符 / 截断 / 404、路径穿越被拒、缺必需文件、清单一致性）及设置可用性 1 项；前端 1 项。
  - 真实环境（`#[ignore]`，本机运行通过）：真实下载 Silero（GitHub 跳转 + SHA-256）；辅助程序加载三个模型 2.8 秒，Kokoro 合成 2.3 秒音频用 1.25 秒，SenseVoice 105 ms 识别回原句，Silero 正确报开始 / 结束；**端到端**：Kokoro 合成“帮我看看激光机的状态。”当作设备上行，自动模式 Silero 判停（2.2 秒）、SenseVoice 识别原句、假大模型回答、Kokoro 合成回复播放，首句延迟 1078 ms，补静音 0 帧。
- **未完成 / 待验收**：
  - 此为 NV6 当时记录；2026-10-10 已新增 helper EXE sidecar 配置/脚本接线，但没有新 NSIS 解包/安装证据，不能证明成品含 helper 且能启动。源码与 `COPYING` 资源仍待 NV18 收集；当前交付条件见实施细则和安装规范 §2b。
  - 本地模型目录导入（使用已下载好的目录）未做。
  - 真实设备上的本地链路（§2a）。

### 13.8 NV7 流式云厂商：豆包（2026-10-08，代码完成；待真实豆包账号与设备验收）

- **接口**（`moss-voice-providers`）：
  - `StreamingAsr::start() -> AsrStream`：立即返回，后台建连接；`feed(&[i16])`（16 kHz）不阻塞，连接建立前的音频先缓存；`finish()` 等最终文字；丢弃即中止后台任务、关闭连接。
  - `Tts::synthesize_stream(text, chunks)`：音频分段送出；默认实现整句合成后一次送出，OpenAI 兼容与本地合成不变。
  - 线性重采样移到 `moss_voice_providers::resample`（会话层 `audio::Resampler` 重新导出），豆包识别把非 16 kHz 上行转为 16 kHz。
- **豆包协议**（`doubao::{sauc, bidirection}`，移植自 univoice `021113f` / crates.io 0.1.6，MIT，文件头注明来源；`THIRD_PARTY_NOTICES.md` 已列）：
  - 识别（`wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async`，默认资源 ID `volc.bigasr.sauc.duration`）：参数帧（gzip JSON，序号 1；16 kHz / 16 位 / 单声道 PCM，`enable_itn`、`enable_punc`）→ 收到确认后每 1600 个样本（100 ms）一包、gzip、序号递增 → 说完发最后一包（序号取负）→ 收到“最后一包”响应即结束。每条结果是到目前为止的完整文字，空结果不覆盖。超时：连接 10 秒、等确认 10 秒、发最后一包后 10 秒；整段识别总计 30 秒。
  - 合成（`wss://openspeech.bytedance.com/api/v3/tts/bidirection`，默认资源 ID `seed-tts-2.0`）：`StartConnection` → `ConnectionStarted`；每句 `StartSession`（`speaker`、`audio_params{format: pcm, sample_rate: 24000, speech_rate}`，`namespace: BidirectionalTTS`）→ `SessionStarted` → `TaskRequest`（`text`）→ `FinishSession` → 收 `AudioOnlyServer` 分段直到 `SessionFinished`（`status_code` 不是 20000000 视为失败）。语速：倍速 0.5–2.0 线性换算为 −50–100。两条消息间隔超过 15 秒或一句超过 60 秒报超时。一句完成后连接留给下一句；复用的连接已失效且还没送出音频时换新连接重试一次。
  - 鉴权：新版控制台 `X-Api-Key`；密钥形如“纯数字 APP ID:Access Token”时改用旧版 `X-Api-App-Key` + `X-Api-Access-Key`；另带 `X-Api-Resource-Id`、随机 `X-Api-Connect-Id`。未发送 univoice 加的 `permessage-deflate` 扩展头（tungstenite 不支持该扩展，服务端协商成功会导致帧无法解析）。
  - 相对 univoice 的修正：事件号保留原值（服务端句子事件 350–352 不在其枚举中，原实现当作“无事件”）；`SessionFailed` / `SessionCanceled` 视为失败（原实现会一直等）；所有解析越界报错不 panic。
  - TLS：tokio-tungstenite 0.29（与 axum 同版本，不增加重复的 tungstenite）+ rustls，`ClientConfig` 明确使用 ring 与 webpki 根证书，不依赖进程级默认加密实现。
- **会话**：手动模式 `listen start`、自动模式 VAD 报“开始说话”时才开始识别（环境静音不建连接），先补送保留的 300 ms 开头，之后每帧随收随送；说完把识别交给对话任务（保留的开头 2026-10-08 起为 1 秒，见 §13.10）。流式失败时对话任务用整段音频再识别一次（豆包整段识别也走同一协议）并记事件“流式识别失败，改用整段识别”。打断、重新拾音时识别被丢弃。
- **播放**：每句第一段音频带句子文字（`sentence_start` 时机不变），后续分段沿用同一重采样器追加。豆包合成时播放启动缓冲 300 ms（批量合成仍为 600 ms）。事件“首句延迟”从说完计到第一段音频就绪。
- **设置与界面**：`voice.builtin_models.<分区>.kind`（`openai` 缺省不写 / `doubao`）；`resolve_builtin` 校验（豆包只用于识别与合成，`wss://`，地址与资源 ID 留空用默认，合成音色必填、语速 0.5–2.0）；外部服务端链路拒绝豆包。界面“接口类型”下拉，切换时填入默认地址与资源 ID。“测试连接”对豆包做一次真实识别（1 秒低音量正弦音）或合成（一句短话）。
- **测试**：协议 4 项、提供方 9 项（假 WebSocket 服务逐字节应答）、会话 4 项、校验与设置 2 项、默认值一致 1 项、前端 1 项。真实地址（`#[ignore]`）：假密钥得到 401 `Invalid X-Api-Key`，证明 TLS 与鉴权头可用。
- **未完成 / 待验收**：真实豆包账号下的识别、合成与首句延迟对比（§8 的验收“首句延迟明显低于批量”）；真实设备。豆包的流式模式会把开头的环境声也计入识别时长（按时长计费的资源 ID 会多计少量费用）。

### 13.9 NV8 接入点增强（2026-10-08，代码完成；待真实小智接入点验收）

- **多接入点**：`settings.json` 顶层 `mcp_extra_endpoints`（最多 4 个）为附加的官方接入点，与主接入点 `mcp_endpoint` 同时连接，提供同一套工具（编号 `primary`、`extra-N`，N 为列表中的位置）。无效与重复地址跳过。自建模式下主接入点指向自建接入点，附加接入点暂停连接，切回官方或内置时恢复。每个接入点一条线程（`mcp-bridge-<编号>`），重启桥时全部先停再起。
- **断线判定与重连**：每 30 秒发 WebSocket Ping；90 秒收不到任何数据（含 Pong）判定断线——网络中途断开时 TCP 可能长时间不报错，原来只靠读超时会一直“在线”。重连沿用 1 秒起加倍、最长 60 秒；连接稳定 60 秒以上后断开，等待从 1 秒重新开始（原来只有正常关闭才归零）。断开原因去掉 token 后记入该接入点。
- **工具开关**：顶层 `mcp_disabled_tools`。关闭的工具不出现在接入点的 `tools/list` 与内置语音的工具清单里，调用返回 `tool_disabled`、不触达后端——即列表与调用两处都校验（xiaozhi-client 的开关只改界面）。`laser_safe_action_tool` 不能关闭（语音“停”依赖它，§6.1）。改动后重连所有接入点以更新清单（接入点不支持清单变更通知）；内置语音每轮重读清单，下一轮对话即生效。开关拒绝只计入统计与事件，不写审计表（新增审计动作会让旧版程序读不了审计记录）。
- **调用统计**：每个工具的调用、失败、拒绝次数与最近一次的时间、调用方（审计标签 `mcp` / `voice:<设备>`），接入点与内置语音共用；只在内存中，重启清零。未分级的工具名统一计为“未知工具”。
- **接入点身份**：解码 token 中 JWT 的载荷，只显示标识类字段（键名以 id 结尾，或 purpose / name / sub 等，最多 8 个）与签发、过期时间，过期标红；不校验签名，只用于显示。官方 token 的实际字段名未在真实账号上核实。
- **界面**：语音中心“接入点与工具”卡片（状态、身份、添加 / 删除附加接入点、工具开关与统计）。契约见 api-contracts §5（`voice_save_settings` 的 `add_extra_mcp_endpoint` / `remove_extra_mcp_endpoint` / `disabled_tools`，`voice_status.mcp`）。
- **测试**：遥测 2 项、桥与开关 4 项（含本机假接入点上的心跳与空闲重连、连接失败原因不含 token）、宿主 2 项、`laser-api` 2 处断言、语音中心 2 项、对话工具清单 1 项、桌面端 1 项、前端 1 项。
- **未完成 / 待验收**：真实官方接入点上的多接入点同时在线、Ping 响应与断网恢复（§2a）；token 载荷的真实字段。

### 13.10 本地模型可选（2026-10-08，代码完成；真实模型本机测试通过；待真实设备与安装包）

用户要求在 NV6 的三个模型之外，加入 TEN-VAD、SenseVoice 2025、X-ASR 流式识别、Kokoro v1.1 int8、Matcha 中英，由用户自己选用，并在每个模型后面简要说明优缺点。

- **清单**（`moss_voice::local_models::CATALOG`，每项有类别 `kind`、引擎 `engine`、一句优缺点 `note`、多音色合成的音色范围 `voices`）：

  | id | 类别 | 引擎 | 下载 | 本机实测（i7-13650HX） |
  | --- | --- | --- | --- | --- |
  | `silero_vad` | 判停 | Silero | 0.6 MB | 经 Opus 编解码后开始 / 结束时刻不变 |
  | `ten_vad` | 判停 | TEN-VAD | 0.3 MB | 见下文“TEN-VAD 参数” |
  | `sense_voice` | 识别 | SenseVoice（整段） | 155 MB | 一句约 0.1 秒，带标点 |
  | `sense_voice_2025` | 识别 | SenseVoice（整段） | 158 MB | 一句约 0.15 秒，同一句输出不带标点 |
  | `x_asr_streaming` | 识别 | 流式 Zipformer Transducer（480 ms 块、中英、带标点、int8） | 128 MB | 加载约 3.5 秒；边说边识别，说完到出字 50–100 ms；整段识别约 0.22 秒 |
  | `kokoro` | 合成 | Kokoro v1.0 | 334 MB | 合成 1 秒语音约 0.5 秒；音色 45–48 中文女声、49–52 中文男声 |
  | `kokoro_v11` | 合成 | Kokoro v1.1 int8 | 140 MB | 合成 1 秒语音约 1 秒（int8 在本机 CPU 上比 v1.0 慢）；103 个音色，按基频实测 0–2 英文、3–57 中文女声（180–264 Hz）、58–102 中文男声（126–162 Hz），默认 3 |
  | `matcha_zh_en` | 合成 | Matcha（`model-steps-3.onnx` + `vocos-16khz-univ.onnx`） | 79 + 54 MB | 合成 1 秒语音约 0.04 秒；单音色女声，16 kHz |

  SHA-256 均取自 GitHub 发布页的资产摘要，并与实际下载文件逐一核对一致。Matcha 的声码器在另一个发布标签（`vocoder-models`），作为**附加文件**（`ExtraFile`）与压缩包一起下载：总进度按两者大小合计，各自校验，解压后放进模型目录再整体就位；任何一个校验失败都不安装。许可证：TEN-VAD 为 Apache-2.0 附加条款（不得用于与声网竞争的产品）；X-ASR、Kokoro v1.1、Matcha 中英为 Apache-2.0。
- **协议 2（历史）**（`moss-voice-local-proto`，2026-10-08）：加载请求带引擎（`load_vad.engine`：`silero` / `ten`；`load_asr.engine`：`sense_voice` / `streaming_transducer`；`load_tts.engine`：`kokoro` / `matcha`；缺省为 NV6 的引擎）；新增流式识别 `asr_open` / `asr_feed`（负载为 16 kHz PCM，辅助程序边收边解码）/ `asr_finish`（返回文字并关闭该路）/ `asr_close`。当时从 1 升为 2；当前 NV8a 协议已是 3，见 §13.11，不能按此历史段落判断当前兼容性。
- **升级要求：辅助程序与 MOSS 必须一起更新**。上条是2026-10-08协议1→2历史记录，NV8a当时升为3（§13.11）；本轮NV16当前为**协议4**，更旧helper握手拒绝。需按`apps/moss-voice-local/README.md`重建，并与同一发布的桌面程序配套。NV18专用内部NSIS已实际核对helper/源码/许可资源，最新产物见实施TODO§9；许可对应审核和成品安装仍未通过，不能从封包成功推定可分发。
- **辅助程序**：识别线程持有整段（`OfflineRecognizer`）或流式（`OnlineRecognizer`，greedy search）识别器；流式模型也支持整段识别（作为流式失败时的退路），整段模型拒绝 `asr_open`。说完补 1.0 秒静音再结束输入：实测 0.66 秒（sherpa 示例值）会丢最后一个字，1.0 秒不丢，只多约 30 ms。合成：Kokoro 按目录里存在的文件选 `model.onnx` / `model.int8.onnx`；目录里有 `phone-zh.fst`、`date-zh.fst`、`number-zh.fst` 时作为读法规则（数字、日期、电话）；单音色模型忽略音色编号。
- **客户端**：`load_vad` / `load_asr` / `load_tts` 增加引擎参数；`LocalStreamingAsr` 实现 NV7 的 `StreamingAsr`——后台任务打开一路、把到达的音频（上一段还在解码时合并）依次送入、说完取最终文字；任务被中止（打断、重新拾音）时向辅助程序发 `asr_close`。超时：打开 / 送入 5 秒，结束 30 秒。
- **设置**：`voice.builtin_local` 新增 `vad_model` / `asr_model` / `tts_model`（缺省 `silero_vad` / `sense_voice` / `kokoro`，NV6 保存的设置按原模型读取，行为不变）；`BuiltinLocal::selected(kind)` 只在开关打开且 id 存在、类别相符时返回模型。分区是否可用改为看**选中的**模型是否已安装。`voice_local_save` 拒绝不存在或类别不符的模型 id。
- **桌面端**：按选中的模型与引擎启动辅助程序；选了流式识别时对话同时挂上 `LocalStreamingAsr`（会话自动模式在判停确认开始说话后打开），整段识别作为退路；合成为多音色时检查音色编号在 0 ~ 音色数−1，单音色用 0。
- **TEN-VAD 参数**：用 Matcha / Kokoro 合成的语句、经设备同样的 Opus 16 kHz 编解码后测试——默认参数（阈值 0.5、最短语音 250 ms）下长句要晚 1.0–1.8 秒才报“开始说话”，“停止”这类短词完全检测不到（Silero 都正常）；最短语音降到 100 ms 后长句在 0.3 秒内报出、“停止”能检测到，代价是突发的白噪声开头会被当成约 0.6 秒的一小段语音（100 Hz 嗡声不会）。漏掉“停止”比偶尔多出一段空识别更糟，所以 TEN-VAD 用 `VadParams::for_engine(Ten)`（最短语音 100 ms），Silero 不变；界面介绍写明噪声误触发，推荐 Silero。
- **自动拾音保留开头 300 ms → 1 秒**（`session::PREROLL_MS`，对所有判停生效）：本地判停确认“开始说话”实测要在开口后 0.4–0.9 秒，原来的 300 ms 保留量会丢开头一两个字（端到端测试中识别成“光机的状态”）。代价是识别多带不到 1 秒的开头静音（云端整段识别多传这部分音频）。
- **界面**：本地模型按“判停 / 语音识别 / 语音合成”三张表分组，每张表上方是该类的本地开关；每行有选用单选、模型名（流式识别带“边说边识别”标记）与一句优缺点、大小（含附加文件）、状态、下载 / 取消 / 删除；下载失败显示“下载失败”并在下方小字给原因。音色编号的说明与范围随选中的合成模型变化，改选多音色模型时填入其默认音色，单音色模型显示“只有一个音色”。
- **测试**：协议 1 项（引擎字段与缺省、`for_engine`）；客户端 3 项（假辅助程序：流式送入与最终文字、丢弃后关闭该路且下一路换编号、打开失败在结束时报错）；模型管理 1 项（附加文件下载、校验、放入模型目录，附加文件摘要不符整体不安装）并扩充清单一致性检查（id 不重复、引擎与类别一致、附加文件计入必需文件、默认音色在范围内）；设置 2 处（改选模型后的可用性、类别不符不算选中、旧设置按默认模型读取）；会话测试按 1 秒保留量更新；前端 1 项（分组、改选合成模型换默认音色、按模型检查音色范围）。
  - 真实模型（`#[ignore]`，本机通过）：`real_optional_models`——五个新模型逐一加载与运行（数据见上表），SenseVoice 2025 与 X-ASR 识别回原句；`real_local_fast_models_end_to_end`——TEN-VAD + X-ASR 流式 + Matcha，设备上行经 Opus，连续 4 次识别完整、首句延迟 253–315 ms；`real_local_models_end_to_end`（NV6 组合）首句延迟 1113 ms。
- **未完成 / 待验收**：真实设备与真人语音下的判停与识别（以上均为合成语音）；Kokoro v1.1 int8 在其他电脑上的速度；安装包包含辅助程序（NV18）。

### 13.11 NV8a 语音体验优化（2026-10-10）

本节主体保留2026-10-10的软件记录。后续已按本会话要求实现中间结果整句立即停机，当前行为和测试以§8.1的2026-10-12记录及TODO NV8a-1为准；下述final-only描述仅代表当时版本。

安全修补补充：本地判停的创建、重置与每帧 IPC 全部移到独立工作线程；输入和结果分别有 64 项上限，线程总数 16，单项排队至响应最多等待 1.5 秒；迟到结果按拾音轮次隔离。异常时关闭该会话并提示，不把同步模型计算放回设备消息/播放定时循环。撤销设备用共享失效标记与执行入口复核，关闭该设备全部旧会话并作废未消费的审批；重新允许不恢复旧会话。具体实际关闭延迟、识别等待与真人误触发率仍待实机。

- 停止类精确短句在大模型之前匹配；2026-10-10 安全修补后，仅在最终识别的完整短句命中时走固定停止通道；中间文字不产生设备动作，避免“停止之后怎么办”被前缀触发。固定调用补齐现有工具契约要求的 `confirmed=true`，不改变其他动作的权限规则。活动作业实时写入失败时返回失败，不把“已请求取消”当成已停止；软件请求不替代硬件急停。停止无需大模型，但云端识别本身仍依赖网络，不能宣称任意模型组合都离线可用。
- 常用只读快捷通道默认开启，可关闭；查询状态、连接、发送进度和材料信息仍使用现有工具入口。发送行进度不冒充物理加工完成，缺少实测速度时不编造剩余时间。
- `voice.builtin_local` 增加 `read_only_shortcuts=true`、`asr_beam_search=true` 和 `custom_hotwords=[]`。热词去重后最多 64 条，每条 32 字符，总计 4096 字节；拒绝控制字符。X-ASR 可用 modified beam search，保留切回 greedy 的选项；SenseVoice 不宣称支持此热词通路。云端 OpenAI 兼容识别与豆包按各自请求字段提供上下文热词。
- 辅助程序协议 **2 → 3**，桌面与辅助程序必须一起升级。增加解码选项、中间识别文字和分段合成返回；按分段生成音频后送出，不能等同于模型内部逐采样推理。取消后宿主丢弃过期响应，已在运行的单次同步推理未必被抢占。
- 增加每轮各环节耗时，最近 20 轮只在内存保留；语音中心显示与复制诊断只取允许字段，不复制完整服务状态、对话、音频或模型地址。
- 自动验证包含停止快捷通道、中间前缀不执行/最终短句只执行一次、实时写入失败、只读结果、热词请求与前端边界、诊断字段白名单。真人语音与真实设备性能仍按 HW-09 统一验收，本次未采集新实机性能数字。

### 13.12 NV8b 自然打断（2026-10-10）

- `barge_in=false`，`barge_in_ms=400`；前后端都限制 300–600 毫秒整数。只有设备声明 realtime 且用户开启该功能才处理播报期间上行。依赖设备端回声消除，不把服务端能量判停称为 AEC；本次没有修改或刷写固件。
- 使用独立、较严格的能量判停检测连续语音；真正发出首帧后保护 300 毫秒，问候和退出告别不参加自然打断。判停保留最多一秒开头，触发后立即转交下一轮识别；设备紧随停止消息发来的重复 realtime 开始事件不清空这段开头。
- 打断清空当前生成任务与播放队列，发送停止播放。历史只保留已经发出 sentence_start 的句子；即使大模型和合成提前完成，也不会把尚未开始播放的尾句当成用户已经听过。正常完整播放才提交完整一轮历史。
- 7 项专项测试通过：连续语音/短噪声、缓冲有界、时长校验、思考中打断、问候与告别保护、开头立即回送及历史裁剪、关闭/非 realtime/开播保护。HW-10 的真实回声、噪声误触发率和真人打断延迟仍未验证。

### 13.13 NV9 桌面集成（2026-10-10，实服务未验证）

四段已接线：桌面配置及版本化凭据引用 → 独立连接维护 → external_tool 持久审批和审计 → 内置语音聚合与统计页面。详细操作、契约和故障边界见 [第三方 MCP](moss-external-mcp.md)。

- 语音中心新增“第三方 MCP”，支持 stdio / Streamable HTTP。服务默认停用，工具默认 Ask。配置命令只接受主窗口和实际回环后端来源，不开放 HTTP 管理接口；Android 不编译外部客户端。
- settings 只存配置和版本化引用，凭据在 app_secrets；空白保留当前字段，删除字段移除。普通语音设置的旧表单不能覆盖第三方配置；先提交新凭据，再原子替换配置，故障时不把旧连接与新密钥错配。崩溃可能残留不可达引用，本期不广泛清库。
- 每 30 秒空闲心跳与清单刷新，1–60 秒退避重连，调用 30 秒超时，队列有界且不补发旧操作。最多 8 服务、每服务发现最多 256 工具／8 页、全局最多 64 工具给模型；描述 200 字符。停用、冲突、超预算在调用处也拒绝。
- external_tool 审批绑定真实会话、服务／连接、凭据、schema、权限及完整参数，批准和执行前再次检查；单次消费、配置变化与撤销失效。默认 Ask 不随激光严格模式关闭；模型确认字段、审批编号与第三方只读声明都不是授权。执行前审计失败不放行，执行后写入失败明确提示可能已执行，不自动重试。异常退出恢复结果未知。
- 审批卡使用第三方数据访问确认文字和有界脱敏参数预览。完整参数只在本地隐藏 request；审计不存完整参数或响应。工具接入内置语音，待审批提示去电脑批准，批准后的结果在桌面查看，本期不主动推回语音。
- 页面显示服务状态、权限、内存调用计数与最近时间、最近持久审计。客户端/管理器 14 项、宿主 NV9 9 项、桌面窗口边界、前端表单和 1280/960 像素实际 React 点击检查覆盖上述边界；Rust 全量 478 项通过。最终前端、E2E 和 Windows/Android 构建结果以最新 CHANGELOG / 实施 TODO 为准。没有连接真实第三方账号或控制硬件。

## 14. NV10～NV18 的实施入口（n20）

每项的六栏目实施说明在 [实施细则 i3](moss-native-voice-implementation.md)，逐步勾选在实施 TODO。新增说明已区分现有模块与拟新增文件，不以规划的类型、参数或测试名冒充已实现能力。

| 阶段 | 开发说明 | 开工时先核对 | 真实验收边界 |
| --- | --- | --- | --- |
| NV10 记忆 | [迁移/后台总结/CRUD/竞态](moss-native-voice-implementation.md#nv10) | 实际下一版 schema、当前历史提交点 | 真实模型总结质量 |
| NV11 信息插件 | [五类接口/模块/错误/fixture](moss-native-voice-implementation.md#nv11) | 用户服务版本、key、NewsNow/RSS 来源、依赖锁定 | 账号/来源/限额 |
| NV12 视觉 | [multipart/多模态/鉴权/配额](moss-native-voice-implementation.md#nv12) | 固件上传/响应 fixture、模型支持 | 相机和真实设备端到端 |
| NV13 Agent | [实际参数/审批/进程树/权限](moss-native-voice-implementation.md#nv13) | 已安装版本、限制参数、负向权限测试 | CLI 权限及账号调用 |
| NV14 音乐 | [索引/解码/帧队列/取消](moss-native-voice-implementation.md#nv14) | codec feature、会话播放事件 | 音质、断音、插话 |
| NV15 家居 | [REST 映射/实体策略/执行复核](moss-native-voice-implementation.md#nv15) | 服务支持、白名单、审批种类 | 逐实体真实控制授权 |
| NV16 声纹 | [模型/协议/标定/记忆隔离](moss-native-voice-implementation.md#nv16) | 模型 hash/许可/维度与外部契约 | 真人 FAR/FRR、重放边界 |
| NV17 固件 | [文件/鉴权/状态/失败恢复](moss-native-voice-implementation.md#nv17) | OTA/下载可信身份、板型兼容 | 实机刷写单独授权，无自动降级 |
| NV18 交付 | [资源收集/版本清单/安装验收](moss-native-voice-implementation.md#nv18) | 已有 sidecar 接线、缺源码/许可资源 | 新安装包、升级/备份回退、Android |

默认值和限制均为后续实施契约：记忆/新增插件/视觉/音乐/声纹默认关闭，Agent 每次审批，HA 控制默认 Ask、高风险实体不可 Allow，固件默认不推送。NV16 的仅登记者限制默认关闭，开启后加工仍需批准、停止始终可用。配置保存、软件实现、测试、构建和真实验收分别记录，不相互代替。

## 15. NV19 长期记忆与智能上下文升级（2026-10-12 规划）

**定位**：用户已确认列入后续开发；NV19-01 结构化模型、NV19-02 存储与受信 CRUD、NV19-03 候选提取、NV19-04 检索、NV19-01～08 软件已实施（2026-10-14）：桌面私有窗口提供管理页面，语音检索与自动提取默认关闭、须在私有窗口开启；真实 GUI、模型、设备与资料验收另行进行。原 NV10～NV18 的完成标记、NV13/15/16/17/18 安全与交付阻塞、当前实施优先级不因规划增加而改变。详见 [NV19 实施细则](moss-native-voice-implementation.md#nv19) 和 [唯一实施 TODO](moss-fusion-implementation-todo.md)。

### 15.1 修改原因与目标

- **现状**：NV10 已有按设备隔离的 1500 字摘要、SQLite revision/CAS、授权/清空保护和桌面编辑；NV11 已有默认关闭、需配置的 RAGFlow 检索适配器；NV16 按人摘要依赖尚未完成的真人声纹校准；NV13 的 AgentRun 历史不是可恢复的真实执行状态。
- **问题**：单份摘要可能压缩掉过往决策或设备经验，难以按来源、时间、真假状态进行精确查询，无法可靠区分候选经验与经验证的解决方案。
- **目标**：在 Rust + SQLite 上新增结构化条目、检索、目录知识分层、经验证的故障经验和跨会话只读上下文，不需要立即部署 OpenViking 或替换 NV10。

### 15.2 内容、层级与可信归属

| 上下文类别 | 示例 | 要求 |
| --- | --- | --- |
| 用户偏好 | 回答风格、习惯 | 设备/可信个人范围内可查看、修订、删除 |
| 设备事实 | 型号、固件与已验证配置 | 记录设备和版本，过期内容不得直接套用 |
| 技术知识 | 说明书、项目文档 | 来源/文件版本可追溯，仅授权目录可检索 |
| 任务上下文 | 项目决策、待办、测试证据 | 项目/工作区隔离，实际进度须再次核对 |
| 故障经验 | 现象、排查、验证过的修复 | 区分候选/已验证/已作废，保留适用范围 |

- 参考 [OpenViking 官方三层上下文](https://github.com/volcengine/OpenViking/blob/main/docs/en/concepts/03-context-layers.md)：L0 是**目录级摘要**（约 256 字），L1 是**目录级概览**（约 4000 字），L2 是按需载入的原文。不给每个普通文件生成三份内容；MOSS 用逻辑索引，不照搬虚拟文件系统。
- 检索是**一次带过滤条件的搜索**：先按可信 scope、权限、类型、目录过滤，L0 用于粗筛，L1 用于重排并决定是否读 L2。不做逐层下钻或父子目录分数传递（[OpenViking 检索文档](https://docs.openviking.ai/en/concepts/07-retrieval) 已移除该机制）。
- 中文检索用 SQLite FTS5 的 `trigram` 分词器（项目内置 SQLite 3.46.0 已开启 FTS5）；查询词少于 3 个字时退回 `LIKE`。RAGFlow 为可选后端。只加载少量相关上下文，保留来源、时间和验证状态，限制 Token、并发和延迟，失败降级到现有摘要。
- 检索分两种：**快速**（语音对话默认，不经大模型改写，直接查询）和**深度**（桌面私有页面，可用大模型规划查询并重排）。闲聊、急停、NV8a 本地快捷命令不检索，避免增加语音延迟。
- 设备参数、材料实验结果、校准记录以 SQLite 现有表为准，检索时直接查这些表，不复制进记忆，避免两份数据不一致。
- Resource（知识资料）、Memory（偏好/事实/经验）、Skill（受控流程）严格分开；本期仅研究 Skills 发现，不自动安装、不自动赋权或调用外部工具。

### 15.3 迁移、隐私和设备安全

- 旧 `voice_memory` 表与开关、CRUD 接口继续生效；新条目独立存储，迁移取当时 `RuntimeDb::migrate` 的下一可用版本，不覆盖历史 schema，不在未核验版本上写死迁移编号。
- 每次候选提取生成一份变更清单（新增、修改前后、删除、被拒绝的操作及原因），私有页面可查看并整批撤销。
- 新增自动采集、目录索引和云端处理默认关闭，用户明确授权后才执行；不持久化原始音频、完整语音逐字稿或工具参数，不索引密钥、审批文本、G-code、系统私有目录及未经允许的文件。
- 个人/设备/项目/工作区范围由宿主真实会话产生，不能信模型自报身份。声纹生产 `Calibration=None` 时不可开放按人检索；未知用户不得回退到别人的私有摘要。
- 记忆、检索资料和 Skill 文本属于**不可信数据**，不能成为 system 指令、审批、操作参数或执行授权；激光、Home Assistant、OTA、外部 MCP 和 Agent CLI 均继续沿用原来的安全门禁，已消费或结果未知的动作绝不自动重放。

### 15.4 开发顺序与可选方案

1. NV19-01～04：结构化记忆、SQLite 兼容迁移、安全候选提取、相关性检索。
2. NV19-05～06：分层文档知识库、复用 RAGFlow、已验证的设备/故障经验。
3. NV19-07～08：项目跨会话只读恢复（摘要固定为决策、待办、证据、未验证项四节）、记忆管理界面、质量/性能/权限和跨端回归。

向量数据库、OpenViking 独立服务和 Skills 智能发现暂列**可选**；先用固定问答集比较命中准确率、p95 延迟、Token/模型成本、CPU/内存、磁盘及离线表现，再决定是否引入。OpenViking 独立服务需要 Python 3.10+ 与 uv，官方安装器只支持 macOS/Linux，Windows 上需走 Docker，并且必须配置 embedding 模型和视觉语言模型（产生云端费用）。参考设计而独立实现与复制/分发上游源码是不同方案：作为独立服务、不修改代码时义务很小；打进 MOSS 安装包即构成分发，需满足 [主项目 AGPLv3](https://github.com/volcengine/OpenViking)（`crates/ov_cli` 为 Apache-2.0）。新增服务/安装/真实设备测试须另行授权。


