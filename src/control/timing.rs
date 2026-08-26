use std::time::Duration;

const SAMPLE_PIXELS: usize = 4096;
const MIN_CHANGED_SAMPLES: usize = 20;
const CHANNEL_DELTA: u16 = 48;

pub(crate) fn sample_frame(rgba: &[u8]) -> Vec<[u8; 3]> {
    let pixels = rgba.len() / 4;
    let stride = pixels.div_ceil(SAMPLE_PIXELS).max(1);
    (0..pixels)
        .step_by(stride)
        .map(|pixel| {
            let offset = pixel * 4;
            [rgba[offset], rgba[offset + 1], rgba[offset + 2]]
        })
        .collect()
}

pub(crate) fn visibly_changed(before: &[[u8; 3]], after: &[[u8; 3]]) -> bool {
    if before.len() != after.len() {
        return true;
    }
    before
        .iter()
        .zip(after)
        .filter(|(before, after)| {
            before
                .iter()
                .zip(*after)
                .map(|(before, after)| u16::from(before.abs_diff(*after)))
                .sum::<u16>()
                >= CHANNEL_DELTA
        })
        .take(MIN_CHANGED_SAMPLES)
        .count()
        == MIN_CHANGED_SAMPLES
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SettleStatus {
    Stable,
    TimedOut,
}

pub(crate) struct VisualSettler {
    timeout: Duration,
    quiet: Duration,
    last_view: Option<u64>,
    last_change: Duration,
}

impl VisualSettler {
    pub(crate) fn new(timeout: Duration, quiet: Duration) -> Self {
        Self {
            timeout,
            quiet,
            last_view: None,
            last_change: Duration::ZERO,
        }
    }

    pub(crate) fn observe(&mut self, elapsed: Duration, view: u64) -> Option<SettleStatus> {
        if self.last_view != Some(view) {
            self.last_view = Some(view);
            self.last_change = elapsed;
        }
        if elapsed >= self.timeout {
            Some(SettleStatus::TimedOut)
        } else if elapsed.saturating_sub(self.last_change) >= self.quiet {
            Some(SettleStatus::Stable)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_sampling_ignores_tiny_motion_but_detects_layout_changes() {
        let baseline = vec![[0, 0, 0]; SAMPLE_PIXELS];
        let mut cursor_only = baseline.clone();
        cursor_only[..4].fill([255, 255, 255]);
        assert!(!visibly_changed(&baseline, &cursor_only));

        let mut layout = baseline.clone();
        layout[..MIN_CHANGED_SAMPLES].fill([255, 255, 255]);
        assert!(visibly_changed(&baseline, &layout));
    }

    #[test]
    fn settling_requires_quiet_after_the_last_change_and_is_bounded() {
        let mut settle =
            VisualSettler::new(Duration::from_millis(1_000), Duration::from_millis(300));
        assert_eq!(settle.observe(Duration::ZERO, 1), None);
        assert_eq!(settle.observe(Duration::from_millis(250), 1), None);
        assert_eq!(settle.observe(Duration::from_millis(400), 2), None);
        assert_eq!(
            settle.observe(Duration::from_millis(700), 2),
            Some(SettleStatus::Stable)
        );

        let mut settle = VisualSettler::new(Duration::from_millis(500), Duration::from_millis(300));
        assert_eq!(settle.observe(Duration::ZERO, 1), None);
        assert_eq!(settle.observe(Duration::from_millis(300), 2), None);
        assert_eq!(
            settle.observe(Duration::from_millis(500), 3),
            Some(SettleStatus::TimedOut)
        );
    }
}
