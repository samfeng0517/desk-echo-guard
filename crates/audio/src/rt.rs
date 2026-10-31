//! Real-time scheduling for the audio threads (Windows; no-ops elsewhere).
//!
//! While the window is in the foreground Windows boosts the whole process,
//! which hides how the audio threads are scheduled.  Once it is minimized
//! or hidden to the tray that boost is gone, and Windows 10 / 11 power
//! throttling (EcoQoS) may also slow a process without a visible window
//! down.  Every thread that moves or processes audio therefore joins MMCSS
//! "Pro Audio" (the device threads and the pipeline threads alike), and
//! the process opts out of power throttling while the pipeline runs.

/// MMCSS "Pro Audio" for the current thread; reverted on drop.  If MMCSS
/// is unavailable (service disabled) the thread gets
/// `THREAD_PRIORITY_HIGHEST` instead, also reverted on drop.
#[must_use = "the thread leaves real-time scheduling when this is dropped"]
pub struct RealtimeThread {
    _guard: imp::Guard,
}

/// Put the current thread into real-time audio scheduling until the
/// returned guard is dropped (on the same thread).
pub fn realtime_thread() -> RealtimeThread {
    RealtimeThread {
        _guard: imp::Guard::join(),
    }
}

/// Ask Windows not to power-throttle (EcoQoS) this process, even when it
/// has no visible window, and to keep honouring its timer resolution when
/// minimized (Windows 11).  Harmless to call more than once; best effort.
pub fn disable_power_throttling() {
    imp::disable_power_throttling()
}

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{
        AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, GetCurrentProcess,
        GetCurrentThread, ProcessPowerThrottling, SetProcessInformation, SetThreadPriority,
        PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE,
        THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_NORMAL,
    };

    pub enum Guard {
        Mmcss(HANDLE),
        Highest,
        Nothing,
    }

    impl Guard {
        pub fn join() -> Self {
            let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
            let mut index = 0u32;
            // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives
            // the call; `index` is a valid out pointer.  Failure returns null.
            let h = unsafe { AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut index) };
            if !h.is_null() {
                return Guard::Mmcss(h);
            }
            // SAFETY: pseudo handle of the calling thread.
            if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) } != 0 {
                Guard::Highest
            } else {
                Guard::Nothing
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            match *self {
                // SAFETY: handle returned by AvSetMmThreadCharacteristicsW
                // on this thread (the guard is !Send), reverted once.
                Guard::Mmcss(h) => unsafe {
                    AvRevertMmThreadCharacteristics(h);
                },
                // SAFETY: pseudo handle of the calling thread.
                Guard::Highest => unsafe {
                    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_NORMAL);
                },
                Guard::Nothing => {}
            }
        }
    }

    fn set_throttling(control: u32) -> bool {
        let state = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: control,
            StateMask: 0, // controlled bits off = never throttle
        };
        // SAFETY: pseudo handle of this process; `state` is a valid
        // PROCESS_POWER_THROTTLING_STATE of the size passed.
        unsafe {
            SetProcessInformation(
                GetCurrentProcess(),
                ProcessPowerThrottling,
                (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
                std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            ) != 0
        }
    }

    pub fn disable_power_throttling() {
        // Each call replaces the whole state.  The timer flag is Windows 11
        // only and Windows 10 rejects the call with it: retry without.
        if !set_throttling(
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
                | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
        ) {
            let _ = set_throttling(PROCESS_POWER_THROTTLING_EXECUTION_SPEED);
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub struct Guard;

    impl Guard {
        pub fn join() -> Self {
            Guard
        }
    }

    pub fn disable_power_throttling() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realtime_thread_joins_and_reverts() {
        disable_power_throttling();
        disable_power_throttling(); // twice is fine
        std::thread::spawn(|| {
            let g = realtime_thread();
            drop(g);
            let _again = realtime_thread();
        })
        .join()
        .unwrap();
    }
}
