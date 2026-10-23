//! Client measurements, the same for every toolkit: frame pace as seen by the compositor,
//! startup time and process memory (`perf.frames`, `perf.process`).
//!
//! A frame is a commit with a new buffer; the interval is the time between two frames of the
//! same surface. It is measured in the compositor because the comparison between toolkits (M1.5)
//! cannot depend on counters internal to each of them.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use silica_control::commands::{ProcessInfo, SurfaceFrames};
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Resource, Weak};

/// Intervals kept per surface: more than 2 minutes at 60 Hz.
const MAX_INTERVALS: usize = 8192;
/// Intervals over this threshold end up in the debug log, to see when they happen.
const SLOW_LOG: Duration = Duration::from_millis(25);
/// How many ancestors to walk up to tie a client to a launched process (sh -c, wrapper).
const MAX_ANCESTORS: usize = 8;

#[derive(Debug)]
struct Track {
    surface: Weak<WlSurface>,
    pid: Option<u32>,
    last: Option<Instant>,
    frames: u64,
    intervals: VecDeque<Duration>,
    startup: Option<Duration>,
}

#[derive(Debug, Default)]
pub struct Perf {
    surfaces: HashMap<ObjectId, Track>,
    launches: HashMap<u32, Instant>,
    /// The compositor's drawn frames, the last `MAX_INTERVALS`.
    renders: VecDeque<RenderStages>,
    /// The GPU's time on the drawn frames, when the renderer measures it; it arrives frames
    /// later than the frame itself.
    gpu: VecDeque<Duration>,
    /// Frames shown with nothing composed (planes only), and of those the direct scanouts.
    planes_only: u64,
    scanout: u64,
}

/// Where the time of one drawn frame went.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RenderStages {
    /// Building the scene: the elements of every output drawn.
    pub build: Duration,
    /// Damage tracking and drawing (on the CPU renderer, the pixels themselves).
    pub draw: Duration,
    /// Handing the frame to the display: copies, fences, the page flip.
    pub submit: Duration,
    /// The glass's share of `build`.
    pub glass: compositor_render::glass::GlassTimes,
}

impl RenderStages {
    /// The compositor's time on the frame (the GPU runs alongside).
    pub fn total(&self) -> Duration {
        self.build + self.draw + self.submit
    }
}

impl Perf {
    /// A process started by the compositor: its first buffer measures the startup time.
    pub fn launched(&mut self, pid: u32, at: Instant) {
        self.launches.insert(pid, at);
    }

    /// A commit with a new buffer on `surface`, from client `pid`.
    pub fn frame(&mut self, surface: &WlSurface, pid: Option<u32>, now: Instant) {
        let track = self.surfaces.entry(surface.id()).or_insert_with(|| Track {
            surface: surface.downgrade(),
            pid: None,
            last: None,
            frames: 0,
            intervals: VecDeque::new(),
            startup: None,
        });
        if track.pid.is_none() {
            track.pid = pid;
        }
        if track.frames == 0 && track.startup.is_none() {
            track.startup = pid
                .and_then(|pid| launched_ancestor(pid, &self.launches))
                .map(|at| now.saturating_duration_since(at));
        }
        if let Some(last) = track.last {
            if track.intervals.len() >= MAX_INTERVALS {
                track.intervals.pop_front();
            }
            let interval = now.saturating_duration_since(last);
            if interval > SLOW_LOG {
                tracing::debug!(
                    "slow frame: {:.1} ms after {} frames (pid {:?})",
                    millis(interval),
                    track.frames,
                    track.pid
                );
            }
            track.intervals.push_back(interval);
        }
        track.last = Some(now);
        track.frames = track.frames.saturating_add(1);
    }

