use std::ops::Range;

/// The rows of a scrollbar thumb for `len` rows of content shown `height` rows at a time,
/// scrolled down by `offset` rows. `None` when the content fits.
pub fn scrollbar_thumb(len: usize, height: usize, offset: usize) -> Option<Range<usize>> {
    if len <= height {
        return None;
    }
    let thumb_height = height.pow(2).div_ceil(len).min(height);
    let thumb_start = (height - thumb_height) * offset / len.saturating_sub(height).max(1);
    Some(thumb_start..thumb_start + thumb_height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_spans_the_visible_share() {
        assert_eq!(scrollbar_thumb(10, 10, 0), None);
        assert_eq!(scrollbar_thumb(20, 10, 0), Some(0..5));
        assert_eq!(scrollbar_thumb(20, 10, 10), Some(5..10));
        assert_eq!(scrollbar_thumb(1000, 10, 0), Some(0..1));
        assert_eq!(scrollbar_thumb(1000, 10, 990), Some(9..10));
    }
}
