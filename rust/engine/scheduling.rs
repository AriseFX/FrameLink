//! Keep capture deadlines responsive when ordinary CPU work is competing.
use std::{marker::PhantomData, rc::Rc};
use windows::Win32::System::Threading::{
    GetCurrentThread, GetThreadPriority, SetThreadPriority, THREAD_PRIORITY,
    THREAD_PRIORITY_ABOVE_NORMAL,
};

/// The guard belongs to the worker that created it. Process and other thread
/// priorities are unchanged; a failed adjustment leaves normal sending usable.
pub(super) struct CapturePriority {
    previous: Option<THREAD_PRIORITY>,
    _thread_bound: PhantomData<Rc<()>>,
}

fn current_priority() -> Option<i32> {
    let value = unsafe { GetThreadPriority(GetCurrentThread()) };
    // Win32 defines THREAD_PRIORITY_ERROR_RETURN as 0x7fffffff.
    (value != i32::MAX).then_some(value)
}

impl CapturePriority {
    pub(super) fn new() -> Self {
        let previous = current_priority()
            .filter(|&value| value < THREAD_PRIORITY_ABOVE_NORMAL.0)
            .filter(|_| unsafe {
                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL).is_ok()
            })
            .map(THREAD_PRIORITY);
        Self {
            previous,
            _thread_bound: PhantomData,
        }
    }

    pub(super) fn level(&self) -> Option<i32> {
        current_priority()
    }
}

impl Drop for CapturePriority {
    fn drop(&mut self) {
        if let Some(previous) = self.previous {
            let _ = unsafe { SetThreadPriority(GetCurrentThread(), previous) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_NORMAL};

    #[test]
    fn capture_priority_is_scoped_to_its_worker() {
        let parent = current_priority();
        std::thread::spawn(|| {
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_NORMAL) }.unwrap();
            {
                let priority = CapturePriority::new();
                assert_eq!(priority.level(), Some(THREAD_PRIORITY_ABOVE_NORMAL.0));
            }
            assert_eq!(current_priority(), Some(THREAD_PRIORITY_NORMAL.0));
            unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) }.unwrap();
            {
                let priority = CapturePriority::new();
                assert_eq!(priority.level(), Some(THREAD_PRIORITY_HIGHEST.0));
            }
            assert_eq!(current_priority(), Some(THREAD_PRIORITY_HIGHEST.0));
        })
        .join()
        .unwrap();
        assert_eq!(current_priority(), parent);
    }
}
