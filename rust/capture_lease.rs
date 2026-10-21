//! Ownership of an acquired desktop frame, independent of the DXGI interface.

pub(crate) trait ReleaseFrame {
    type Error;

    fn release(self) -> Result<(), Self::Error>;
}

pub(crate) struct FrameLease<T: ReleaseFrame> {
    frame: Option<T>,
}

impl<T: ReleaseFrame> FrameLease<T> {
    pub(crate) fn new(frame: T) -> Self {
        Self { frame: Some(frame) }
    }

    /// The release attempt consumes ownership even when the API returns an
    /// error. Retrying from Drop could release a different or invalid frame.
    pub(crate) fn release(mut self) -> Result<(), T::Error> {
        self.frame.take().unwrap().release()
    }
}

impl<T: ReleaseFrame> Drop for FrameLease<T> {
    fn drop(&mut self) {
        if let Some(frame) = self.frame.take() {
            let _ = frame.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        panic::{AssertUnwindSafe, catch_unwind},
        rc::Rc,
    };

    struct Frame {
        events: Rc<RefCell<Vec<&'static str>>>,
        result: Result<(), &'static str>,
    }

    impl ReleaseFrame for Frame {
        type Error = &'static str;

        fn release(self) -> Result<(), Self::Error> {
            self.events.borrow_mut().push("release");
            self.result
        }
    }

    fn lease(
        events: &Rc<RefCell<Vec<&'static str>>>,
        result: Result<(), &'static str>,
    ) -> FrameLease<Frame> {
        FrameLease::new(Frame {
            events: events.clone(),
            result,
        })
    }

    #[test]
    fn frame_stays_owned_through_processing_and_releases_before_reacquisition() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut held_frame = Some(lease(&events, Ok(())));
        events
            .borrow_mut()
            .extend(["copy", "map", "submit", "pace"]);
        assert!(!events.borrow().contains(&"release"));

        held_frame.take().unwrap().release().unwrap();
        events.borrow_mut().push("acquire");
        drop(held_frame);
        assert_eq!(
            &*events.borrow(),
            &["copy", "map", "submit", "pace", "release", "acquire"]
        );
    }

    #[test]
    fn failed_explicit_release_is_returned_and_never_retried() {
        let events = Rc::new(RefCell::new(Vec::new()));
        assert_eq!(
            lease(&events, Err("access lost")).release(),
            Err("access lost")
        );
        assert_eq!(&*events.borrow(), &["release"]);
    }

    #[test]
    fn scope_exit_releases_a_retained_frame_once() {
        let events = Rc::new(RefCell::new(Vec::new()));
        {
            let _held_frame = Some(lease(&events, Ok(())));
        }
        assert_eq!(&*events.borrow(), &["release"]);
    }

    #[test]
    fn acquisition_processing_error_releases_without_a_successful_return() {
        fn process(events: &Rc<RefCell<Vec<&'static str>>>) -> Result<(), &'static str> {
            let _frame = lease(events, Ok(()));
            events.borrow_mut().push("resource");
            let resource: Result<(), &'static str> = Err("invalid resource");
            resource?;
            Ok(())
        }

        let events = Rc::new(RefCell::new(Vec::new()));
        assert_eq!(process(&events), Err("invalid resource"));
        assert_eq!(&*events.borrow(), &["resource", "release"]);
    }

    #[test]
    fn unwind_releases_once_even_when_release_returns_an_error() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _held_frame = Some(lease(&events, Err("access lost")));
            panic!("processing panicked");
        }));
        assert!(result.is_err());
        assert_eq!(&*events.borrow(), &["release"]);
    }
}
