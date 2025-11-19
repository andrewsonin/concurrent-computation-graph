use core::ops::Range;

pub(crate) trait RangeSplitAtHalf {
    fn split_at_half(&self) -> (Range<usize>, Range<usize>);
}

impl RangeSplitAtHalf for Range<usize> {
    #[inline]
    fn split_at_half(&self) -> (Range<usize>, Range<usize>) {
        let len = self
            .end
            .checked_sub(self.start)
            .expect("RangeSplitAtHalf::split_at");
        let mid = len / 2;
        let mid_abs = self.start + mid;
        (self.start..mid_abs, mid_abs..self.end)
    }
}
