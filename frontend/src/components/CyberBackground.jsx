import React, { useEffect, useRef } from "react";

/**
 * MOSS 等离子流体背景（固定空间锚点版，无鼠标视差偏移）
 *
 * - WebGL 片元着色器：多层域扭曲噪声，紫 / 赤红 / 蓝等离子在原地缓慢流动；移除指针偏移保持空间稳定不眩晕。
 * - 叠加一层 2D 星点（原位闪烁与极缓漂移），以及静态暗角与细噪点。
 * - 性能与合规：着色器以 50% 分辨率渲染后由 CSS 放大；限帧约 40 fps；页面隐藏或窗口不在前台（切到别的应用、
 *   最小化、收进托盘）时暂停，回到前台继续，并给 <html> 标 `data-app-active="false"` 让页面 CSS 循环动画
 *   一起暂停；启动 2 秒内自测帧率，低于 28 fps 自动降到 33% 分辨率并减少星点；绝不调用 `gl.clear`。
 * - WebGL 不可用时退回 CSS 静态渐变（不报错、不影响业务）；prefers-reduced-motion 只绘制一帧静态画面。
 * - 纯装饰：aria-hidden、pointer-events: none，不参与任何业务逻辑。
 */

const VERTEX_SHADER = "attribute vec2 a;void main(){gl_Position=vec4(a,0.0,1.0);}";

function buildFragmentShader(highPrecision) {
  const precision = highPrecision ? "precision highp float;" : "precision mediump float;";
  return `${precision}
uniform vec2 r;uniform float t;
float h(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
float n(vec2 p){vec2 i=floor(p),f=fract(p);f=f*f*(3.0-2.0*f);
  return mix(mix(h(i),h(i+vec2(1.0,0.0)),f.x),mix(h(i+vec2(0.0,1.0)),h(i+vec2(1.0,1.0)),f.x),f.y);}
float fbm(vec2 p){float v=0.0,a=0.5;mat2 R=mat2(0.8,0.6,-0.6,0.8);
  for(int i=0;i<5;i++){v+=a*n(p);p=R*p*2.02;a*=0.5;}return v;}
void main(){
  vec2 p=(gl_FragCoord.xy-0.5*r)/max(r.y,1.0);
  vec2 q=vec2(fbm(p*1.6+t*0.04),fbm(p*1.6+vec2(5.2,1.3)-t*0.035));
  vec2 w=vec2(fbm(p*1.6+3.8*q+vec2(1.7,9.2)+t*0.07),fbm(p*1.6+3.8*q+vec2(8.3,2.8)-t*0.05));
  float f=fbm(p*1.6+4.0*w);
  vec3 c=mix(vec3(0.02,0.015,0.05),vec3(0.33,0.22,0.95),clamp(f*f*2.2,0.0,1.0));
  c=mix(c,vec3(1.0,0.23,0.36),clamp(length(q)*0.95-0.38,0.0,1.0)*0.85);
  c=mix(c,vec3(0.23,0.45,1.0),clamp(w.y*w.y*1.2,0.0,1.0)*0.55);
  c+=pow(clamp(f,0.0,1.0),5.0)*vec3(1.0,0.82,0.92)*1.6;
  c*=(1.0-smoothstep(0.15,1.5,length(p-vec2(0.15,0.1))))*0.78;
  gl_FragColor=vec4(c,1.0);
}`;
}

const FRAME_INTERVAL_MS = 24; // ≈ 40 fps
const PROBE_WINDOW_MS = 2000;
const LOW_FPS_THRESHOLD = 28;

