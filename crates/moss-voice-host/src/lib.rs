//! MOSS 内置语音与长期记忆宿主编排层（零 Tauri / UI 依赖）。
//!
//! 聚合 `laser-host`、`moss-voice*`、`moss-home`、`moss-agent` 与 `moss-usb-provision`，
//! 供桌面壳 `apps/desktop/src-tauri`（及未来无界面宿主）通过类型化方法直接调用。

mod builtin;
pub mod context;

pub use builtin::{BuiltinState, BuiltinVoice, autostart, init, parse_sections, record_moss_event};
pub use moss_home::home_backup_audit;

fn fallback_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("moss-voice-host-async")
            .enable_all()
            .build()
            .expect("moss-voice-host tokio runtime")
    })
}

/// 在同步调用上下文中驱动短生命周期异步任务：
/// - 禁止在 `moss-voice-server` 的异步工作线程内调用。调试构建按专用线程名检查，该名称也覆盖服务自己的阻塞池；桌面命令使用桌面运行时的阻塞池。
/// - 若当前线程处于其他多线程 Tokio 运行时，经 `block_in_place` 复用当前运行时句柄；
/// - 若处于单线程 Tokio 运行时（`CurrentThread`），在作用域线程中转交共享多线程运行时执行，避免重入 panic 或 `tokio::spawn` 死锁；
/// - 否则（普通 OS 线程 / 独立测试）直接使用共享轻量多线程运行时。
pub(crate) fn block_on<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        debug_assert_ne!(
            std::thread::current().name(),
            Some(moss_voice_server::VOICE_SERVER_RUNTIME_THREAD_NAME),
            "语音宿主 block_on 禁止在内置语音服务运行时线程内调用"
        );
        return match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(future))
            }
            _ => std::thread::scope(|scope| {
                match scope
                    .spawn(move || fallback_runtime().block_on(future))
                    .join()
                {
                    Ok(output) => output,
                    Err(payload) => std::panic::resume_unwind(payload),
                }
            }),
        };
    }
    fallback_runtime().block_on(future)
}

#[cfg(test)]
mod runtime_tests {
    use super::block_on;

    #[test]
    fn block_on_works_on_plain_os_thread() {
        let value = block_on(async { tokio::spawn(async { 42usize }).await.unwrap() });
        assert_eq!(value, 42);
    }

    #[test]
    fn block_on_works_inside_multi_thread_tokio_worker_and_blocking_pool() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let from_worker = rt.block_on(rt.spawn(async {
            block_on(async { tokio::spawn(async { "worker-ok" }).await.unwrap() })
        }));
        let from_worker = from_worker.unwrap();
        assert_eq!(from_worker, "worker-ok");

        let from_blocking = rt.block_on(async {
            tokio::task::spawn_blocking(|| {
                block_on(async { tokio::spawn(async { "blocking-ok" }).await.unwrap() })
            })
            .await
            .unwrap()
        });
        assert_eq!(from_blocking, "blocking-ok");
    }

    #[cfg(debug_assertions)]
    #[test]
    fn block_on_rejects_voice_server_async_worker() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name(moss_voice_server::VOICE_SERVER_RUNTIME_THREAD_NAME)
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(rt.spawn(async { block_on(async {}) }))
            .unwrap_err();
        assert!(err.is_panic(), "{err}");
    }

    #[test]
    fn block_on_works_inside_current_thread_tokio_without_deadlock() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let value = rt.block_on(async {
            block_on(async { tokio::spawn(async { "current-thread-ok" }).await.unwrap() })
        });
        assert_eq!(value, "current-thread-ok");
    }
}
