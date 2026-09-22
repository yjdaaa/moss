import React, { useEffect, useRef } from "react";

/**
 * MOSS 550W 太空舱全息高能视觉背景系统 3.0
 * 流浪地球科幻工业质感 × 550W量子核心全息罗盘 × 3D激光地平线网格 × 高能粒子电弧
 * 彻底解决“背景太淡”的问题，提供强烈的深空光影对比、高辨识度红蓝激光场与电影级战术工业UI质感
 */
export function CyberBackground() {
  const canvasRef = useRef(null);
  const glowRef = useRef(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let animId = null;
    let particles = [];
    let mouse = { x: -9999, y: -9999 };
    let smoothMouse = { x: -9999, y: -9999 };
    let W = 0;
    let H = 0;
    let isVisible = true;
    let rotationAngle = 0;

    // 自适应重设分辨率
    const handleResize = () => {
      W = canvas.width = window.innerWidth;
      H = canvas.height = window.innerHeight;
      // 根据屏幕面积计算粒子密度，上限 100 颗以保障平滑 60fps
      const count = Math.min(100, Math.max(40, Math.floor((W * H) / 16000)));
      particles = Array.from({ length: count }, () => ({
        x: Math.random() * W,
        y: Math.random() * H,
        z: Math.random() * 0.8 + 0.2, // 空间深度
        r: Math.random() * 2.0 + 0.8,
        vx: (Math.random() - 0.5) * 0.35,
        vy: (Math.random() - 0.5) * 0.35,
        // 三种核心配色：0=高能激光青(195), 1=MOSS量子赤红(355), 2=聚变琥珀金(38)
        colorType: Math.random() > 0.6 ? 1 : (Math.random() > 0.4 ? 0 : 2),
        phase: Math.random() * Math.PI * 2,
        phaseSpeed: Math.random() * 0.02 + 0.01,
      }));
    };

    handleResize();
    window.addEventListener("resize", handleResize);

    const handlePointerMove = (e) => {
      mouse.x = e.clientX;
      mouse.y = e.clientY;
      if (smoothMouse.x < 0) {
        smoothMouse.x = e.clientX;
        smoothMouse.y = e.clientY;
      }
    };

    window.addEventListener("pointermove", handlePointerMove, { passive: true });

    const handleVisibility = () => {
      isVisible = !document.hidden;
      if (isVisible && !animId) {
        animId = requestAnimationFrame(render);
      }
    };
    document.addEventListener("visibilitychange", handleVisibility);

    // 主渲染循环
    const render = (time) => {
      if (!isVisible) {
        animId = null;
        return;
      }

      ctx.clearRect(0, 0, W, H);
      rotationAngle += 0.0025;

      // 平滑光标插值
      smoothMouse.x += (mouse.x - smoothMouse.x) * 0.14;
      smoothMouse.y += (mouse.y - smoothMouse.y) * 0.14;

      if (glowRef.current && smoothMouse.x >= 0) {
        glowRef.current.style.transform = `translate3d(${smoothMouse.x - 300}px, ${smoothMouse.y - 300}px, 0)`;
      }

      // 1. 绘制流浪地球 MOSS 550W 深空全息能量罗盘（背景右上角天体级星环）
      const mossCenterX = W * 0.78;
      const mossCenterY = H * 0.24;
      const mossRadius = Math.min(W, H) * 0.38;

      ctx.save();
      ctx.translate(mossCenterX, mossCenterY);

      // (a) 核心红巨星量子呼吸光晕
      const corePulse = 1.0 + 0.06 * Math.sin(time * 0.002);
      const radGrad = ctx.createRadialGradient(0, 0, 10, 0, 0, mossRadius * 1.3);
      radGrad.addColorStop(0, "rgba(255, 35, 55, 0.28)");
      radGrad.addColorStop(0.35, "rgba(255, 20, 40, 0.12)");
      radGrad.addColorStop(0.7, "rgba(180, 10, 30, 0.04)");
      radGrad.addColorStop(1, "rgba(0, 0, 0, 0)");
      ctx.fillStyle = radGrad;
      ctx.beginPath();
      ctx.arc(0, 0, mossRadius * 1.3, 0, Math.PI * 2);
      ctx.fill();

      // (b) 旋转的外层同心刻度环 1（顺时针）
      ctx.save();
      ctx.rotate(rotationAngle);
      ctx.strokeStyle = "rgba(255, 50, 65, 0.22)";
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.arc(0, 0, mossRadius * 0.88, 0, Math.PI * 2);
      ctx.stroke();

      // 刻度齿轮标记
      ctx.fillStyle = "rgba(255, 80, 95, 0.35)";
      for (let a = 0; a < Math.PI * 2; a += Math.PI / 12) {
        const x1 = Math.cos(a) * (mossRadius * 0.88);
        const y1 = Math.sin(a) * (mossRadius * 0.88);
        const x2 = Math.cos(a) * (mossRadius * 0.93);
        const y2 = Math.sin(a) * (mossRadius * 0.93);
        ctx.beginPath();
        ctx.moveTo(x1, y1);
        ctx.lineTo(x2, y2);
        ctx.stroke();
      }
      ctx.restore();

      // (c) 逆向旋转的量子轨道环 2（逆时针）
      ctx.save();
      ctx.rotate(-rotationAngle * 1.6);
      ctx.strokeStyle = "rgba(0, 229, 255, 0.20)";
      ctx.setLineDash([8, 14]);
      ctx.lineWidth = 1.2;
      ctx.beginPath();
      ctx.arc(0, 0, mossRadius * 0.65, 0, Math.PI * 2);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.restore();

      // (d) 核心 MOSS 红眼内环
      ctx.strokeStyle = "rgba(255, 45, 60, 0.55)";
      ctx.lineWidth = 2.0;
      ctx.shadowColor = "rgba(255, 30, 50, 0.8)";
      ctx.shadowBlur = 16;
      ctx.beginPath();
      ctx.arc(0, 0, mossRadius * 0.32 * corePulse, 0, Math.PI * 2);
      ctx.stroke();
      ctx.shadowBlur = 0;

      // 核心中心高能聚光点
      ctx.fillStyle = "rgba(255, 230, 235, 0.65)";
      ctx.beginPath();
      ctx.arc(0, 0, 4, 0, Math.PI * 2);
      ctx.fill();

      // 全息十字准星线条
      ctx.strokeStyle = "rgba(255, 60, 75, 0.28)";
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(-mossRadius * 0.45, 0);
      ctx.lineTo(mossRadius * 0.45, 0);
      ctx.moveTo(0, -mossRadius * 0.45);
      ctx.lineTo(0, mossRadius * 0.45);
      ctx.stroke();

      ctx.restore();

      // 2. 粒子与激光能量连线系统
      for (let i = 0; i < particles.length; i++) {
        const p = particles[i];

        p.x += p.vx * p.z;
        p.y += p.vy * p.z;

        if (p.x < -20) p.x = W + 20;
        else if (p.x > W + 20) p.x = -20;
        if (p.y < -20) p.y = H + 20;
        else if (p.y > H + 20) p.y = -20;

        const dx = p.x - smoothMouse.x;
        const dy = p.y - smoothMouse.y;
        const distSq = dx * dx + dy * dy;

        let alpha = 0.35 + 0.35 * Math.sin(time * 0.002 + p.phase);
        if (distSq < 45000) {
          alpha += (1 - distSq / 45000) * 0.55;
        }

        let colorRgb = "0, 229, 255"; // 激光青
        if (p.colorType === 1) colorRgb = "255, 45, 65"; // MOSS红
        else if (p.colorType === 2) colorRgb = "255, 185, 60"; // 聚变金

        ctx.beginPath();
        ctx.arc(p.x, p.y, p.r * p.z, 0, Math.PI * 2);
        ctx.fillStyle = `rgba(${colorRgb}, ${Math.min(0.95, alpha)})`;
        ctx.shadowColor = `rgba(${colorRgb}, 0.7)`;
        ctx.shadowBlur = 6 * p.z;
        ctx.fill();
        ctx.shadowBlur = 0;

        // 鼠标附近产生能量引力电弧
        if (distSq < 36000) {
          const ratio = 1 - distSq / 36000;
          ctx.beginPath();
          ctx.moveTo(p.x, p.y);
          ctx.lineTo(smoothMouse.x, smoothMouse.y);
          ctx.strokeStyle = `rgba(${colorRgb}, ${ratio * 0.45})`;
          ctx.lineWidth = 1.0 * ratio;
          ctx.stroke();
        }
      }

      animId = requestAnimationFrame(render);
    };

    animId = requestAnimationFrame(render);

    return () => {
      if (animId) cancelAnimationFrame(animId);
      window.removeEventListener("resize", handleResize);
      window.removeEventListener("pointermove", handlePointerMove);
      document.removeEventListener("visibilitychange", handleVisibility);
    };
  }, []);

  return (
    <div className="cyber-background-container" aria-hidden="true">
      {/* 1. 强化版深空工业背景底色 + 高对比冷暖激光能流 */}
      <div className="cyber-bg-atmosphere" />

      {/* 2. 交互式动态量子罗盘与高能粒子画布 */}
      <canvas ref={canvasRef} className="cyber-bg-canvas" />

      {/* 3. 强化版 3D 透视地平线高能激光光栅 */}
      <div className="cyber-bg-grid">
        <div className="cyber-grid-scanline" />
      </div>

      {/* 4. 跟随鼠标的聚变能量光斑 */}
      <div ref={glowRef} className="cyber-cursor-glow" />

      {/* 5. 战术工业角标与军规刻度水印 (Tactical HUD Elements) */}
      <div className="tactical-hud-overlay">
        <div className="hud-mark top-left">
          <span className="mark-badge">UEG · 550W</span>
          <span className="mark-sub">QUANTUM CORE // ONLINE</span>
        </div>
        <div className="hud-mark top-right">
          <span className="mark-badge">LASER WORKBENCH</span>
          <span className="mark-sub">SYS.PROTOCOL v2.4</span>
        </div>
        <div className="hud-mark bottom-left">
          <span className="mark-coord">GRID 1200×800mm · DUAL GALVO</span>
        </div>
        <div className="hud-mark bottom-right">
          <span className="mark-coord">STATUS: HARDWARE STANDBY // 0.00ms</span>
        </div>
      </div>

      {/* 6. 太空舱金属微噪点滤镜 */}
      <div className="cyber-noise-overlay" />
    </div>
  );
}
