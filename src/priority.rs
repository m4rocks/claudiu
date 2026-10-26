//! Windows only: run the `claude` processes at above normal priority, so a heavy build in one session cannot slow
//! the sessions down. Only `claude` itself is raised. Windows gives a child the normal class unless its parent is
//! below normal, so bash, cargo and rustc start at normal priority and stay as they are.

/// Raise the given `claude` processes to above normal priority. A process that already runs at another class was set
/// by the user or another tool and is left alone.
#[cfg(windows)]
pub fn raise_claude_priority(claude_pids: &[u32]) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        ABOVE_NORMAL_PRIORITY_CLASS, GetPriorityClass, NORMAL_PRIORITY_CLASS, OpenProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION, SetPriorityClass,
    };

    for &pid in claude_pids {
        // SAFETY: the handle is checked for null before use and closed after.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION, 0, pid);
            if handle.is_null() {
                continue;
            }
            if GetPriorityClass(handle) == NORMAL_PRIORITY_CLASS {
                SetPriorityClass(handle, ABOVE_NORMAL_PRIORITY_CLASS);
            }
            CloseHandle(handle);
        }
    }
}

/// Claudiu started from a session of an older version can have inherited a lowered priority; it runs at normal
/// priority whatever started it.
#[cfg(windows)]
pub fn restore_own_priority() {
    use windows_sys::Win32::System::Threading::{
        BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, GetPriorityClass, NORMAL_PRIORITY_CLASS, SetPriorityClass,
    };
    // SAFETY: the pseudo handle of the current process is always valid.
    unsafe {
        let me = GetCurrentProcess();
        if GetPriorityClass(me) == BELOW_NORMAL_PRIORITY_CLASS {
            SetPriorityClass(me, NORMAL_PRIORITY_CLASS);
        }
    }
}