    /// The measurements of the surfaces still alive; `label` gives a surface its name.
    pub fn report(
        &mut self,
        period: Duration,
        label: impl Fn(&ObjectId) -> String,
    ) -> Vec<SurfaceFrames> {
        self.surfaces
            .retain(|_, track| track.surface.upgrade().is_ok());
        let mut report: Vec<SurfaceFrames> = self
            .surfaces
            .iter()
            .map(|(id, track)| {
                let intervals: Vec<Duration> = track.intervals.iter().copied().collect();
                let stats = Stats::of(&intervals, period);
                SurfaceFrames {
                    surface: label(id),
                    pid: track.pid,
                    frames: track.frames,
                    mean_ms: stats.mean,
                    p50_ms: stats.p50,
                    p95_ms: stats.p95,
                    p99_ms: stats.p99,
                    max_ms: stats.max,
                    slow: stats.slow,
                    period_ms: millis(period),
                    startup_ms: track.startup.map(millis),
                }
            })
            .collect();
        report.sort_by(|a, b| a.surface.cmp(&b.surface));
        report
    }

    /// A frame the compositor drew (frames with nothing to draw are not counted).
    pub fn rendered(&mut self, stages: RenderStages) {
        if self.renders.len() >= MAX_INTERVALS {
            self.renders.pop_front();
        }
        self.renders.push_back(stages);
    }

    /// A frame shown without composing: a plane moved (the cursor) or, with `scanout`, a
    /// client's buffer is on the primary plane.
    pub fn shown_on_planes(&mut self, scanout: bool) {
        self.planes_only += 1;
        self.scanout += u64::from(scanout);
    }

    /// The GPU time of a drawn frame.
    pub fn gpu_rendered(&mut self, time: Duration) {
        if self.gpu.len() >= MAX_INTERVALS {
            self.gpu.pop_front();
        }
        self.gpu.push_back(time);
    }

    /// The times of the drawn frames since the last reset.
    pub fn render_times(&mut self, reset: bool) -> silica_control::commands::RenderTimes {
        // Counting the slow ones is not needed here: a long but finite period (mul_f64 on
        // Duration::MAX would overflow).
        let period = Duration::from_secs(3600);
        let totals: Vec<Duration> = self.renders.iter().map(RenderStages::total).collect();
        let stats = Stats::of(&totals, period);
        let stage = |name: &str, durations: Vec<Duration>| {
            let stats = Stats::of(&durations, period);
            silica_control::commands::StageTimes {
                stage: name.to_owned(),
                frames: durations.len() as u64,
                p50_ms: stats.p50,
                p95_ms: stats.p95,
                p99_ms: stats.p99,
                max_ms: stats.max,
            }
        };
        let mut stages = vec![
            stage("build", self.renders.iter().map(|r| r.build).collect()),
            stage("draw", self.renders.iter().map(|r| r.draw).collect()),
            stage("submit", self.renders.iter().map(|r| r.submit).collect()),
            stage(
                "glass.backdrop",
                self.renders.iter().map(|r| r.glass.backdrop).collect(),
            ),
            stage(
                "glass.blur",
                self.renders.iter().map(|r| r.glass.blur).collect(),
            ),
            stage(
                "glass.material",
                self.renders.iter().map(|r| r.glass.material).collect(),
            ),
        ];
        if !self.gpu.is_empty() {
            stages.push(stage("gpu", self.gpu.iter().copied().collect()));
        }
        let (planes_only, scanout) = (self.planes_only, self.scanout);
        if reset {
            self.renders.clear();
            self.gpu.clear();
            self.planes_only = 0;
            self.scanout = 0;
        }
        silica_control::commands::RenderTimes {
            frames: totals.len() as u64,
            mean_ms: stats.mean,
            p50_ms: stats.p50,
            p95_ms: stats.p95,
            p99_ms: stats.p99,
            max_ms: stats.max,
            stages,
            planes_only,
            scanout,
        }
    }

