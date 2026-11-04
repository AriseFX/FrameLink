//! Reuse the crop only when DXGI metadata proves it was not changed.
use windows::Win32::{
    Foundation::RECT,
    Graphics::Dxgi::{DXGI_OUTDUPL_MOVE_RECT, IDXGIOutputDuplication},
};

// Unexpected metadata sizes or allocation failures fall back to a full crop.
const MAX_METADATA_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct RegionChanges {
    dirty: Vec<RECT>,
    moves: Vec<DXGI_OUTDUPL_MOVE_RECT>,
}

fn overlaps(a: RECT, b: RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

fn valid(rect: RECT) -> bool {
    rect.left >= 0 && rect.top >= 0 && rect.right > rect.left && rect.bottom > rect.top
}

fn prepare<T: Default + Clone>(buffer: &mut Vec<T>, bytes: usize) -> bool {
    let count = bytes.div_ceil(size_of::<T>());
    if buffer.len() < count {
        if buffer.try_reserve_exact(count - buffer.len()).is_err() {
            return false;
        }
        buffer.resize(count, T::default());
    }
    true
}

fn returned<T>(buffer: &[T], bytes: u32) -> Option<&[T]> {
    let bytes = bytes as usize;
    if !bytes.is_multiple_of(size_of::<T>()) {
        return None;
    }
    buffer.get(..bytes / size_of::<T>())
}

impl RegionChanges {
    /// The caller must hold the acquired duplication frame. Missing, malformed,
    /// or unavailable metadata is never evidence that pixels stayed unchanged.
    pub fn affects(
        &mut self,
        duplication: &IDXGIOutputDuplication,
        crop: RECT,
        metadata_bytes: u32,
    ) -> bool {
        self.affects_with(
            crop,
            metadata_bytes,
            |buffer| {
                let mut required = 0;
                unsafe {
                    duplication.GetFrameDirtyRects(
                        size_of_val(buffer) as u32,
                        buffer.as_mut_ptr(),
                        &mut required,
                    )
                }
                .ok()
                .map(|()| required)
            },
            |buffer| {
                let mut required = 0;
                unsafe {
                    duplication.GetFrameMoveRects(
                        size_of_val(buffer) as u32,
                        buffer.as_mut_ptr(),
                        &mut required,
                    )
                }
                .ok()
                .map(|()| required)
            },
        )
    }

    fn affects_with(
        &mut self,
        crop: RECT,
        metadata_bytes: u32,
        mut dirty_rects: impl FnMut(&mut [RECT]) -> Option<u32>,
        mut move_rects: impl FnMut(&mut [DXGI_OUTDUPL_MOVE_RECT]) -> Option<u32>,
    ) -> bool {
        let bytes = metadata_bytes as usize;
        if bytes == 0 || bytes > MAX_METADATA_BYTES || !prepare(&mut self.dirty, bytes) {
            return true;
        }
        let Some(required) = dirty_rects(&mut self.dirty) else {
            return true;
        };
        let Some(dirty) = returned(&self.dirty, required) else {
            return true;
        };
        if dirty
            .iter()
            .any(|&rect| !valid(rect) || overlaps(crop, rect))
        {
            return true;
        }
        if !prepare(&mut self.moves, bytes) {
            return true;
        }
        let Some(required) = move_rects(&mut self.moves) else {
            return true;
        };
        let Some(moves) = returned(&self.moves, required) else {
            return true;
        };
        // Moves write their destination. Any changes to the old source are
        // described by other move destinations or dirty rectangles.
        moves
            .iter()
            .any(|m| !valid(m.DestinationRect) || overlaps(crop, m.DestinationRect))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn crop_intersections_include_single_pixels_but_not_touching_edges() {
        let crop = rect(100, 200, 516, 616);
        for update in [
            rect(0, 0, 101, 201),
            rect(515, 615, 900, 900),
            crop,
            rect(0, 0, 1920, 1080),
        ] {
            assert!(overlaps(crop, update));
        }
        for update in [
            rect(0, 0, 100, 1080),
            rect(516, 0, 1920, 1080),
            rect(0, 0, 1920, 200),
            rect(0, 616, 1920, 1080),
        ] {
            assert!(!overlaps(crop, update));
        }
        assert!(!valid(rect(0, 0, 0, 1)));
        assert!(!valid(rect(-1, 0, 4, 4)));
    }

    #[test]
    fn metadata_lengths_are_checked_and_storage_is_reused() {
        let mut buffer = Vec::<RECT>::new();
        assert!(prepare(&mut buffer, 35));
        assert_eq!(buffer.len(), 3);
        let pointer = buffer.as_ptr();
        assert!(prepare(&mut buffer, 16));
        assert_eq!(buffer.as_ptr(), pointer);
        assert_eq!(returned(&buffer, 32).unwrap().len(), 2);
        assert_eq!(returned(&buffer, 0).unwrap().len(), 0);
        assert!(returned(&buffer, 33).is_none());
        assert!(returned(&buffer, 64).is_none());
    }

    #[test]
    fn outside_updates_are_skipped_but_move_destinations_are_not() {
        let mut changes = RegionChanges::default();
        let crop = rect(100, 200, 516, 616);
        let dirty_outside = |buffer: &mut [RECT]| {
            buffer[0] = rect(0, 0, 20, 20);
            Some(size_of::<RECT>() as u32)
        };
        assert!(!changes.affects_with(crop, 64, dirty_outside, |_| Some(0)));
        assert!(!changes.affects_with(
            crop,
            64,
            |_| Some(0),
            |buffer| {
                buffer[0].DestinationRect = rect(516, 200, 600, 300);
                Some(size_of::<DXGI_OUTDUPL_MOVE_RECT>() as u32)
            }
        ));
        assert!(changes.affects_with(crop, 64, dirty_outside, |buffer| {
            buffer[0].DestinationRect = rect(515, 200, 600, 300);
            Some(size_of::<DXGI_OUTDUPL_MOVE_RECT>() as u32)
        }));
        assert!(changes.affects_with(
            crop,
            64,
            |buffer| {
                buffer[0] = rect(0, 0, 1920, 1080);
                Some(size_of::<RECT>() as u32)
            },
            |_| panic!("An intersecting dirty rectangle already requires refresh")
        ));
    }

    #[test]
    fn unavailable_or_malformed_metadata_always_refreshes() {
        let mut changes = RegionChanges::default();
        let crop = rect(100, 200, 516, 616);
        for bytes in [0, MAX_METADATA_BYTES as u32 + 1, u32::MAX] {
            assert!(changes.affects_with(crop, bytes, |_| panic!(), |_| panic!()));
        }
        assert!(changes.affects_with(crop, 64, |_| None, |_| panic!()));
        assert!(changes.affects_with(crop, 64, |_| Some(0), |_| None));
        assert!(changes.affects_with(crop, 64, |_| Some(3), |_| panic!()));
        assert!(changes.affects_with(crop, 64, |_| Some(128), |_| panic!()));
        assert!(changes.affects_with(crop, 64, |_| Some(0), |_| Some(1)));
        assert!(changes.affects_with(
            crop,
            64,
            |buffer| {
                buffer[0] = rect(4, 4, 2, 2);
                Some(size_of::<RECT>() as u32)
            },
            |_| panic!()
        ));
    }
}
