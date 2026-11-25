//! MMCSS "Pro Audio" scheduling for the render thread.
//!
//! The feeder thread registers itself with the Multimedia Class Scheduler Service
//! under the "Pro Audio" task so Windows gives it glitch-resistant priority
//! (S04 / PLAN §5). The registration is undone when the guard drops.

use windows::core::w;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW,
};

/// RAII guard that keeps the current thread registered with MMCSS "Pro Audio"
/// for its lifetime.
pub struct ProAudioGuard {
    handle: HANDLE,
}

impl ProAudioGuard {
    /// Join the "Pro Audio" MMCSS task on the calling thread.
    ///
    /// Returns `None` if the OS declined (e.g. MMCSS unavailable); the caller then
    /// runs at normal priority rather than failing the stream.
    #[must_use]
    pub fn enter() -> Option<Self> {
        let mut task_index: u32 = 0;
        // SAFETY: `w!("Pro Audio")` is a static, NUL-terminated wide string, and
        // `task_index` is a valid, writable out-param for the duration of the call.
        let result = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
        match result {
            Ok(handle) if !handle.is_invalid() => Some(Self { handle }),
            _ => None,
        }
    }
}

impl Drop for ProAudioGuard {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: `handle` was returned by AvSetMmThreadCharacteristicsW on this
            // thread and has not been reverted yet.
            unsafe {
                let _ = AvRevertMmThreadCharacteristics(self.handle);
            }
        }
    }
}