    /// Resets the counted frames; the startup time stays, it does not repeat.
    pub fn reset(&mut self) {
        for track in self.surfaces.values_mut() {
            track.last = None;
            track.frames = 0;
            track.intervals.clear();
        }
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

/// Interval statistics, in milliseconds.
#[derive(Debug, Default, PartialEq)]
struct Stats {
    mean: f64,
    p50: f64,
    p95: f64,
    p99: f64,
    max: f64,
    slow: u64,
}

impl Stats {
    fn of(intervals: &[Duration], period: Duration) -> Self {
        if intervals.is_empty() {
            return Self::default();
        }
        let mut sorted = intervals.to_vec();
        sorted.sort_unstable();
        // Nearest-rank percentile: the smallest value with at least p% of the samples.
        let percentile = |p: f64| {
            let rank = (p / 100.0 * sorted.len() as f64).ceil() as usize;
            sorted
                .get(rank.saturating_sub(1))
                .copied()
                .map_or(0.0, millis)
        };
        let total: Duration = sorted.iter().sum();
        let limit = period.mul_f64(1.5);
        Self {
            mean: millis(total) / sorted.len() as f64,
            p50: percentile(50.0),
            p95: percentile(95.0),
            p99: percentile(99.0),
            max: sorted.last().copied().map_or(0.0, millis),
            slow: sorted.iter().filter(|d| **d > limit).count() as u64,
        }
    }
}

/// The first process launched by the compositor among `pid` and its ancestors.
fn launched_ancestor(pid: u32, launches: &HashMap<u32, Instant>) -> Option<Instant> {
    let mut current = pid;
    for _ in 0..MAX_ANCESTORS {
        if let Some(at) = launches.get(&current) {
            return Some(*at);
        }
        current = parent_pid(current)?;
        if current <= 1 {
            return None;
        }
    }
    None
}

/// The parent of a process, from the fourth field of `/proc/<pid>/stat`.
fn parent_pid(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_parent(&stat)
}

fn parse_parent(stat: &str) -> Option<u32> {
    // The command name is in parentheses and may contain others: start from the last one.
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// RSS and PSS of a process from `/proc/<pid>/smaps_rollup`.
pub fn process_info(pid: u32) -> Result<ProcessInfo, String> {
    let rollup = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .map_err(|e| format!("process {pid}: {e}"))?;
    parse_rollup(pid, &rollup)
}

fn parse_rollup(pid: u32, rollup: &str) -> Result<ProcessInfo, String> {
    let field = |name: &str| {
        rollup
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| {
                rest.trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .ok()
            })
            .ok_or_else(|| format!("{name} missing in smaps_rollup"))
    };
    Ok(ProcessInfo {
        pid,
        rss_kib: field("Rss:")?,
        pss_kib: field("Pss:")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn stats_use_nearest_rank_and_count_slow_frames() {
        let mut intervals: Vec<Duration> = (0..98).map(|_| ms(16)).collect();
        intervals.push(ms(30));
        intervals.push(ms(50));
        let stats = Stats::of(&intervals, ms(16));
        assert_eq!(stats.p50, 16.0);
        assert_eq!(stats.p99, 30.0);
        assert_eq!(stats.max, 50.0);
        // Over 24 ms: 30 and 50.
        assert_eq!(stats.slow, 2);
        assert!((stats.mean - 16.48).abs() < 1e-9);
        assert_eq!(Stats::of(&[], ms(16)), Stats::default());
    }

    #[test]
    fn parent_from_stat_with_parentheses_in_name() {
        assert_eq!(parse_parent("42 (a) b) S 7 42 42 0"), Some(7));
        assert_eq!(parse_parent("garbage"), None);
    }

    #[test]
    fn rollup_fields() {
        let rollup = "00400000-7fff [rollup]\nRss:              51200 kB\nPss:              40960 kB\nPss_Anon:  1 kB\n";
        assert_eq!(
            parse_rollup(9, rollup),
            Ok(ProcessInfo {
                pid: 9,
                rss_kib: 51200,
                pss_kib: 40960
            })
        );
        assert!(parse_rollup(9, "Rss: 1 kB").is_err());
    }

    #[test]
    fn own_process_is_measurable() {
        let info = process_info(std::process::id()).unwrap();
        assert!(info.rss_kib > 0 && info.pss_kib > 0);
    }
}
