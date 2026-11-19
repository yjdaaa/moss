//! 子进程所在的 Job Object 句柄（Windows）：句柄关闭（含 MOSS 异常退出）时结束整个进程树，
//! 不留孤儿进程。供 `moss-voice`（OneJev、本地视觉）、`moss-voice-providers`（`moss-voice-local`、`speech.cpp`）
//! 与 `moss-voice-host`（Laya）统一共用。

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    /// 子进程所在的 Job Object；句柄关闭（含 MOSS 异常退出）时结束整个进程树。
    pub struct JobHandle(HANDLE);

    // 句柄由单一拥有者持有或在互斥锁保护下访问；Win32 Job Object 句柄本身支持跨线程操作。
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl JobHandle {
        /// 为已启动子进程创建启用 `KILL_ON_JOB_CLOSE` 的 Job Object 并绑定该进程。
        pub fn for_child(child: &std::process::Child) -> Option<Self> {
            use std::os::windows::io::AsRawHandle;
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job == 0 as HANDLE {
                    return None;
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if configured == 0
                    || AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) == 0
                {
                    CloseHandle(job);
                    return None;
                }
                Some(Self(job))
            }
        }

        /// 主动结束 Job Object 中的整个进程树。
        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    /// 非 Windows 平台没有 Job Object；停止时由调用方直接结束子进程。
    pub struct JobHandle;

    impl JobHandle {
        pub fn for_child(_child: &std::process::Child) -> Option<Self> {
            None
        }

        pub fn terminate(&self) {}
    }
}

pub use imp::JobHandle;

#[cfg(test)]
mod tests {
    use super::JobHandle;

    #[test]
    fn job_handle_type_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<JobHandle>();
    }
}