function createPlasma(canvas) {
  const gl = canvas.getContext("webgl", { premultipliedAlpha: false, antialias: false, powerPreference: "low-power" });
  if (!gl) return null;
  const compile = (type, source) => {
    const shader = gl.createShader(type);
    if (!shader) return null;
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader;
    gl.deleteShader(shader);
    return null;
  };
  const vs = compile(gl.VERTEX_SHADER, VERTEX_SHADER);
  if (!vs) return null;
  const highpFmt = gl.getShaderPrecisionFormat?.(gl.FRAGMENT_SHADER, gl.HIGH_FLOAT);
  const preferHighp = Boolean(highpFmt && highpFmt.precision > 0);
  const fs = (preferHighp ? compile(gl.FRAGMENT_SHADER, buildFragmentShader(true)) : null)
    || compile(gl.FRAGMENT_SHADER, buildFragmentShader(false));
  if (!fs) {
    gl.deleteShader(vs);
    return null;
  }
  const program = gl.createProgram();
  if (!program) {
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    return null;
  }
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    gl.deleteProgram(program);
    gl.deleteShader(vs);
    gl.deleteShader(fs);
    return null;
  }
  gl.useProgram(program);
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(program, "a");
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  return {
    gl,
    dispose() {
      gl.deleteBuffer(buffer);
      gl.deleteProgram(program);
      gl.deleteShader(vs);
      gl.deleteShader(fs);
      const lose = gl.getExtension("WEBGL_lose_context");
      if (lose) lose.loseContext();
    },
    uniforms: {
      r: gl.getUniformLocation(program, "r"),
      t: gl.getUniformLocation(program, "t"),
    },
  };
}

