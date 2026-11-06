import React, { useId } from "react";
import { mossEyeCorePalette } from "../voiceState.js";

/** 7×7 点阵几何（viewBox 0..100）：点距 7.5、首点 27.5，四角距圆心约 31.8，落在外圈 r38 以内。 */
const GRID = 7;
const PITCH = 7.5;
const ORIGIN = 27.5;
const DOT_R = 2.9;

/** "#" 为亮点，按行以空白分隔，返回 [行, 列] 列表。 */
function bitmap(rows) {
  return rows
    .trim()
    .split(/\s+/)
    .flatMap((row, r) => [...row].map((ch, c) => (ch === "#" ? [r, c] : null)).filter(Boolean));
}

const SMILE = bitmap(".......  .#...#.  .#...#.  .......  #.....#  .#####.  .......");
const SMILE_BLINK = bitmap(".......  .......  ##...##  .......  #.....#  .#####.  .......");
/** 思考转圈：外环 8 个点顺时针轮流点亮，前一个点留半亮拖尾。 */
const SPINNER_RING = [[0, 3], [1, 5], [3, 6], [5, 5], [6, 3], [5, 1], [3, 0], [1, 1]];
/** 播报声波柱：7 列对称高度，按帧切换。 */
const EQ_FRAMES = [
  [1, 3, 5, 7, 5, 3, 1],
  [3, 5, 7, 5, 3, 5, 3],
  [1, 5, 3, 7, 3, 5, 1],
].map((heights) => heights.flatMap((h, c) => [...Array(GRID)].map((_, r) => (Math.abs(r - 3) <= (h - 1) / 2 ? [r, c] : null)).filter(Boolean)));
const spinner = {
  seconds: 1.2,
  base: [...SPINNER_RING, [3, 3]],
  frames: SPINNER_RING.map((cell, k) => ({ on: [cell], tail: [SPINNER_RING[(k + SPINNER_RING.length - 1) % SPINNER_RING.length]] })),
};

/**
 * 每个状态一段会动的点阵小图标（逐帧切换，帧数对应 styles.css 中的 moss-eye-pf2/3/6/8）。
 * 区分状态靠图形本身，不只靠颜色（2026-11-06 用户反馈旧版“状态之间看不出区别”）。
 */
const PIXEL_ANIMS = {
  idle: { seconds: 3.6, frames: [SMILE, SMILE, SMILE, SMILE, SMILE, SMILE_BLINK].map((on) => ({ on })) },
  listening: {
    seconds: 0.8,
    frames: [
      bitmap("..###..  ..###..  #.###.#  #.###.#  .#...#.  ..###..  ...#..."),
      bitmap("..###..  ..###..  ..###..  ..###..  .#...#.  ..###..  ...#..."),
    ].map((on) => ({ on })),
  },
  thinking: spinner,
  pulse: spinner,
  speaking: { seconds: 0.54, frames: EQ_FRAMES.map((on) => ({ on })) },
  engraving: {
    seconds: 0.6,
    frames: [
      bitmap("...#...  ...#...  .......  ##.#.##  .......  ...#...  ...#..."),
      bitmap("#..#..#  ...#...  .......  ##.#.##  .......  ...#...  #..#..#"),
    ].map((on) => ({ on })),
  },
  alert: {
    seconds: 0.7,
    frames: [bitmap("...#...  ...#...  ...#...  ...#...  ...#...  .......  ...#..."), []].map((on) => ({ on })),
  },
};

const ALL_CELLS = [...Array(GRID * GRID)].map((_, i) => [Math.floor(i / GRID), i % GRID]);

function Dot({ cell, className }) {
  const [r, c] = cell;
  return <circle className={className} cx={ORIGIN + c * PITCH} cy={ORIGIN + r * PITCH} r={DOT_R} />;
}

/**
 * MOSS 状态徽标（左上角导航眼与语音中心孪生舱共用）。
 * - 2026-11-06 用户在候选页中选定“像素点阵”方案：外圈 + 7×7 LED 点阵，与小智实机屏幕同一味道；
 *   待命笑脸眨眼、聆听麦克风、思考转圈、播报声波柱、雕刻激光十字、警戒闪烁“!”；离线沿用用户认可的“闭眼睡觉 + z”。
 * - 配色沿用 `mossEyeCorePalette`；`size` 只决定 class（`sm` 导航 40px / `lg` 孪生舱）。
 * - `hardware` 只在导航眼上传入：unprobed / online / engraving / fault 用外圈颜色表示激光机状态。
 * - 亮点层读取祖先元素上的 `--eye-gaze-x` / `--eye-gaze-y`（-1..1）轻微偏向指针，本组件不监听指针。
 */
export function MossEye({ state = "idle", level = 0, hardware = "", size = "sm", className = "" }) {
  const palette = mossEyeCorePalette(state);
  const ratio = Math.max(0, Math.min(1, (Number(level) || 0) / 100));
  const glowId = `${useId().replace(/:/g, "")}-moss-eye-glow`;
  const offline = state === "offline";
  const anim = PIXEL_ANIMS[state] || PIXEL_ANIMS.idle;
  const frameCount = anim.frames.length;
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
        <radialGradient id={glowId} cx="50%" cy="50%" r="50%">
          <stop offset="0%" stopColor={palette.glowStops[0]} />
          <stop offset="66%" stopColor={palette.glowStops[1]} />
          <stop offset="100%" stopColor={palette.glowStops[2]} />
        </radialGradient>
      </defs>
      <circle className="moss-eye-halo" cx="50" cy="50" r="49" fill={`url(#${glowId})`} />
      {hardware && <circle className="moss-eye-hw" cx="50" cy="50" r="46" fill="none" strokeWidth="5" />}
      <circle className="moss-eye-socket" cx="50" cy="50" r="38" strokeWidth="4" />

      {offline ? (
        <g className="moss-eye-sleep">
          <path d="M33 50 Q50 64 67 50" fill="none" strokeWidth="5" strokeLinecap="round" />
          <text x="66" y="34" className="moss-eye-z">z</text>
        </g>
      ) : (
        <g className="moss-eye-px">
          {ALL_CELLS.map((cell) => (
            <Dot key={`off-${cell[0]}-${cell[1]}`} cell={cell} className="moss-eye-px-off" />
          ))}
          <g className="moss-eye-px-lit">
            {(anim.base || []).map((cell) => (
              <Dot key={`base-${cell[0]}-${cell[1]}`} cell={cell} className="moss-eye-px-base" />
            ))}
            {anim.frames.map((frame, k) => (
              <g
                key={`frame-${k}`}
                className={`moss-eye-px-frame moss-eye-pf--${frameCount}`}
                style={{
                  "--px-t": `${anim.seconds}s`,
                  // 负延迟让第 k 帧恰好在第 k 段显示，且首次渲染就处于动画中（避免全部帧叠在一起）。
                  "--px-d": `${((k * anim.seconds) / frameCount - anim.seconds).toFixed(3)}s`,
                }}
              >
                {frame.on.map((cell) => (
                  <Dot key={`on-${cell[0]}-${cell[1]}`} cell={cell} className="moss-eye-px-on" />
                ))}
                {(frame.tail || []).map((cell) => (
                  <Dot key={`tail-${cell[0]}-${cell[1]}`} cell={cell} className="moss-eye-px-tail" />
                ))}
              </g>
            ))}
          </g>
        </g>
      )}
    </svg>
  );
}

export default MossEye;
