import React from "react";

/**
 * Prevent a voice plug-in/card render error from unmounting the entire desktop
 * shell. Do not expose raw exceptions here: they can contain user settings.
 */
export class VoiceErrorBoundary extends React.Component {
  constructor(props) {
    super(props);
    this.state = { failed: false };
  }

  static getDerivedStateFromError() {
    return { failed: true };
  }

  componentDidCatch(error) {
    // Log only the error class, never raw message/stack/voice credentials.
    console.error("语音中心渲染异常，已隔离该页面：", error?.name || "RenderError");
  }

  render() {
    if (!this.state.failed) return this.props.children;
    return (
      <div className="ob-page voice-page">
        <section className="ob-card" id="voice-render-error" role="alert">
          <div className="ob-card-h"><h2>语音中心暂时无法显示</h2></div>
          <p>语音中心的某个模块发生了页面异常。其他工作台页面仍可正常使用；这不代表设备已停止。</p>
          <div className="voice-actions">
            <button type="button" className="btn btn-primary" onClick={() => this.setState({ failed: false })}>重试语音中心</button>
            <button type="button" className="btn btn-ghost" onClick={() => this.props.onBack?.("/")}>返回工作台</button>
          </div>
        </section>
      </div>
    );
  }
}
