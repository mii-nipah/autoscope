use std::collections::{HashSet, VecDeque};

use image::{GrayImage, imageops};
use optical_flow_lk::{DetectorContext, Grid, TrackParams, TrackStatus, TrackerContext};
use resvg::tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Stroke, Transform};

use super::{FrameSampler, MotionStyle, resize_rgba};

type Point = (f32, f32);
const MAX_TRACKS: usize = 240;
const PARAMS: TrackParams = TrackParams {
    window_size: 21,
    max_iterations: 30,
    min_eigen_threshold: optical_flow_lk::DEFAULT_MIN_EIGEN_THRESHOLD,
};

struct Track {
    points: VecDeque<(u64, Point)>,
    alive: bool,
}

pub(super) struct MotionTracks {
    pub style: MotionStyle,
    source: (u32, u32),
    target: (u32, u32),
    analysis: (u32, u32),
    sampler: FrameSampler,
    history_limit: Option<u32>,
    tick: u64,
    steps: u64,
    tracker: TrackerContext,
    detector: DetectorContext,
    tracks: Vec<Track>,
    previous: Option<GrayImage>,
}

impl MotionTracks {
    pub fn new(
        source: (u32, u32),
        target: (u32, u32),
        source_fps: u32,
        history_limit: Option<u32>,
        style: MotionStyle,
    ) -> Self {
        let longest = source.0.max(source.1).max(640);
        Self {
            style,
            source,
            target,
            analysis: (
                (source.0 * 640 / longest).max(1),
                (source.1 * 640 / longest).max(1),
            ),
            sampler: FrameSampler::new(source_fps, source_fps.min(15)),
            history_limit,
            tick: 0,
            steps: 0,
            tracker: TrackerContext::new(),
            detector: DetectorContext::new(),
            tracks: Vec::new(),
            previous: None,
        }
    }

    pub fn reset(&mut self) {
        self.tracks.clear();
        self.previous = None;
        self.steps = 0;
    }

    pub fn frame(&mut self, rgba: &[u8], capture: bool) -> Option<Vec<u8>> {
        self.tick += 1;
        if self.analysis.0 < 24 || self.analysis.1 < 24 {
            return None;
        }
        if self.sampler.take() || self.previous.is_none() {
            self.advance(rgba);
        }
        if let Some(limit) = self.history_limit {
            let oldest = self.tick.saturating_sub(limit as u64);
            for track in &mut self.tracks {
                while track.points.front().is_some_and(|p| p.0 < oldest) {
                    track.points.pop_front();
                }
            }
            self.tracks.retain(|t| !t.points.is_empty());
        }
        capture.then(|| self.render(rgba)).flatten()
    }

    fn advance(&mut self, rgba: &[u8]) {
        let gray = GrayImage::from_fn(self.source.0, self.source.1, |x, y| {
            let p = ((y * self.source.0 + x) * 4) as usize;
            image::Luma([((77 * rgba[p] as u32
                + 150 * rgba[p + 1] as u32
                + 29 * rgba[p + 2] as u32)
                >> 8) as u8])
        });
        let gray = imageops::resize(
            &gray,
            self.analysis.0,
            self.analysis.1,
            imageops::FilterType::Triangle,
        );
        if self.previous.as_ref() == Some(&gray) {
            for track in self.tracks.iter_mut().filter(|t| t.alive) {
                track
                    .points
                    .push_back((self.tick, track.points.back().unwrap().1));
            }
            self.steps += 1;
            return;
        }
        self.tracker.advance(&gray, 4);
        let points: Vec<_> = self
            .tracks
            .iter()
            .filter(|t| t.alive)
            .map(|t| t.points.back().unwrap().1)
            .collect();
        let results = self.tracker.track_fb(&points, None, PARAMS, 1.0);
        for (track, result) in self.tracks.iter_mut().filter(|t| t.alive).zip(results) {
            track.alive = result.status == TrackStatus::Tracked && result.error <= 20.0;
            if track.alive {
                track.points.push_back((self.tick, result.pos));
            }
        }
        let alive = self.tracks.iter().filter(|t| t.alive).count();
        // A discontinuity invalidates old paths; do not draw across unrelated scenes.
        let cut = !points.is_empty()
            && alive * 4 < points.len()
            && self.previous.as_ref().is_some_and(|old| {
                old.as_raw()
                    .iter()
                    .zip(gray.as_raw())
                    .map(|(a, b)| a.abs_diff(*b) as u64)
                    .sum::<u64>()
                    > 25 * gray.len() as u64
            });
        if cut {
            self.tracks.clear();
        }
        self.tracks.retain(|t| t.alive || t.points.len() > 1);
        if self.steps.is_multiple_of(5) || alive < MAX_TRACKS / 4 || cut {
            let existing: Vec<_> = self
                .tracks
                .iter()
                .filter(|t| t.alive)
                .map(|t| t.points.back().unwrap().1)
                .collect();
            let grid = Grid {
                cols: 8,
                rows: 6,
                max_per_cell: 5,
            };
            for corner in self
                .detector
                .detect_grid(&gray, grid, 0.01, 8, &existing)
                .iter()
                .take(MAX_TRACKS.saturating_sub(existing.len()))
            {
                self.tracks.push(Track {
                    points: VecDeque::from([(self.tick, corner.position())]),
                    alive: true,
                });
            }
        }
        self.previous = Some(gray);
        self.steps += 1;
    }

