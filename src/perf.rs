use std::collections::VecDeque;
use std::time::Duration;

const DEFAULT_WINDOW: usize = 512;

#[derive(Debug, Clone, Copy)]
pub struct StatsSnapshot {
    pub fps_estimate: f64,
    pub frame_avg_us: u64,
    pub frame_p95_us: u64,
    pub frame_p99_us: u64,
    pub update_avg_us: u64,
    pub render_avg_us: u64,
    pub last_feed_events: usize,
}

impl Default for StatsSnapshot {
    fn default() -> Self {
        Self {
            fps_estimate: 0.0,
            frame_avg_us: 0,
            frame_p95_us: 0,
            frame_p99_us: 0,
            update_avg_us: 0,
            render_avg_us: 0,
            last_feed_events: 0,
        }
    }
}

#[derive(Debug)]
pub struct RuntimeStats {
    frame: RollingWindow,
    update: RollingWindow,
    render: RollingWindow,
    last_feed_events: usize,
}

impl RuntimeStats {
    pub fn new() -> Self {
        Self {
            frame: RollingWindow::new(DEFAULT_WINDOW),
            update: RollingWindow::new(DEFAULT_WINDOW),
            render: RollingWindow::new(DEFAULT_WINDOW),
            last_feed_events: 0,
        }
    }

    pub fn record_frame(
        &mut self,
        update_elapsed: Duration,
        render_elapsed: Duration,
        frame_elapsed: Duration,
        feed_events: usize,
    ) {
        self.update.push(duration_to_us(update_elapsed));
        self.render.push(duration_to_us(render_elapsed));
        self.frame.push(duration_to_us(frame_elapsed));
        self.last_feed_events = feed_events;
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let frame_avg = self.frame.avg();
        let fps = if frame_avg == 0 {
            0.0
        } else {
            1_000_000.0 / frame_avg as f64
        };

        StatsSnapshot {
            fps_estimate: fps,
            frame_avg_us: frame_avg,
            frame_p95_us: self.frame.percentile(95),
            frame_p99_us: self.frame.percentile(99),
            update_avg_us: self.update.avg(),
            render_avg_us: self.render.avg(),
            last_feed_events: self.last_feed_events,
        }
    }
}

#[derive(Debug)]
struct RollingWindow {
    capacity: usize,
    values: VecDeque<u64>,
}

impl RollingWindow {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            values: VecDeque::with_capacity(capacity),
        }
    }

    fn push(&mut self, value: u64) {
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value);
    }

    fn avg(&self) -> u64 {
        if self.values.is_empty() {
            return 0;
        }

        let sum: u128 = self.values.iter().map(|v| *v as u128).sum();
        (sum / self.values.len() as u128) as u64
    }

    fn percentile(&self, pct: u8) -> u64 {
        if self.values.is_empty() {
            return 0;
        }

        let mut sorted: Vec<u64> = self.values.iter().copied().collect();
        sorted.sort_unstable();

        let idx = ((sorted.len() - 1) * pct as usize) / 100;
        sorted[idx]
    }
}

fn duration_to_us(duration: Duration) -> u64 {
    duration.as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::RuntimeStats;
    use std::time::Duration;

    #[test]
    fn stats_snapshot_reports_non_zero_after_records() {
        let mut stats = RuntimeStats::new();
        stats.record_frame(
            Duration::from_micros(500),
            Duration::from_micros(400),
            Duration::from_micros(1_000),
            7,
        );

        let snapshot = stats.snapshot();
        assert!(snapshot.fps_estimate > 0.0);
        assert_eq!(snapshot.last_feed_events, 7);
        assert_eq!(snapshot.frame_avg_us, 1_000);
    }

    #[test]
    fn p99_is_at_least_p95() {
        let mut stats = RuntimeStats::new();
        for i in 1..=100 {
            stats.record_frame(
                Duration::from_micros(i),
                Duration::from_micros(i),
                Duration::from_micros(i),
                0,
            );
        }

        let snapshot = stats.snapshot();
        assert!(snapshot.frame_p99_us >= snapshot.frame_p95_us);
    }
}
