import React, { useEffect, useRef } from "react";

/**
 * MOSS 等离子流体背景（技术书 §9.5.5 C · 背景方案「等离子流体」）
 *
 * - WebGL 片元着色器：多层域扭曲噪声，紫 / 赤红 / 蓝等离子缓慢流动，随鼠标轻微偏移。
 * - 叠加一层 2D 星点（闪烁），以及静态暗角与细噪点。
 * - 性能：着色器以 50% 分辨率渲染后由 CSS 放大；限帧约 40 fps；页面隐藏时暂停；
 *   启动 2 秒内自测帧率，低于 28 fps 自动降到 33% 分辨率并减少星点。
 * - WebGL 不可用时退回 CSS 静态渐变（不报错、不影响业务）。
 * - prefers-reduced-motion：只绘制一帧静态画面。
 * - 纯装饰：aria-hidden、pointer-events: none，不参与任何业务逻辑。
 */

const VERTEX_SHADER = "attribute vec2 a;void main(){gl_Position=vec4(a,0.,1.);}";

const FRAGMENT_SHADER = `precision mediump float;
uniform vec2 r;uniform float t;uniform vec2 m;
float h(vec2 p){return fract(sin(dot(p,vec2(127.1,311.7)))*43758.5453);}
float n(vec2 p){vec2 i=floor(p),f=fract(p);f=f*f*(3.-2.*f);
  return mix(mix(h(i),h(i+vec2(1,0)),f.x),mix(h(i+vec2(0,1)),h(i+vec2(1,1)),f.x),f.y);}
float fbm(vec2 p){float v=0.,a=.5;mat2 R=mat2(.8,.6,-.6,.8);
  for(int i=0;i<5;i++){v+=a*n(p);p=R*p*2.02;a*=.5;}return v;}
void main(){
  vec2 p=(gl_FragCoord.xy-.5*r)/r.y;p+=m*.08;
  vec2 q=vec2(fbm(p*1.6+t*.04),fbm(p*1.6+vec2(5.2,1.3)-t*.035));
  vec2 w=vec2(fbm(p*1.6+3.8*q+vec2(1.7,9.2)+t*.07),fbm(p*1.6+3.8*q+vec2(8.3,2.8)-t*.05));
  float f=fbm(p*1.6+4.*w);
  vec3 c=mix(vec3(.02,.015,.05),vec3(.33,.22,.95),clamp(f*f*2.2,0.,1.));
  c=mix(c,vec3(1.,.23,.36),clamp(length(q)*.95-.38,0.,1.)*.85);
  c=mix(c,vec3(.23,.45,1.),clamp(w.y*w.y*1.2,0.,1.)*.55);
  c+=pow(clamp(f,0.,1.),5.)*vec3(1.,.82,.92)*1.6;
  c*=smoothstep(1.5,.15,length(p-vec2(.15,.1)))*.78;
  gl_FragColor=vec4(c,1.);
}`;

const FRAME_INTERVAL_MS = 24; // ≈ 40 fps
const PROBE_WINDOW_MS = 2000;
const LOW_FPS_THRESHOLD = 28;

