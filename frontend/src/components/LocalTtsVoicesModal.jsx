import React, { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { Volume2, Play, Square, Check, X, Download, Sparkles } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { voiceErrorText } from "../voiceState.js";
import { isTopmostModalBackdrop } from "./AppDialog.jsx";

export function LocalTtsVoicesModal({ visible, ...props }) {
  return visible ? <LocalTtsVoicesModalDialog {...props} /> : null;
}

function LocalTtsVoicesModalDialog({
  modelId,
  modelLabel = "",
  currentTtsOn = false,
  currentModelId = "",
  currentSpeaker = null,
  onApplyVoice = null,
  onDownloadModel = null,
  onClose,
}) {
  const [data, setData] = useState(null);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [actionNotice, setActionNotice] = useState(null);
  const [langFilter, setLangFilter] = useState("all");
  const [genderFilter, setGenderFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [loadingSpeaker, setLoadingSpeaker] = useState(null);
  const [playingSpeaker, setPlayingSpeaker] = useState(null);
  const [applyingSpeaker, setApplyingSpeaker] = useState(null);

  const backdropRef = useRef(null);
  const backdropPressRef = useRef(false);
  const mountedRef = useRef(true);
  const audioRef = useRef(null);
  const audioCacheRef = useRef(new Map());
  const pollInFlightRef = useRef(false);

  const stopAudio = useCallback(() => {
    if (audioRef.current) {
      try {
        audioRef.current.pause();
        audioRef.current.src = "";
      } catch {
        // 忽略释放异常
      }
      audioRef.current = null;
    }
    if (mountedRef.current) {
      setPlayingSpeaker(null);
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      stopAudio();
    };
  }, [stopAudio]);

  const fetchCatalog = useCallback(
    async (silent = false) => {
      if (!modelId) return;
      if (silent) {
        if (pollInFlightRef.current) return;
        pollInFlightRef.current = true;
      } else {
        setLoading(true);
        setLoadError("");
      }
      try {
        const res = await voiceApi.localTtsVoices(modelId);
        if (!mountedRef.current) return;
        setData(res);
      } catch (err) {
        if (!mountedRef.current) return;
        if (!silent) {
          setLoadError(voiceErrorText(err));
        }
      } finally {
        if (silent) {
          pollInFlightRef.current = false;
        } else if (mountedRef.current) {
          setLoading(false);
        }
      }
    },
    [modelId]
  );

  useEffect(() => {
    stopAudio();
    setLangFilter("all");
    setGenderFilter("all");
    setQuery("");
    setActionNotice(null);
    fetchCatalog(false);
  }, [modelId, fetchCatalog, stopAudio]);

  // 模型已安装但本机试听尚未全部缓存完成时，轻量轮询刷新缓存进度，满额即停
  useEffect(() => {
    if (!data?.installed) return undefined;
    if ((data.cached_count ?? 0) >= (data.total_count ?? 0)) return undefined;
    const timer = setInterval(() => {
      if (typeof document !== "undefined" && document.hidden) return;
      fetchCatalog(true);
    }, 1500);
    return () => clearInterval(timer);
  }, [data?.installed, data?.cached_count, data?.total_count, fetchCatalog]);

  const modalBusy = applyingSpeaker !== null;

  useEffect(() => {
    const onKey = (e) => {
      if (e.defaultPrevented || e.isComposing || e.keyCode === 229) return;
      if (e.key === "Escape") {
        if (!isTopmostModalBackdrop(backdropRef.current)) return;
        e.preventDefault();
        if (modalBusy) return;
        onClose?.();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [modalBusy, onClose]);

  const handlePlay = async (speakerItem) => {
    const sid = speakerItem.id;
    if (playingSpeaker === sid) {
      stopAudio();
      return;
    }
    stopAudio();
    setActionNotice(null);
    const cacheKey = `${modelId}:${sid}`;
    let dataUrl = audioCacheRef.current.get(cacheKey) || "";
    if (!dataUrl) {
      setLoadingSpeaker(sid);
      try {
        const res = await voiceApi.localTtsPreview(modelId, sid);
        if (!mountedRef.current) return;
        dataUrl = res?.audio_data_url || "";
        if (dataUrl) {
          audioCacheRef.current.set(cacheKey, dataUrl);
          setData((prev) => {
            if (!prev || !Array.isArray(prev.speakers)) return prev;
            let added = 0;
            const nextSpeakers = prev.speakers.map((s) => {
              if (s.id === sid && !s.cached) {
                added = 1;
                return { ...s, cached: true };
              }
              return s;
            });
            return added
              ? { ...prev, cached_count: Math.min(prev.total_count, (prev.cached_count || 0) + added), speakers: nextSpeakers }
              : prev;
          });
        }
      } catch (err) {
        if (!mountedRef.current) return;
        setActionNotice({ ok: false, text: `试听失败：${voiceErrorText(err)}` });
        setLoadingSpeaker(null);
        return;
      } finally {
        if (mountedRef.current) {
          setLoadingSpeaker(null);
        }
      }
    }
    if (!dataUrl || !mountedRef.current) return;
    try {
      const audio = new Audio(dataUrl);
      audioRef.current = audio;
      setPlayingSpeaker(sid);
      audio.onended = () => {
        if (mountedRef.current && audioRef.current === audio) {
          setPlayingSpeaker(null);
        }
      };
      audio.onerror = () => {
        if (mountedRef.current && audioRef.current === audio) {
          setPlayingSpeaker(null);
        }
      };
      await audio.play();
    } catch (err) {
      if (mountedRef.current) {
        setPlayingSpeaker(null);
        setActionNotice({ ok: false, text: `无法播放试听音频：${err?.message || "浏览器阻止了播放"}` });
      }
    }
  };

  const handleApply = async (speakerItem) => {
    if (!onApplyVoice || modalBusy) return;
    setApplyingSpeaker(speakerItem.id);
    setActionNotice(null);
    try {
      const note = await onApplyVoice(modelId, speakerItem.id, speakerItem);
      if (!mountedRef.current) return;
      setActionNotice({
        ok: true,
        text: note || `已切换为 #${speakerItem.id} ${speakerItem.name}（${speakerItem.gender_label}）`,
      });
    } catch (err) {
      if (!mountedRef.current) return;
      setActionNotice({ ok: false, text: `选用音色失败：${voiceErrorText(err)}` });
    } finally {
      if (mountedRef.current) {
        setApplyingSpeaker(null);
      }
    }
  };

  const languages = useMemo(() => (Array.isArray(data?.languages) ? data.languages : []), [data]);
  const speakers = useMemo(() => (Array.isArray(data?.speakers) ? data.speakers : []), [data]);

  const filteredSpeakers = useMemo(() => {
    const q = query.trim().toLowerCase();
    return speakers.filter((s) => {
      if (langFilter !== "all" && s.lang !== langFilter) return false;
      if (genderFilter !== "all" && s.gender !== genderFilter) return false;
      if (!q) return true;
      return (
        String(s.id).includes(q) ||
        String(s.name || "").toLowerCase().includes(q) ||
        String(s.code || "").toLowerCase().includes(q) ||
        String(s.lang_label || "").toLowerCase().includes(q)
      );
    });
  }, [speakers, langFilter, genderFilter, query]);

  const installed = Boolean(data?.installed);
  const totalCount = data?.total_count ?? speakers.length;
  const cachedCount = data?.cached_count ?? 0;
  const cachePercent = totalCount > 0 ? Math.round((cachedCount / totalCount) * 100) : 0;
  const titleText = data?.label || modelLabel || "语音合成模型音色与语言包";

  const content = (
    <div
      ref={backdropRef}
      className="mat-modal-backdrop tts-voices-backdrop"
      id="voice-local-tts-modal-backdrop"
      onMouseDown={(e) => {
        backdropPressRef.current = e.button === 0 && e.target === e.currentTarget;
      }}
      onMouseUp={(e) => {
        if (e.target !== e.currentTarget) backdropPressRef.current = false;
      }}
      onClick={(e) => {
        if (backdropPressRef.current && e.target === e.currentTarget && !modalBusy) {
          onClose?.();
        }
        backdropPressRef.current = false;
      }}
    >
      <div
        className="mat-modal tts-voices-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="tts-voices-modal-title"
        id="voice-local-tts-modal"
      >
        <div className="mat-modal-head">
          <h3 id="tts-voices-modal-title" className="auth-heading">
            <Volume2 size={18} />
            <span>{titleText}</span>
          </h3>
          <button
            type="button"
            className="mat-modal-close"
            onClick={onClose}
            title="关闭"
            aria-label="关闭"
            disabled={modalBusy}
            id="voice-local-tts-modal-close"
          >
            <X size={16} />
          </button>
        </div>

        <div className="mat-modal-body tts-voices-body">
          {loading ? (
            <p className="voice-muted">正在读取模型语言包与本机试听缓存…</p>
          ) : loadError ? (
            <div className="voice-hint voice-hint--bad">{loadError}</div>
          ) : (
            <>
              {data?.note && <p className="voice-muted tts-voices-lead">{data.note}</p>}

              <div className={`voice-hint ${installed ? (cachedCount >= totalCount ? "voice-hint--ok" : "") : "voice-hint--warn"}`} id="voice-local-tts-cache-banner">
                {installed ? (
                  cachedCount >= totalCount ? (
                    <div className="tts-cache-row">
                      <span>
                        <Sparkles size={13} style={{ verticalAlign: "-2px", marginRight: 5 }} />
                        本机试听已全部自动缓存（<b>{cachedCount} / {totalCount}</b>）· 断网秒播，点「选用」即可直接切换声音
                      </span>
                    </div>
                  ) : (
                    <div className="tts-cache-stack">
                      <div className="tts-cache-row">
                        <span>
                          正在后台自动生成本机试听缓存（<b>{cachedCount} / {totalCount}</b>，优先缓存中文音色；已缓存项可直接秒播，未缓存项点击也会立即合成并缓存）
                        </span>
                        <span className="mono">{cachePercent}%</span>
                      </div>
                      <div className="voice-progress" aria-hidden="true">
                        <span style={{ width: `${cachePercent}%` }} />
                      </div>
                    </div>
                  )
                ) : (
                  <div className="tts-cache-row">
                    <span>该模型尚未下载；下载完成后会自动把全部音色的试听语音缓存到本机磁盘。</span>
                    {onDownloadModel && (
                      <button
                        type="button"
                        className="btn btn-ghost voice-inline-btn"
                        onClick={() => {
                          onDownloadModel(modelId);
                          onClose?.();
                        }}
                        id="voice-local-tts-modal-download"
                      >
                        <Download size={13} /> 下载此模型
                      </button>
                    )}
                  </div>
                )}
              </div>

              <div className="tts-lang-pack-section">
                <div className="tts-section-label">支持的语言包（点击可筛选对应音色）</div>
                <div className="tts-lang-pack-grid" id="voice-local-tts-languages">
                  {languages.map((lang) => {
                    const active = langFilter === lang.code;
                    return (
                      <button
                        key={lang.code}
                        type="button"
                        className={`tts-lang-card${active ? " is-active" : ""}`}
                        onClick={() => setLangFilter(active ? "all" : lang.code)}
                        aria-pressed={active}
                        id={`voice-local-tts-lang-${lang.code}`}
                      >
                        <div className="tts-lang-card-head">
                          <b>{lang.label}</b>
                          <span className={`ob-pill ${lang.recommended ? "ob-pill--ok" : "ob-pill--info"}`}>
                            {lang.count} 个音色{lang.recommended ? " · 推荐" : ""}
                          </span>
                        </div>
                        {lang.note && <div className="ob-sub">{lang.note}</div>}
                        <div className="tts-lang-sample mono">试听语：“{lang.sample_text}”</div>
                      </button>
                    );
                  })}
                </div>
              </div>

              {actionNotice && (
                <div className={`voice-hint ${actionNotice.ok ? "voice-hint--ok" : "voice-hint--bad"}`} id="voice-local-tts-action-notice">
                  {actionNotice.text}
                </div>
              )}

              <div className="tts-filter-bar">
                <div className="tts-filter-pills" role="group" aria-label="语言与性别筛选">
                  <button
                    type="button"
                    className={`btn btn-ghost voice-inline-btn${langFilter === "all" ? " is-selected" : ""}`}
                    onClick={() => setLangFilter("all")}
                  >
                    全部语种 ({speakers.length})
                  </button>
                  <button
                    type="button"
                    className={`btn btn-ghost voice-inline-btn${genderFilter === "all" ? " is-selected" : ""}`}
                    onClick={() => setGenderFilter("all")}
                  >
                    全部性别
                  </button>
                  <button
                    type="button"
                    className={`btn btn-ghost voice-inline-btn${genderFilter === "female" ? " is-selected" : ""}`}
                    onClick={() => setGenderFilter(genderFilter === "female" ? "all" : "female")}
                  >
                    女声
                  </button>
                  <button
                    type="button"
                    className={`btn btn-ghost voice-inline-btn${genderFilter === "male" ? " is-selected" : ""}`}
                    onClick={() => setGenderFilter(genderFilter === "male" ? "all" : "male")}
                  >
                    男声
                  </button>
                </div>
                <input
                  type="search"
                  className="text-input tts-search-input"
                  placeholder="搜索音色编号、名称或代号（如 45 / 晓北 / zf_）"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  id="voice-local-tts-search"
                />
              </div>

              <div className="tts-speaker-list" id="voice-local-tts-speakers">
                {filteredSpeakers.length === 0 ? (
                  <p className="voice-muted">没有匹配的音色，请调整筛选条件。</p>
                ) : (
                  filteredSpeakers.map((sp) => {
                    const isCurrent =
                      currentModelId === modelId &&
                      currentSpeaker !== null &&
                      Number(currentSpeaker) === sp.id;
                    const isActiveInMix = isCurrent && currentTtsOn;
                    const isPlaying = playingSpeaker === sp.id;
                    const isLoadingThis = loadingSpeaker === sp.id;
                    const isApplyingThis = applyingSpeaker === sp.id;
                    return (
                      <div
                        key={sp.id}
                        className={`tts-speaker-row${isActiveInMix ? " is-current" : ""}${isPlaying ? " is-playing" : ""}`}
                        id={`voice-local-tts-row-${sp.id}`}
                      >
                        <div className="tts-speaker-main">
                          <div className="tts-speaker-title">
                            <span className="mono tts-speaker-id">#{sp.id}</span>
                            <b>{sp.name}</b>
                            <span className="mono voice-muted tts-speaker-code">{sp.code}</span>
                            <span className={`ob-pill ${sp.gender === "female" ? "ob-pill--info" : "ob-pill--off"}`}>
                              {sp.gender_label}
                            </span>
                            <span className={`ob-pill ${sp.recommended_for_zh ? "ob-pill--ok" : "ob-pill--off"}`}>
                              {sp.lang_label}
                            </span>
                            {sp.cached && (
                              <span className="ob-pill ob-pill--ok" title="试听语音已缓存在本机磁盘，点击秒播">
                                本机已缓存
                              </span>
                            )}
                            {isActiveInMix ? (
                              <span className="ob-pill ob-pill--ok">当前启用</span>
                            ) : isCurrent ? (
                              <span className="ob-pill ob-pill--info">当前本地默认</span>
                            ) : null}
                          </div>
                          <div className="ob-sub tts-speaker-sample">试听语：“{sp.sample_text}”</div>
                        </div>

                        <div className="tts-speaker-actions">
                          <button
                            type="button"
                            className="btn btn-ghost voice-inline-btn"
                            disabled={!installed || (loadingSpeaker !== null && !isLoadingThis)}
                            onClick={() => handlePlay(sp)}
                            title={installed ? (sp.cached ? "直接播放本机缓存试听" : "合成试听并缓存到本机") : "请先下载该合成模型"}
                            id={`voice-local-tts-play-${sp.id}`}
                          >
                            {isPlaying ? (
                              <>
                                <Square size={13} /> 停止
                              </>
                            ) : isLoadingThis ? (
                              "合成缓存中…"
                            ) : (
                              <>
                                <Play size={13} /> 试听
                              </>
                            )}
                          </button>

                          {onApplyVoice && (
                            <button
                              type="button"
                              className={`btn ${isActiveInMix ? "btn-ghost" : "btn-hud-primary"} voice-inline-btn`}
                              disabled={!installed || modalBusy || isActiveInMix}
                              onClick={() => handleApply(sp)}
                              title={installed ? "将本地语音合成切换为此模型与音色" : "请先下载该合成模型"}
                              id={`voice-local-tts-apply-${sp.id}`}
                            >
                              {isApplyingThis ? (
                                "保存中…"
                              ) : isActiveInMix ? (
                                <>
                                  <Check size={13} /> 使用中
                                </>
                              ) : (
                                "选用此音色"
                              )}
                            </button>
                          )}
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );

  if (typeof document === "undefined") return content;
  return createPortal(content, document.body);
}