export function CyberBackground() {
  const plasmaRef = useRef(null);
  const starsRef = useRef(null);
  const rootRef = useRef(null);

  useEffect(() => {
    const plasmaCanvas = plasmaRef.current;
    const starCanvas = starsRef.current;
    const root = rootRef.current;
    if (!plasmaCanvas || !starCanvas || !root) return undefined;

    const reduceMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;
    const plasma = createPlasma(plasmaCanvas);
    if (!plasma) root.classList.add("plasma-bg--fallback");
    const ctx = starCanvas.getContext("2d");

    let animId = null;
    let focused = typeof document.hasFocus === "function" ? document.hasFocus() : true;
    let isVisible = !document.hidden && focused;
    let lowPower = false;
    let builtLowPower = null;
    let lastFrame = 0;
    let lastDrawTime = 0;
    let probeStart = 0;
    let probeFrames = 0;
    let dpr = 1;
    let W = 0;
    let H = 0;
    let stars = [];

    const buildStars = () => {
      const density = lowPower ? 0.15 : 0.35;
      const count = Math.round(((window.innerWidth * window.innerHeight) / 2200) * density);
      stars = Array.from({ length: count }, () => ({
        x: Math.random() * W,
        y: Math.random() * H,
        r: (0.3 + Math.random() ** 2 * 1.4) * dpr,
        phase: Math.random() * Math.PI * 2,
        speed: 0.5 + Math.random() * 1.5,
        depth: [0.25, 0.6, 1][Math.floor(Math.random() * 3)],
        color: Math.random() < 0.1 ? "255,190,200" : Math.random() < 0.25 ? "190,205,255" : "255,255,255",
      }));
    };

    const resize = () => {
      const nextDpr = Math.min(window.devicePixelRatio || 1, 1.25);
      const nextW = Math.max(1, Math.round(window.innerWidth * nextDpr));
      const nextH = Math.max(1, Math.round(window.innerHeight * nextDpr));
      let changed = builtLowPower !== lowPower;
      builtLowPower = lowPower;
      if (W !== nextW || H !== nextH || starCanvas.width !== nextW || starCanvas.height !== nextH || dpr !== nextDpr) {
        dpr = nextDpr;
        W = starCanvas.width = nextW;
        H = starCanvas.height = nextH;
        changed = true;
      }
      if (plasma) {
        const scale = lowPower ? 0.33 : 0.5;
        const nextPW = Math.max(1, Math.round(window.innerWidth * scale));
        const nextPH = Math.max(1, Math.round(window.innerHeight * scale));
        if (plasmaCanvas.width !== nextPW || plasmaCanvas.height !== nextPH) {
          plasmaCanvas.width = nextPW;
          plasmaCanvas.height = nextPH;
          plasma.gl.viewport(0, 0, nextPW, nextPH);
          changed = true;
        }
      }
      if (!changed) return false;
      lastFrame = 0;
      buildStars();
      return true;
    };

    const drawFrame = (time, advance = true) => {
      if (advance) lastDrawTime = time;
      if (plasma) {
        const { gl, uniforms } = plasma;
        gl.uniform2f(uniforms.r, plasmaCanvas.width, plasmaCanvas.height);
        gl.uniform1f(uniforms.t, time / 1000);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
      }
      if (ctx) {
        ctx.clearRect(0, 0, W, H);
        for (const s of stars) {
          if (!reduceMotion && advance) {
            s.x -= 0.01 * s.depth * dpr;
            if (s.x < -5) s.x = W + 5;
          }
          const alpha = 0.3 + 0.7 * Math.abs(Math.sin(s.phase + (time / 1000) * s.speed));
          ctx.fillStyle = `rgba(${s.color},${alpha})`;
          ctx.beginPath();
          ctx.arc(s.x, s.y, s.r, 0, Math.PI * 2);
          ctx.fill();
        }
      }
    };

    const loop = (time) => {
      if (!isVisible) {
        animId = null;
        return;
      }
      if (!probeStart) probeStart = time;
      probeFrames += 1;
      if (probeStart > 0 && time - probeStart > PROBE_WINDOW_MS) {
        const fps = probeFrames / ((time - probeStart) / 1000);
        if (fps < LOW_FPS_THRESHOLD && !lowPower) {
          lowPower = true;
          root.classList.add("plasma-bg--low");
          resize();
        }
        probeStart = -1;
      }
      if (lastFrame === 0 || time - lastFrame >= FRAME_INTERVAL_MS) {
        lastFrame = time;
        drawFrame(time, true);
      }
      animId = requestAnimationFrame(loop);
    };

    const markState = () => {
      root.dataset.state = reduceMotion ? "static" : isVisible ? "running" : "paused";
      // 整个应用在后台时，页面上的 CSS 循环动画也一起暂停（styles.css `[data-app-active="false"]`）
      document.documentElement.dataset.appActive = isVisible ? "true" : "false";
    };
    const onVisibility = () => {
      isVisible = !document.hidden && focused;
      if (!isVisible && probeStart > 0) {
        // 隐藏期间不计入帧率自检，回到前台后重新测
        probeStart = 0;
        probeFrames = 0;
      }
      if (isVisible) {
        lastFrame = 0;
        if (!reduceMotion && !animId) animId = requestAnimationFrame(loop);
      }
      markState();
    };
    const onFocus = () => {
      focused = true;
      onVisibility();
    };
    const onBlur = () => {
      focused = false;
      onVisibility();
    };
    const onResize = () => {
      if (!resize()) return;
      drawFrame(lastDrawTime, false);
    };

    resize();
    window.addEventListener("resize", onResize);
    document.addEventListener("visibilitychange", onVisibility);
    window.addEventListener("focus", onFocus);
    window.addEventListener("blur", onBlur);
    markState();
    if (reduceMotion) {
      drawFrame(0, false);
    } else {
      // 启动时不在前台也先画一帧，窗口出现时不是空白
      if (isVisible) animId = requestAnimationFrame(loop);
      else drawFrame(0, false);
    }

    return () => {
      if (animId) cancelAnimationFrame(animId);
      window.removeEventListener("resize", onResize);
      document.removeEventListener("visibilitychange", onVisibility);
      window.removeEventListener("focus", onFocus);
      window.removeEventListener("blur", onBlur);
      delete document.documentElement.dataset.appActive;
      plasma?.dispose();
    };
  }, []);

  return (
    <div ref={rootRef} className="cyber-background-container plasma-bg" aria-hidden="true">
      <canvas ref={plasmaRef} className="plasma-bg__fluid" />
      <canvas ref={starsRef} className="plasma-bg__stars" />
      <div className="plasma-bg__vignette" />
      <div className="cyber-noise-overlay" />
    </div>
  );
}