    fn render(&self, rgba: &[u8]) -> Option<Vec<u8>> {
        let scale = (
            self.target.0 as f32 / self.analysis.0 as f32,
            self.target.1 as f32 / self.analysis.1 as f32,
        );
        let mut paths: Vec<_> = self
            .tracks
            .iter()
            .filter_map(|track| {
                let points: Vec<_> = track
                    .points
                    .iter()
                    .map(|(_, p)| (p.0 * scale.0, p.1 * scale.1))
                    .collect();
                let first = *points.first()?;
                let excursion = points
                    .iter()
                    .map(|p| distance(first, *p))
                    .fold(0.0_f32, f32::max);
                (excursion >= 0.8).then_some((excursion, points))
            })
            .collect();
        paths.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut occupied = HashSet::new();
        let mut output = None;
        for (_, points) in paths {
            let last = points.last().unwrap();
            // One representative per display cell keeps text and small controls readable.
            if !occupied.insert(((last.0 / 24.) as i32, (last.1 / 24.) as i32)) {
                continue;
            }
            let out = output.get_or_insert_with(|| resize_rgba(rgba, self.source, self.target));
            let mut pixmap = PixmapMut::from_bytes(out, self.target.0, self.target.1).unwrap();
            draw_path(&mut pixmap, &points, self.style);
        }
        output
    }
}

fn distance(a: Point, b: Point) -> f32 {
    (b.0 - a.0).hypot(b.1 - a.1)
}

fn simplified(points: &[Point], start: usize, end: usize, out: &mut Vec<usize>) {
    let a = points[start];
    let b = points[end];
    let length = distance(a, b);
    let furthest = (start + 1..end)
        .map(|i| {
            let p = points[i];
            let t = if length > 0. {
                ((p.0 - a.0) * (b.0 - a.0) + (p.1 - a.1) * (b.1 - a.1)) / (length * length)
            } else {
                0.
            };
            let t = t.clamp(0., 1.);
            (
                i,
                distance(p, (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1))),
            )
        })
        .max_by(|a, b| a.1.total_cmp(&b.1));
    if let Some((i, _)) = furthest.filter(|(_, error)| *error > 0.4) {
        simplified(points, start, i, out);
        simplified(points, i, end, out);
    } else {
        out.push(end);
    }
}

