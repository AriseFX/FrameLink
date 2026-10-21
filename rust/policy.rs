//! Pure scheduling and buffer ownership policy, shared by the Windows worker and tests.
use crate::model::{Monitor, Region};
use std::time::{Duration, Instant};

/// None means no display is presently available, so capture recovery should
/// handle the transient state. As with Windows' virtual desktop rectangle,
/// gaps between displays remain part of its bounding rectangle.
pub fn region_in_desktop(region: Region, monitors: &[Monitor]) -> Option<bool> {
    let first = monitors.first()?;
    let mut left = i64::from(first.x);
    let mut top = i64::from(first.y);
    let mut right = left + i64::from(first.width);
    let mut bottom = top + i64::from(first.height);
    for monitor in &monitors[1..] {
        left = left.min(i64::from(monitor.x));
        top = top.min(i64::from(monitor.y));
        right = right.max(i64::from(monitor.x) + i64::from(monitor.width));
        bottom = bottom.max(i64::from(monitor.y) + i64::from(monitor.height));
    }
    Some(
        i64::from(region.x) >= left
            && i64::from(region.y) >= top
            && i64::from(region.x) + i64::from(region.width) <= right
            && i64::from(region.y) + i64::from(region.height) <= bottom,
    )
}

/// Late frames resume immediately instead of skipping an additional frame interval.
pub fn next_frame_deadline(previous: Instant, interval: Duration, now: Instant) -> Instant {
    (previous + interval).max(now)
}

pub fn timer_delay(remaining: Duration, precision: bool) -> Duration {
    remaining.saturating_sub(if precision {
        Duration::from_micros(500)
    } else {
        Duration::ZERO
    })
}

#[derive(Debug, Default)]
pub struct CaptureRetry {
    deadline: Option<Instant>,
}
impl CaptureRetry {
    pub fn failed(&mut self, now: Instant) {
        self.deadline = Some(now + Duration::from_secs(1));
    }
    pub fn recovered(&mut self) {
        self.deadline = None;
    }
    pub fn pending(&self) -> bool {
        self.deadline.is_some()
    }
    pub fn due(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|d| now >= d)
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlotPlan {
    pub slot: usize,
    pub refresh: bool,
}

/// NDI holds the submitted pixel memory until the *next* asynchronous call returns.
/// Changed images can only be written into the other slot. Repeats reuse the
/// current pixels without writing to the memory still owned by NDI.
#[derive(Debug, Default)]
pub struct LatestFrameSlots {
    current: Option<usize>,
}
impl LatestFrameSlots {
    pub fn prepare(&self, changed: bool) -> SlotPlan {
        let refresh = changed || self.current.is_none();
        SlotPlan {
            slot: match self.current {
                Some(i) if refresh => i ^ 1,
                Some(i) => i,
                None => 0,
            },
            refresh,
        }
    }
    pub fn submitted(&mut self, slot: usize) {
        debug_assert!(slot < 2);
        self.current = Some(slot);
    }
}

pub fn temporary_dxgi_failure(result: u32) -> bool {
    matches!(
        result,
        0x887a0005 | 0x887a0007 | 0x887a0022 | 0x887a0026 | 0x887a0028 | 0x887a0025 | 0x80070005
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn late_work_does_not_lose_an_extra_frame() {
        let start = Instant::now();
        let interval = Duration::from_nanos(1_000_000_000 / 240);
        let late = start + interval + Duration::from_micros(50);
        assert_eq!(next_frame_deadline(start, interval, late), late);
        assert_eq!(
            next_frame_deadline(start, interval, start),
            start + interval
        );
    }
    #[test]
    fn only_precision_mode_reserves_at_most_half_a_millisecond() {
        assert_eq!(
            timer_delay(Duration::from_micros(900), false),
            Duration::from_micros(900)
        );
        assert_eq!(
            timer_delay(Duration::from_micros(900), true),
            Duration::from_micros(400)
        );
        assert_eq!(
            timer_delay(Duration::from_micros(400), true),
            Duration::ZERO
        );
    }
    #[test]
    fn repeated_pixels_stay_immutable_until_next_submission_returns() {
        let mut slots = LatestFrameSlots::default();
        let mut in_flight = None;
        for changed in [false, false, true, true, false, false, true] {
            let plan = slots.prepare(changed);
            if plan.refresh {
                assert_ne!(Some(plan.slot), in_flight);
            } else {
                assert_eq!(Some(plan.slot), in_flight);
            }
            // Only a returning NDI call releases the old pointer.
            slots.submitted(plan.slot);
            in_flight = Some(plan.slot);
        }
    }
    #[test]
    fn recovery_is_bounded_and_resets_on_success() {
        let now = Instant::now();
        let mut retry = CaptureRetry::default();
        retry.failed(now);
        assert!(!retry.due(now + Duration::from_millis(999)));
        assert!(retry.due(now + Duration::from_secs(1)));
        retry.recovered();
        assert!(!retry.pending());
        assert!(!retry.due(now + Duration::from_secs(10)));
        assert!(temporary_dxgi_failure(0x887a0026));
        assert!(!temporary_dxgi_failure(0x80070057));
    }
    #[test]
    fn display_bounds_distinguish_detachment_from_temporarily_missing_displays() {
        let monitor = |x, width| Monitor {
            id: String::new(),
            name: String::new(),
            x,
            y: 0,
            width,
            height: 1440,
            refresh_hz: 240,
            primary: x == 0,
        };
        let region = Region {
            x: -1500,
            y: 300,
            width: 640,
            height: 640,
        };
        assert_eq!(region_in_desktop(region, &[]), None);
        assert_eq!(
            region_in_desktop(region, &[monitor(-2560, 2560), monitor(0, 2560)]),
            Some(true)
        );
        assert_eq!(region_in_desktop(region, &[monitor(0, 2560)]), Some(false));
        let crossing = Region {
            x: -200,
            y: 300,
            width: 640,
            height: 640,
        };
        assert_eq!(
            region_in_desktop(crossing, &[monitor(-2560, 2560), monitor(0, 2560)]),
            Some(true)
        );
    }
}
