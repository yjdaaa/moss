//! 子进程所在的 Job Object 句柄（Windows）：句柄关闭（含 MOSS 异常退出）时结束整个进程树，
//! 不留孤儿进程。内置语音服务的本地进程（Jev 网关、本地视觉、Laya）共用。
//!
//! 2026-11-17 删除自建服务端链路时从原 `supervisor` 模块移出。

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

    // 句柄只在持有 Mutex 时使用
    unsafe impl Send for JobHandle {}

    impl JobHandle {
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
                    &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION
                        as *const core::ffi::c_void,
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
    /// 非 Windows 平台没有 Job Object；停止时直接结束子进程。
    pub struct JobHandle;

    impl JobHandle {
        pub fn for_child(_child: &std::process::Child) -> Option<Self> {
            None
        }

        pub fn terminate(&self) {}
    }
}

pub use imp::JobHandle;
