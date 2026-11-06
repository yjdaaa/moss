import React, { useId } from "react";
import { mossEyeCorePalette } from "../voiceState.js";

/** 四角星星（漫画“闪光”符号），以 (0,0) 为中心、半径 6。 */
const STAR = "M0 -6 Q0.9 -0.9 6 0 Q0.9 0.9 0 6 Q-0.9 0.9 -6 0 Q-0.9 -0.9 0 -6Z";

/**
 * MOSS 机械眼（2026-11-06 用户要求“可爱风一点”，左上角导航眼与语音中心孪生舱眼统一为同一组件）。
 * - 同一套形状、配色（`mossEyeCorePalette`）与动画节奏，按 `size` 决定细节多少：
 *   `sm`（导航 40px）不画眼睛两侧的声波弧；`lg`（孪生舱）多一圈星点轨道。
 * - 造型沿用用户认可的“闭眼睡觉”画法（同日反馈：写实眼球“恐怖”、豆豆眼“不像眼睛”）：
 *   睁眼 = 与睡觉弧线同粗同色的上眼睑线 + 两根小睫毛 + 上暗下亮的大虹膜 + 两点大高光，眨眼时整只眼压成一条线；
 *   每个状态配一个漫画符号特效，与睡觉的“z”同一语言：待命闪星、拾音/播报两侧声波、思考冒泡、雕刻火花、警戒“!”。
 * - `hardware` 只在导航眼上传入：unprobed / online / engraving / fault 用外圈颜色表示激光机状态。
 * - 视线跟随读取祖先元素上的 CSS 变量 `--eye-gaze-x` / `--eye-gaze-y`（-1..1），本组件不监听指针。
 */
export function MossEye({ state = "idle", level = 0, hardware = "", size = "sm", className = "" }) {
  const palette = mossEyeCorePalette(state);
  const ratio = Math.max(0, Math.min(1, (Number(level) || 0) / 100));
  const uid = useId().replace(/:/g, "");
  const irisId = `${uid}-moss-eye-iris`;
  const glowId = `${uid}-moss-eye-glow`;
  const lidClipId = `${uid}-moss-eye-lid`;
  const classes = [
    "moss-eye",
    `moss-eye--${size}`,
    `moss-eye--${state}`,
    hardware ? `moss-eye-hw--${hardware}` : "",
    className,
  ].filter(Boolean).join(" ");
  return (
    <svg
      className={classes}
      viewBox="0 0 100 100"
      aria-hidden="true"
      data-tempo={palette.tempo}
      style={{ "--eye-level": ratio.toFixed(2), "--eye-accent": palette.accent }}
    >
      <defs>
        <linearGradient id={irisId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={palette.coreStops[4]} />
          <stop offset="42%" stopColor={palette.coreStops[3]} />
          <stop offset="82%" stopColor={palette.coreStops[2]} />
          <stop offset="100%" stopColor={palette.coreStops[1]} />
        </linearGradient>
        <radialGradient id={glowId} cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor={palette.glowStops[0]} />
          <stop offset="66%" stopColor={palette.glowStops[1]} />
          <stop offset="100%" stopColor={palette.glowStops[2]} />
        </radialGradient>
        {/* 虹膜只露出上眼睑线以下的部分 */}
        <clipPath id={lidClipId}>
          <path d="M18 47 Q50 25 82 47 L82 92 L18 92 Z" />
        </clipPath>
      </defs>
      <circle className="moss-eye-halo" cx="50" cy="50" r="49" fill={`url(#${glowId})`} />
      {hardware && <circle className="moss-eye-hw" cx="50" cy="50" r="46" fill="none" strokeWidth="5" />}
      {size === "lg" && (
        <circle className="moss-eye-orbit" cx="50" cy="50" r="46" fill="none" strokeWidth="3.2" strokeLinecap="round" strokeDasharray="0.1 8.6" />
      )}
      <circle className="moss-eye-socket" cx="50" cy="50" r="38" strokeWidth="4" />

      <g className="moss-eye-ball">
        <g clipPath={`url(#${lidClipId})`}>
          <g className="moss-eye-look">
            <g className="moss-eye-shape">
              <ellipse className="moss-eye-iris" cx="50" cy="55" rx="15.5" ry="18" fill={`url(#${irisId})`} />
              <ellipse className="moss-eye-glint" cx="44.5" cy="51" rx="5" ry="6" />
              <circle className="moss-eye-glint-sub" cx="56.5" cy="64" r="2.6" />
              <circle className="moss-eye-glint-sub" cx="56.5" cy="51.5" r="1.6" />
            </g>
          </g>
        </g>
        <path className="moss-eye-lid" d="M31 47 Q50 30 69 47" fill="none" strokeWidth="5" strokeLinecap="round" />
        <path className="moss-eye-lash" d="M33.5 43.5 L27.5 38.5 M66.5 43.5 L72.5 38.5" fill="none" strokeWidth="4" strokeLinecap="round" />
      </g>

      <g className="moss-eye-sleep">
        <path d="M33 50 Q50 64 67 50" fill="none" strokeWidth="5" strokeLinecap="round" />
        <text x="66" y="34" className="moss-eye-z">z</text>
      </g>

      {/* 漫画符号特效：按状态由 CSS 显隐，均为装饰（aria-hidden 已在根节点）。 */}
      <g className="moss-eye-fx moss-eye-fx--sparkle">
        <path d={STAR} transform="translate(85 15) scale(1.15)" />
        <path d={STAR} transform="translate(14 84) scale(0.8)" />
        <path d={STAR} transform="translate(90 74) scale(0.6)" />
      </g>
      <g className="moss-eye-fx moss-eye-fx--waves" fill="none" strokeWidth="4" strokeLinecap="round">
        <path className="moss-eye-wave-1" d="M7 39 Q0 50 7 61" />
        <path className="moss-eye-wave-2" d="M-3 32 Q-13 50 -3 68" />
        <path className="moss-eye-wave-1" d="M93 39 Q100 50 93 61" />
        <path className="moss-eye-wave-2" d="M103 32 Q113 50 103 68" />
      </g>
      <g className="moss-eye-fx moss-eye-fx--think">
        <circle className="moss-eye-bubble-1" cx="76" cy="17" r="3" />
        <circle className="moss-eye-bubble-2" cx="85" cy="9" r="4.2" />
        <circle className="moss-eye-bubble-3" cx="96" cy="0" r="5.6" />
      </g>
      <g className="moss-eye-fx moss-eye-fx--sparks">
        <path d={STAR} transform="translate(86 80) scale(0.9)" />
        <path d={STAR} transform="translate(75 92) scale(0.6)" />
        <path d={STAR} transform="translate(95 64) scale(0.55)" />
      </g>
      <g className="moss-eye-fx moss-eye-fx--alert">
        <text x="83" y="25" className="moss-eye-bang">!</text>
      </g>
    </svg>
  );
}

export default MossEye;