fn draw_path(pixmap: &mut PixmapMut<'_>, points: &[Point], style: MotionStyle) {
    let mut indices = vec![0];
    simplified(points, 0, points.len() - 1, &mut indices);
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    let mut outline = PathBuilder::new();
    outline.move_to(points[0].0, points[0].1);
    for p in &points[1..] {
        outline.line_to(p.0, p.1);
    }
    paint.set_color_rgba8(12, 20, 29, 255);
    pixmap.stroke_path(
        &outline.finish().unwrap(),
        &paint,
        &Stroke {
            width: 4.,
            ..Stroke::default()
        },
        Transform::identity(),
        None,
    );
    for (k, pair) in points.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        if a == b {
            continue;
        }
        let mut path = PathBuilder::new();
        path.move_to(a.0, a.1);
        path.line_to(b.0, b.1);
        let path = path.finish().unwrap();
        tint(&mut paint, (k + 1) as f32 / (points.len() - 1) as f32);
        pixmap.stroke_path(
            &path,
            &paint,
            &Stroke {
                width: 2.,
                ..Stroke::default()
            },
            Transform::identity(),
            None,
        );
    }
    for (k, pair) in indices.windows(2).enumerate() {
        let (a, b) = (points[pair[0]], points[pair[1]]);
        let length = distance(a, b);
        let turn = indices.get(k + 2).is_none_or(|i| {
            let c = points[*i];
            (b.0 - a.0) * (c.0 - b.0) + (b.1 - a.1) * (c.1 - b.1) <= 0.5 * length * distance(b, c)
        });
        if style == MotionStyle::Arrows && length >= 0.8 && turn {
            let d = ((b.0 - a.0) / length, (b.1 - a.1) / length);
            let tip = (a.0 + 0.8 * (b.0 - a.0), a.1 + 0.8 * (b.1 - a.1));
            let mut head = PathBuilder::new();
            head.move_to(tip.0, tip.1);
            head.line_to(tip.0 - 7. * d.0 - 3.5 * d.1, tip.1 - 7. * d.1 + 3.5 * d.0);
            head.line_to(tip.0 - 7. * d.0 + 3.5 * d.1, tip.1 - 7. * d.1 - 3.5 * d.0);
            head.close();
            let head = head.finish().unwrap();
            paint.set_color_rgba8(12, 20, 29, 255);
            pixmap.stroke_path(
                &head,
                &paint,
                &Stroke {
                    width: 3.,
                    ..Stroke::default()
                },
                Transform::identity(),
                None,
            );
            tint(
                &mut paint,
                (pair[0] as f32 + 0.8 * (pair[1] - pair[0]) as f32) / (points.len() - 1) as f32,
            );
            pixmap.fill_path(
                &head,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }
}

fn tint(paint: &mut Paint<'_>, progress: f32) {
    let color: [u8; 3] = std::array::from_fn(|c| {
        let old = [82., 165., 248.][c];
        let new = [255., 199., 83.][c];
        (old + (new - old) * progress) as u8
    });
    paint.set_color_rgba8(color[0], color[1], color[2], 255);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(x: usize, y: usize) -> Vec<u8> {
        let mut rgba = [24, 24, 24, 255].repeat(160 * 120);
        for dy in 0..32 {
            for dx in 0..32 {
                let value = if (dx / 5 + dy / 5) % 2 == 0 { 220 } else { 70 };
                let p = ((y + dy) * 160 + x + dx) * 4;
                rgba[p..p + 3].fill(value);
            }
        }
        rgba
    }

    fn motion(limit: Option<u32>) -> MotionTracks {
        MotionTracks::new((160, 120), (160, 120), 15, limit, MotionStyle::Arrows)
    }

    #[test]
    fn lk_recovers_known_translation_and_rejects_replaced_content() {
        let mut tracker = motion(None);
        for n in 0..6 {
            tracker.frame(&scene(30 + n * 3, 30 + n * 2), false);
        }
        let tracks: Vec<_> = tracker
            .tracks
            .iter()
            .filter(|t| t.alive && t.points.len() == 6)
            .collect();
        assert!(tracks.len() >= 3, "only {} persistent tracks", tracks.len());
        for track in tracks {
            let (a, b) = (
                track.points.front().unwrap().1,
                track.points.back().unwrap().1,
            );
            assert!(distance((b.0 - a.0, b.1 - a.1), (15., 10.)) < 0.5);
        }
        let blank = [250, 250, 250, 255].repeat(160 * 120);
        assert!(tracker.frame(&blank, true).is_none());
        assert!(tracker.tracks.is_empty());
    }

    #[test]
    fn return_motion_survives_sparse_capture_until_reset_or_expiry() {
        for limit in [None, Some(15)] {
            let mut tracker = motion(limit);
            let original = scene(40, 40);
            assert!(tracker.frame(&original, true).is_none());
            for offset in [3, 6, 9, 12, 9, 6, 3, 0] {
                tracker.frame(&scene(40 + offset, 40), false);
            }
            assert!(
                tracker.frame(&original, true).is_some(),
                "out-and-back must remain visible"
            );
            for _ in 0..16 {
                tracker.frame(&original, false);
            }
            assert_eq!(tracker.frame(&original, true).is_none(), limit.is_some());
            tracker.reset();
            assert!(tracker.frame(&original, true).is_none());
            for _ in 0..10 {
                assert!(tracker.frame(&original, true).is_none());
            }
        }
    }

    #[test]
    fn arrows_face_travel_direction_and_trails_share_the_path() {
        let render = |style, points: &[Point]| {
            let mut pixmap = resvg::tiny_skia::Pixmap::new(48, 48).unwrap();
            draw_path(&mut pixmap.as_mut(), points, style);
            pixmap
        };
        for (points, base, empty) in [
            ([(10., 24.), (20., 24.), (30., 24.)], 20, 28),
            ([(30., 24.), (20., 24.), (10., 24.)], 20, 12),
        ] {
            let arrows = render(MotionStyle::Arrows, &points);
            let trails = render(MotionStyle::Trails, &points);
            assert!(arrows.pixel(base, 21).unwrap().alpha() > 0);
            assert_eq!(arrows.pixel(empty, 21).unwrap().alpha(), 0);
            assert_eq!(trails.pixel(base, 21).unwrap().alpha(), 0);
            assert!(trails.pixel(15, 24).unwrap().alpha() > 0);
            assert!(trails.pixel(25, 24).unwrap().alpha() > 0);
        }
    }
}
