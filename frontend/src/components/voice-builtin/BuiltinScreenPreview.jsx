import React, { useState } from "react";
import { Bot, Satellite, Send, Sparkles } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup, gotoVoiceGroup } from "../VoiceLayout.jsx";
import {
  GOGGLE_BOT_POSES,
  SCREEN_CARD_STYLE_OPTIONS,
  builtinDeviceView,
  formatBytes,
  localModelView,
  voiceErrorText,
} from "../../voiceState.js";

/**
 * 副屏预览：小智彩屏画风（点选即存，在线设备不用重连）与 12 个全屏场景试看。n69 起是 `screen_card_style` 唯一的编辑处。
 * n70（2026-11-08）：刷了全屏画风包的小智按场景铺满整屏、一直显示；没刷的只能中央半屏试看 5 秒。
 */
export function BuiltinScreenPreview({ builtin }) {
  const { status, busy, act } = builtin;
  const [asking, setAsking] = useState("");
  const [botNotice, setBotNotice] = useState("");
  const [optimisticStyle, setOptimisticStyle] = useState("");
  const [lastPushedPose, setLastPushedPose] = useState("");
  if (!status || status.error) return null;

  const devices = Array.isArray(status.devices) ? status.devices : [];
  const sessions = Array.isArray(status.sessions) ? status.sessions : [];
  const savedCardStyle = status.local?.settings?.screen_card_style === "goggle_bot" ? "goggle_bot" : "moss_hud";
  const currentCardStyle = optimisticStyle || savedCardStyle;
  const legacyBotGifs = Array.isArray(status.goggle_bot_gifs)
    ? Object.fromEntries(status.goggle_bot_gifs.filter((item) => item?.id && item?.data_url).map((item) => [item.id, item.data_url]))
    : (status.goggle_bot_gifs && typeof status.goggle_bot_gifs === "object" ? status.goggle_bot_gifs : {});
  const styleGifCatalog = status.screen_style_gifs && typeof status.screen_style_gifs === "object"
    ? status.screen_style_gifs
    : {};
  const activeSceneGifs = currentCardStyle === "goggle_bot"
    ? { ...legacyBotGifs, ...(styleGifCatalog.goggle_bot || {}) }
    : (styleGifCatalog.moss_hud || {});
  const onlineColorDevice = devices.find((d) => {
    if (d.status !== "allowed") return false;
    const sess = sessions.find((s) => s.device_id === d.device_id);
    return Boolean(sess && sess.screen_monochrome !== true);
  }) || null;
  const styleLabel = SCREEN_CARD_STYLE_OPTIONS.find((item) => item.id === currentCardStyle)?.shortLabel || currentCardStyle;
  const onlinePackReady = Boolean(onlineColorDevice) && sessions.some((s) => s.device_id === onlineColorDevice.device_id && s.screen_pack === true);
  const autoKeep = status.local?.settings?.screen_pack_auto_keep === true;
  const alwaysOnline = status.mqtt_port != null;
  const fullFontModel = (Array.isArray(status.local?.models) ? status.local.models : []).find((m) => m.kind === "font") || null;
  const fullFontView = fullFontModel ? localModelView(fullFontModel) : null;

  // 2026-11-09 用户决定：自动保持全屏画风（默认关）。开启时确认一次，之后 MOSS 自动给没装好的彩屏小智推送画风包
  const toggleAutoKeep = async (on) => {
    if (on) {
      const ok = await showConfirm(
        "开启后，MOSS 每次打开、每台彩屏小智连上来时会按「版本前缀 + 内容指纹」自动检查：还没装全屏画风，或换了画风、基础底包、字库有更新的，就在它空闲时自动推送，不再逐次询问。\n" +
          "推送会让小智重启一次，开机后自己下载并写入资源分区（不刷主程序），沿用它原来的唤醒词；期间不要断电。\n" +
          "加工中、有待批准请求、小智正在对话时不推；同一台一天最多自动推一次（主动换画风或导入/移除底包会立即放行），结果记在“记录与诊断”。装好以后上电就是全屏画面，指纹一致不再推送。\n" +
          "只支持 ESP32-S3、16 MB 闪存、小智固件 2.0.2 以上的彩屏设备。随时可以关掉。",
        { title: "开启自动保持全屏画风？", tone: "warning", danger: true, confirmText: "开启并允许自动推送" },
      );
      if (!ok) return;
    }
    await act(on ? "开启自动保持全屏画风" : "关闭自动保持全屏画风", () => voiceApi.localSave({ screen_pack_auto_keep: on }));
  };

  const switchCardStyle = async (nextStyle) => {
    if (nextStyle === currentCardStyle) return;
    setOptimisticStyle(nextStyle);
    setBotNotice("");
    const opt = SCREEN_CARD_STYLE_OPTIONS.find((item) => item.id === nextStyle);
    const updated = await act("切换卡片画风", () => voiceApi.localSave({ screen_card_style: nextStyle }));
    if (updated) {
      setOptimisticStyle("");
      if (onlineColorDevice && lastPushedPose) {
        try {
          await voiceApi.deviceRequest(onlineColorDevice.device_id, "push_bot_pose", {
            pose: lastPushedPose,
            style: nextStyle,
          });
          setBotNotice(`已切换为「${opt?.shortLabel || nextStyle}」画风，并同步刷新在线小智屏幕${onlinePackReady ? (autoKeep ? "；画风包会自动按新画风重刷（小智重启一次），之后开机待命画面也换成新画风" : "；开机待命画面要重刷画风包才换：到“唤醒词”点“一键重刷全屏画风”") : ""}`);
          return;
        } catch {
          // 若设备刚好断开，不影响画风保存提示
        }
      }
      setBotNotice(`已换成「${opt?.shortLabel || nextStyle}」画风，在线小智不用重连，马上生效${onlinePackReady ? (autoKeep ? "；画风包会自动按新画风重刷（小智重启一次），之后开机待命画面也换成新画风" : "；开机待命画面要重刷画风包才换：到“唤醒词”点“一键重刷全屏画风”") : ""}`);
    } else {
      setOptimisticStyle("");
    }
  };

  const pushBotPose = async (pose) => {
    if (!onlineColorDevice) return;
    const sceneTitle = currentCardStyle === "goggle_bot" ? (pose.botLabel || pose.label) : (pose.mossLabel || pose.label);
    setAsking(onlineColorDevice.device_id);
    setBotNotice("");
    try {
      await voiceApi.deviceRequest(onlineColorDevice.device_id, "push_bot_pose", {
        pose: pose.id,
        style: currentCardStyle,
      });
      setLastPushedPose(pose.id);
      setBotNotice(onlinePackReady
        ? `${builtinDeviceView(onlineColorDevice).title} 已切到「${sceneTitle}」全屏画面`
        : `已将「${sceneTitle}」投到 ${builtinDeviceView(onlineColorDevice).title} 屏幕中央试看（没刷全屏画风，5 秒后回到官方画面）`);
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "投送屏幕预览失败", tone: "error" });
    } finally {
      setAsking("");
    }
  };

  return (
    <VoiceGroup
      id="screen-preview"
      title="副屏预览（小智彩屏画风）"
      summary={`当前画风：${styleLabel} · 12 个全屏场景${onlineColorDevice ? (onlinePackReady ? " · 小智已刷全屏画风" : " · 可推到小智试看") : ""}`}
    >
      <div
        className={`voice-bot-showcase ${currentCardStyle === "goggle_bot" ? "voice-bot-showcase--bot" : "voice-bot-showcase--moss"}`}
        id="voice-goggle-bot-card"
        data-card-style={currentCardStyle}
      >
        <div className="voice-bot-showcase-head">
          <div className="voice-bot-showcase-title">
            <span className="voice-bot-showcase-mascot" aria-hidden="true">
              {currentCardStyle === "goggle_bot" ? <Bot size={18} /> : <Satellite size={18} />}
            </span>
            <span>
              <b>小智副屏预览</b>
              <small>12 个全屏场景 · 这里缩小一半预览；刷进小智后铺满整屏、一直显示，固件的状态字和字幕叠在画面上</small>
            </span>
          </div>
          <div className="voice-bot-style-switch" role="radiogroup" aria-label="小智彩屏卡片画风">
            {SCREEN_CARD_STYLE_OPTIONS.map((opt) => {
              const active = currentCardStyle === opt.id;
              const Icon = opt.id === "goggle_bot" ? Bot : Satellite;
              return (
                <button
                  key={opt.id}
                  type="button"
                  role="radio"
                  aria-checked={active}
                  className={`voice-bot-style-opt${active ? " is-active" : ""}`}
                  disabled={Boolean(busy)}
                  title={opt.hint}
                  id={`voice-card-style-${opt.id}`}
                  onClick={() => switchCardStyle(opt.id)}
                >
                  <Icon size={14} aria-hidden="true" />
                  {opt.shortLabel}
                </button>
              );
            })}
          </div>
        </div>
        {!onlineColorDevice && (
          <div className="voice-bot-offline-tip" id="voice-bot-offline-tip">
            <Sparkles size={13} aria-hidden="true" />
            连上并允许一台彩屏小智（如立创·实战派 320×240）后，点画面右上角的小飞机就能推过去试看
          </div>
        )}
        {onlineColorDevice && (
          <div className={`voice-bot-pack-tip${onlinePackReady ? " is-ready" : ""}`} id="voice-bot-pack-tip">
            <Sparkles size={13} aria-hidden="true" />
            {onlinePackReady ? (
              <span>{builtinDeviceView(onlineColorDevice).title} 已刷全屏画风：对话、雕刻、审批、听歌都铺满整屏一直显示；点小飞机直接切到那个画面。</span>
            ) : (
              <span>
                {builtinDeviceView(onlineColorDevice).title} 还没刷全屏画风，现在只能在屏幕中央半屏试看 5 秒。
                刷入后上电就是全屏画面、不用等 MOSS：在“唤醒词”点“一键刷入全屏画风”并确认一次（写一次资源分区并重启，不刷主程序；下载完会自动标为已刷）。
                <button type="button" className="voice-link-btn" id="voice-bot-pack-goto" onClick={() => gotoVoiceGroup("devices", "wake-word", "voice-wakeword-screen-pack")}>去一键刷入</button>
              </span>
            )}
          </div>
        )}
        <div className="voice-pack-auto" id="voice-screen-pack-auto">
          <label className="voice-check">
            <input type="checkbox" id="voice-screen-pack-auto-keep" checked={autoKeep} disabled={busy} onChange={(e) => toggleAutoKeep(e.target.checked)} />
            <span>
              自动保持全屏画风
              <small>按版本前缀 + 内容指纹自动检查：新接入、换画风、换底包或字库更新时，空闲自动同步到彩屏小智（开启时确认一次）</small>
            </span>
          </label>
          {autoKeep && !alwaysOnline && (
            <p className="voice-muted" id="voice-screen-pack-auto-mqtt-tip">
              建议同时打开“服务 › 常连模式（实验）”：小智开机就连上 MOSS，不用先唤醒，打开 MOSS 后马上就能检查补刷。
              <button type="button" className="voice-link-btn" onClick={() => gotoVoiceGroup("service", "settings-builtinMqtt")}>去打开</button>
            </p>
          )}
          {fullFontModel && fullFontView && (
            <p className="voice-muted" id="voice-screen-full-font-tip">
              {fullFontModel.installed
                ? "三层字库已齐备：日常 6,845 字随资源包内置，生僻字、繁体与日韩泰阿共 48,662 字由服务端按句动态补齐。"
                : fullFontView.canCancel
                  ? `正在后台准备小智全量补字库（${fullFontView.text}），完成后生僻字与多语种字幕自动热生效。`
                  : "小智全量补字库尚未就绪（当前由内置 6,845 日常字库兜底）："}
              {fullFontView.canDownload && (
                <button
                  type="button"
                  className="voice-link-btn"
                  disabled={Boolean(busy)}
                  id="voice-screen-full-font-download"
                  onClick={() => act("下载小智全量补字库", () => voiceApi.localDownload(fullFontModel.id))}
                >
                  立即下载（{formatBytes(fullFontModel.size)}）
                </button>
              )}
            </p>
          )}
        </div>
        <div className="voice-bot-gif-grid" id="voice-goggle-bot-gallery">
          {GOGGLE_BOT_POSES.map((pose) => {
            const gifSrc = activeSceneGifs[pose.id] || legacyBotGifs[pose.id];
            const sceneTitle = currentCardStyle === "goggle_bot" ? (pose.botLabel || pose.label) : (pose.mossLabel || pose.label);
            const sceneDesc = currentCardStyle === "goggle_bot" ? (pose.botDesc || pose.desc) : (pose.mossDesc || pose.desc);
            // 只标记本页刚推送过的那一屏：固件会按工作状态自动换卡，这里不冒充“设备当前画面”。
            const pushed = Boolean(onlineColorDevice) && lastPushedPose === pose.id;
            return (
              <div className={`voice-bot-gif-tile${pushed ? " is-pushed" : ""}`} key={pose.id} data-pose={pose.id}>
                <div className="voice-bot-gif-frame">
                  {gifSrc ? (
                    <img
                      key={`${currentCardStyle}-${pose.id}`}
                      src={gifSrc}
                      alt={`${sceneTitle}`}
                      className="voice-bot-gif-img"
                      width={160}
                      height={120}
                    />
                  ) : (
                    <span className="voice-muted mono">160×120</span>
                  )}
                  {pushed && <span className="voice-bot-gif-badge">刚推到小智</span>}
                  {onlineColorDevice && (
                    <button
                      type="button"
                      className="voice-bot-push"
                      disabled={Boolean(asking) || Boolean(busy)}
                      title={`把「${sceneTitle}」推到 ${builtinDeviceView(onlineColorDevice).title} 的屏幕上试看`}
                      aria-label={`把${sceneTitle}推到小智屏`}
                      id={`voice-push-bot-${pose.id}`}
                      onClick={() => pushBotPose(pose)}
                    >
                      <Send size={13} aria-hidden="true" />
                    </button>
                  )}
                </div>
                <div className="voice-bot-gif-title">{sceneTitle}</div>
                <div className="voice-bot-gif-desc" title={sceneDesc}>{sceneDesc}</div>
              </div>
            );
          })}
        </div>
        <details className="voice-bot-showcase-more">
          <summary>这些画面什么时候出现？</summary>
          <ul>
            <li>待命：固件自己显示，跟随刷包时选的画风</li>
            <li>对话：聆听 → 思考 → 说话；回复里带表情时换成开心 / 难过 / 惊讶</li>
            <li>开始雕刻后：雕刻画面，中央叠这次的刀路和进度，字幕写百分比与剩余时间；生成预览时不推</li>
            <li>等电脑点批准：警戒画面，倒计时写在字幕里</li>
            <li>雕完或急停：完工 / 急停画面停约 10 秒后回到聆听</li>
            <li>放本地音乐：唱机 / 听歌小机器人，停播为止</li>
            <li>没刷全屏画风的小智：雕刻进度、审批、完工等仍在屏幕中央半屏显示，几秒后回到官方画面</li>
          </ul>
        </details>
        {botNotice && <div className="voice-ok" id="voice-bot-showcase-notice">{botNotice}</div>}
      </div>
    </VoiceGroup>
  );
}
