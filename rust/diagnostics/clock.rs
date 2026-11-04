#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SourceTimestamp {
    pub unix_100ns: i64,
    pub uncertainty_100ns: u64,
}

/// Map a past QPC observation to UTC, bracketing the UTC read with two QPC reads.
/// This bound covers only the local pairing interval, not cross-host clock error.
pub(crate) fn source_timestamp(
    presented: i64,
    frequency: i64,
    before: i64,
    after: i64,
    utc_100ns: i64,
) -> Option<SourceTimestamp> {
    if frequency <= 0 || before < 0 || after < before || presented <= 0 {
        return None;
    }
    let midpoint = before + (after - before) / 2;
    let age = midpoint.checked_sub(presented)?;
    if age < 0 {
        return None;
    }
    let frequency = i128::from(frequency);
    let age_100ns = i64::try_from(i128::from(age) * 10_000_000 / frequency).ok()?;
    // Ceiling keeps a sub-tick pairing interval from being rounded down to zero.
    let uncertainty =
        (i128::from(after - before) * 10_000_000 + 2 * frequency - 1) / (2 * frequency);
    Some(SourceTimestamp {
        unix_100ns: utc_100ns.checked_sub(age_100ns)?,
        uncertainty_100ns: u64::try_from(uncertainty).ok()?,
    })
}

#[cfg(windows)]
pub(crate) fn utc_timecode_100ns() -> i64 {
    let time =
        unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
    let ticks = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    ticks.saturating_sub(116_444_736_000_000_000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_past_desktop_updates_and_bounds_pairing_precision() {
        assert_eq!(
            source_timestamp(80, 1_000, 98, 102, 1_000_000),
            Some(SourceTimestamp {
                unix_100ns: 800_000,
                uncertainty_100ns: 20_000
            })
        );
        assert_eq!(
            source_timestamp(1, 30_000_000, 2, 3, 100)
                .unwrap()
                .uncertainty_100ns,
            1
        );
    }

    #[test]
    fn invalid_clocks_and_unrepresentable_ages_are_not_latency_samples() {
        for (presented, frequency, before, after) in [
            (0, 1, 1, 2),
            (3, 1, 1, 2),
            (1, 0, 1, 2),
            (1, 1, -1, 2),
            (1, 1, 3, 2),
            (1, 1, i64::MAX, i64::MAX),
        ] {
            assert!(source_timestamp(presented, frequency, before, after, 100).is_none());
        }
    }
}
