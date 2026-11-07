//! Process bookkeeping, so nothing is left running or lying around if Sound Honter is killed.

use std::os::windows::io::AsRawHandle;
use std::process::Child;
use std::ptr::null;
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

const STILL_ACTIVE: u32 = 259;

/// A job whose processes Windows ends when Sound Honter exits, however it exits. The handle is
/// never closed by hand: closing it at exit is what ends them.
fn job() -> Option<isize> {
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    *JOB.get_or_init(|| unsafe {
        let job = CreateJobObjectW(null(), null());
        if job.is_null() {
            return None;
        }
        // SAFETY: plain data, for which all-zero means "no limits".
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const _,
            std::mem::size_of_val(&limits) as u32,
        );
        if ok == 0 {
            CloseHandle(job);
            return None;
        }
        Some(job as isize)
    })
}

/// Makes Windows end `child` if Sound Honter exits first. Best effort: if it fails, the child
/// just runs to completion on its own, as it would have anyway.
pub fn end_with_us(child: &Child) {
    if let Some(job) = job() {
        // SAFETY: both handles are valid for the duration of the call.
        unsafe { AssignProcessToJobObject(job as _, child.as_raw_handle() as _) };
    }
}

/// Whether a process with this id is still running. Unsure counts as running, so callers only
/// clean up after processes that are certainly gone.
pub fn is_running(pid: u32) -> bool {
    // SAFETY: the handle is checked before use and closed after.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code = 0;
        let ok = GetExitCodeProcess(process, &mut code);
        CloseHandle(process);
        ok == 0 || code == STILL_ACTIVE
    }
}