function createPlasma(canvas) {
  const gl = canvas.getContext("webgl", { premultipliedAlpha: false, antialias: false, powerPreference: "low-power" });
  if (!gl) return null;
  const compile = (type, source) => {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    return gl.getShaderParameter(shader, gl.COMPILE_STATUS) ? shader : null;
  };
  const vs = compile(gl.VERTEX_SHADER, VERTEX_SHADER);
  const fs = compile(gl.FRAGMENT_SHADER, FRAGMENT_SHADER);
  if (!vs || !fs) return null;
  const program = gl.createProgram();
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) return null;
  gl.useProgram(program);
  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const loc = gl.getAttribLocation(program, "a");
  gl.enableVertexAttribArray(loc);
  gl.vertexAttribPointer(loc, 2, gl.FLOAT, false, 0, 0);
  return {
    gl,
    uniforms: {
      r: gl.getUniformLocation(program, "r"),
      t: gl.getUniformLocation(program, "t"),
      m: gl.getUniformLocation(program, "m"),
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
    let isVisible = !document.hidden;
    let lowPower = false;
    let lastFrame = 0;
    let probeStart = 0;
    let probeFrames = 0;
    let dpr = 1;
    let W = 0;
    let H = 0;
    let stars = [];
    const mouse = { x: 0, y: 0 };
    const smooth = { x: 0, y: 0 };

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
      dpr = Math.min(window.devicePixelRatio || 1, 1.25);
      W = starCanvas.width = Math.round(window.innerWidth * dpr);
      H = starCanvas.height = Math.round(window.innerHeight * dpr);
      if (plasma) {
        const scale = lowPower ? 0.33 : 0.5;
        plasmaCanvas.width = Math.max(1, Math.round(window.innerWidth * scale));
        plasmaCanvas.height = Math.max(1, Math.round(window.innerHeight * scale));
        plasma.gl.viewport(0, 0, plasmaCanvas.width, plasmaCanvas.height);
      }
      buildStars();
    };

    const drawFrame = (time) => {
      smooth.x += (mouse.x - smooth.x) * 0.04;
      smooth.y += (mouse.y - smooth.y) * 0.04;
      if (plasma) {
        const { gl, uniforms } = plasma;
        gl.uniform2f(uniforms.r, plasmaCanvas.width, plasmaCanvas.height);
        gl.uniform1f(uniforms.t, time / 1000);
        gl.uniform2f(uniforms.m, smooth.x, -smooth.y);
        gl.drawArrays(gl.TRIANGLES, 0, 3);
      }
      if (ctx) {
        ctx.clearRect(0, 0, W, H);
        for (const s of stars) {
          if (!reduceMotion) {
            s.x -= 0.01 * s.depth * dpr;
            if (s.x < -5) s.x = W + 5;
          }
          const alpha = 0.3 + 0.7 * Math.abs(Math.sin(s.phase + (time / 1000) * s.speed));
          const x = s.x - smooth.x * 40 * s.depth * dpr;
          const y = s.y - smooth.y * 30 * s.depth * dpr;
          ctx.fillStyle = `rgba(${s.color},${alpha})`;
          ctx.beginPath();
          ctx.arc(x, y, s.r, 0, Math.PI * 2);
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
      if (time - lastFrame >= FRAME_INTERVAL_MS) {
        lastFrame = time;
        drawFrame(time);
      }
      animId = requestAnimationFrame(loop);
    };

    const onPointerMove = (e) => {
      mouse.x = e.clientX / window.innerWidth - 0.5;
      mouse.y = e.clientY / window.innerHeight - 0.5;
    };
    const onVisibility = () => {
      isVisible = !document.hidden;
      if (!isVisible && probeStart > 0) {
        // 隐藏期间不计入帧率自检，回到前台后重新测
        probeStart = 0;
        probeFrames = 0;
      }
      if (isVisible && !reduceMotion && !animId) animId = requestAnimationFrame(loop);
    };
    const onResize = () => {
      resize();
      if (reduceMotion) drawFrame(0);
    };

    resize();
    window.addEventListener("resize", onResize);
    document.addEventListener("visibilitychange", onVisibility);
    if (reduceMotion) {
      drawFrame(0);
    } else {
      window.addEventListener("pointermove", onPointerMove, { passive: true });
      animId = requestAnimationFrame(loop);
    }

    return () => {
      if (animId) cancelAnimationFrame(animId);
      window.removeEventListener("resize", onResize);
      window.removeEventListener("pointermove", onPointerMove);
      document.removeEventListener("visibilitychange", onVisibility);
      const lose = plasma?.gl.getExtension("WEBGL_lose_context");
      if (lose) lose.loseContext();
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
