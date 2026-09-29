//! Preflight cel growth before allocating or cloning pixel buffers.

use super::{Document, MAX_DOCUMENT_CELS, MAX_DOCUMENT_FRAMES, checked_cel_pixel_total};
use crate::raster;

fn check_cel_count(count: usize) -> Result<(), String> {
    if count > MAX_DOCUMENT_CELS {
        return Err(format!(
            "document would have {count} cels; limit is {MAX_DOCUMENT_CELS}"
        ));
    }
    Ok(())
}

impl Document {
    pub(super) fn check_frame_growth(&self, count: usize) -> Result<(), String> {
        let frames = self
            .meta
            .frames
            .len()
            .checked_add(count)
            .ok_or("frame count overflowed")?;
        if frames > MAX_DOCUMENT_FRAMES {
            return Err(format!(
                "document would have {frames} frames; limit is {MAX_DOCUMENT_FRAMES}"
            ));
        }
        Ok(())
    }

    /// Check all copies together, so a batch cannot allocate its first few
    /// frames before discovering that its complete result exceeds the budget.
    pub(super) fn check_cel_copies(
        &self,
        select: impl Fn(usize, usize) -> bool,
        copies: usize,
    ) -> Result<(), String> {
        let selected = self.cels.iter().filter(|((l, f), _)| select(*l, *f));
        let count = selected
            .clone()
            .count()
            .checked_mul(copies)
            .and_then(|additional| self.cels.len().checked_add(additional))
            .ok_or("cel count overflowed")?;
        check_cel_count(count)?;
        let pixels = checked_cel_pixel_total(
            selected
                .map(|(_, (_, _, image))| (u64::from(image.width()), u64::from(image.height()))),
        )?;
        let copies = u64::try_from(copies).map_err(|_| "cel copy count overflowed")?;
        let additional = pixels
            .checked_mul(copies)
            .ok_or("copied cel pixels overflowed")?;
        checked_cel_pixel_total(
            self.cels
                .values()
                .map(|(_, _, image)| (u64::from(image.width()), u64::from(image.height())))
                .chain(std::iter::once((additional, 1))),
        )?;
        Ok(())
    }

    /// Account for the replaced cel's removal as well as the new dimensions.
    /// Existing, same-size edits remain legal at the document limit.
    pub(super) fn check_cel_replacement(
        &self,
        layer: usize,
        frame: usize,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        raster::checked_rgba_dimensions("cel", u64::from(width), u64::from(height))?;
        let key = (layer, frame);
        check_cel_count(self.cels.len() + usize::from(!self.cels.contains_key(&key)))?;
        checked_cel_pixel_total(
            self.cels
                .iter()
                .filter(|(existing, _)| **existing != key)
                .map(|(_, (_, _, image))| (u64::from(image.width()), u64::from(image.height())))
                .chain(std::iter::once((u64::from(width), u64::from(height)))),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{DEFAULT_FRAME_MS, FrameAction, MAX_DOCUMENT_LAYERS};

    #[test]
    fn oversized_frame_batch_is_rejected_before_any_frame_is_copied() {
        // One MiB of source pixels; the rejected batch would allocate 256 MiB
        // more. Only the source is needed to prove the complete call is refused.
        let mut document = Document::new("batch", 512, 512);
        document.fill_cel(0, 0, [10, 20, 30, 255]).unwrap();
        let before = document.structure();
        let error = document
            .add_frames(DEFAULT_FRAME_MS, Some(0), 256)
            .unwrap_err();
        assert!(error.contains("decoded pixels"), "{error}");
        assert_eq!(document.structure(), before);
        assert_eq!(document.get_pixel(0, 0, 0, 0).unwrap(), [10, 20, 30, 255]);
    }

    #[test]
    fn frame_growth_checks_counts_and_source_before_mutation() {
        let mut document = Document::new("frames", 1, 1);
        assert!(document.add_frames(100, None, 0).is_err());
        assert!(document.add_frame(100, Some(1)).is_err());
        assert!(document.add_frames(100, None, MAX_DOCUMENT_FRAMES).is_err());
        assert_eq!(document.meta.frames.len(), 1);
        document
            .add_frames(100, None, MAX_DOCUMENT_FRAMES - 1)
            .unwrap();
        for action in [FrameAction::Insert, FrameAction::Duplicate] {
            assert!(document.frame_ops(action, 0, None, None).is_err());
            assert_eq!(document.meta.frames.len(), MAX_DOCUMENT_FRAMES);
        }
    }

    #[test]
    fn frame_batch_counts_cels_across_every_layer() {
        let mut document = Document::new("layers", 1, 1);
        for _ in 1..MAX_DOCUMENT_LAYERS {
            document.add_layer(None, 255, raster::Blend::Normal);
        }
        for layer in 0..MAX_DOCUMENT_LAYERS {
            document.fill_cel(layer, 0, [1, 2, 3, 255]).unwrap();
        }
        let error = document.add_frames(100, Some(0), 64).unwrap_err();
        assert!(error.contains("cels; limit"), "{error}");
        assert_eq!(document.meta.frames.len(), 1);
        assert_eq!(document.cels.len(), MAX_DOCUMENT_LAYERS);
    }

    #[test]
    fn scaled_cel_is_checked_against_the_other_cels_before_resizing() {
        let mut document = Document::new("scale", 1, 1);
        document.fill_cel(0, 0, [1, 2, 3, 255]).unwrap();
        document.add_frame(100, Some(0)).unwrap();
        // Valid as one image, but exceeds the document's budget with frame 1.
        let error = document.scale_cel(0, 0, 8192, 8192, "nearest").unwrap_err();
        assert!(error.contains("decoded pixels"), "{error}");
        assert_eq!(document.cels[&(0, 0)].2.dimensions(), (1, 1));
        document.scale_cel(0, 0, 2, 2, "nearest").unwrap();
        assert_eq!(document.cels[&(0, 0)].2.dimensions(), (2, 2));
    }

    #[test]
    fn allowed_batch_copies_every_selected_cel_and_duration() {
        let mut document = Document::new("copies", 2, 2);
        document.fill_cel(0, 0, [7, 8, 9, 255]).unwrap();
        let last = document.add_frames(80, Some(0), 3).unwrap();
        assert_eq!(last, 3);
        for frame in 1..=last {
            assert_eq!(document.get_pixel(0, frame, 0, 0).unwrap(), [7, 8, 9, 255]);
            assert_eq!(document.meta.frames[frame].duration_ms, 80);
            assert!(document.dirty.contains(&(0, frame)));
        }
    }
}
