use std::{
    ffi::OsStr,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
};

use anyhow::{Context, Result, bail};
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use super::write_png;
mod motion;
use motion::MotionTracks;

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Default,
    clap::ValueEnum,
    schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingMode {
    #[default]
    Video,
    Images,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Default,
    clap::ValueEnum,
    schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum MotionStyle {
    Off,
    #[default]
    Arrows,
    Trails,
}

// Keep explicit boolean selections from existing clients working at the wire boundary.
fn motion_style<'de, D: serde::Deserializer<'de>>(d: D) -> Result<MotionStyle, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Setting {
        Style(MotionStyle),
        Enabled(bool),
    }
    Ok(match Option::<Setting>::deserialize(d)? {
        Some(Setting::Style(style)) => style,
        Some(Setting::Enabled(false)) => MotionStyle::Off,
        _ => MotionStyle::Arrows,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingResult {
    pub mode: RecordingMode,
    pub fps: u32,
    pub frames: u64,
    #[serde(default, deserialize_with = "motion_style")]
    pub motion: MotionStyle,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, clap::Args, schemars::JsonSchema,
)]
#[serde(default)]
pub struct RecordingOptions {
    /// Recording FPS; defaults to the session FPS and cannot exceed it.
    #[arg(long)]
    pub fps: Option<u32>,
    /// video creates an MP4; images creates chronological contact-sheet PNGs.
    #[arg(long, value_enum, default_value_t = RecordingMode::Video)]
    #[serde(deserialize_with = "default_mode")]
    pub mode: RecordingMode,
    /// Frames per contact sheet in images mode (default and maximum 10).
    #[arg(long, default_value_t = 10)]
    #[serde(deserialize_with = "default_sheet_frames")]
    pub frames_per_image: u32,
    /// LK motion paths: arrows (default), trails without heads, or off.
    #[arg(long, value_enum, default_value_t = MotionStyle::Arrows,
          num_args = 0..=1, require_equals = true, default_missing_value = "arrows")]
    #[serde(deserialize_with = "motion_style")]
    pub motion: MotionStyle,
}

impl Default for RecordingOptions {
    fn default() -> Self {
        Self {
            fps: None,
            mode: RecordingMode::Video,
            frames_per_image: 10,
            motion: MotionStyle::Arrows,
        }
    }
}

// Older MCP clients may send null for optional recording settings.
fn default_mode<'de, D: serde::Deserializer<'de>>(d: D) -> Result<RecordingMode, D::Error> {
    Ok(Option::<RecordingMode>::deserialize(d)?.unwrap_or_default())
}

fn default_sheet_frames<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    Ok(Option::<u32>::deserialize(d)?.unwrap_or(RecordingOptions::default().frames_per_image))
}

pub struct Recorder {
    pub path: PathBuf,
    pub fps: u32,
    pub mode: RecordingMode,
    sampler: FrameSampler,
    output: RecordingOutput,
    frames: u64,
    motion: Option<MotionTracks>,
}

impl Recorder {
    pub fn start(
        path: &Path,
        size: (i32, i32),
        source_fps: u32,
        options: RecordingOptions,
    ) -> Result<Self> {
        let (width, height) = size;
        let RecordingOptions {
            fps,
            mode,
            frames_per_image,
            motion,
        } = options;
        let fps = fps.unwrap_or(source_fps);
        if !(1..=source_fps).contains(&fps) {
            bail!("recording fps must be between 1 and the session fps ({source_fps})");
        }
        let output = match mode {
            RecordingMode::Video => {
                RecordingOutput::Video(VideoRecorder::start(path, width, height, fps)?)
            }
            RecordingMode::Images => RecordingOutput::Images(ImageRecorder::new(
                path,
                width as u32,
                height as u32,
                frames_per_image,
            )?),
        };
        let target_size = match &output {
            RecordingOutput::Images(images) => images.thumbnail_size,
            RecordingOutput::Video(_) => (width as u32, height as u32),
        };
        Ok(Self {
            path: path.to_owned(),
            fps,
            mode,
            sampler: FrameSampler::new(source_fps, fps),
            output,
            frames: 0,
            motion: (motion != MotionStyle::Off).then(|| {
                MotionTracks::new(
                    (width as u32, height as u32),
                    target_size,
                    source_fps,
                    (mode == RecordingMode::Video).then_some(source_fps),
                    motion,
                )
            }),
        })
    }

    pub fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        let capture = self.sampler.take();
        if capture
            && matches!(&self.output, RecordingOutput::Images(images) if images.pending.is_empty())
            && let Some(motion) = &mut self.motion
        {
            motion.reset();
        }
        let annotated = self
            .motion
            .as_mut()
            .and_then(|motion| motion.frame(rgba, capture));
        if capture {
            self.output.frame(rgba, annotated)?;
            self.frames += 1;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<RecordingResult> {
        let files = self.output.finish()?;
        let path = if self.mode == RecordingMode::Video {
            files.first().cloned()
        } else {
            None
        };
        Ok(RecordingResult {
            mode: self.mode,
            fps: self.fps,
            frames: self.frames,
            motion: self.motion.map_or(MotionStyle::Off, |motion| motion.style),
            path,
            files,
        })
    }
}

struct FrameSampler {
    source_fps: u32,
    target_fps: u32,
    seen: u64,
    accumulator: u32,
}

impl FrameSampler {
    fn new(source_fps: u32, target_fps: u32) -> Self {
        Self {
            source_fps,
            target_fps,
            seen: 0,
            accumulator: 0,
        }
    }

    fn take(&mut self) -> bool {
        self.seen += 1;
        if self.seen == 1 {
            return true;
        }
        self.accumulator += self.target_fps;
        if self.accumulator < self.source_fps {
            return false;
        }
        self.accumulator -= self.source_fps;
        true
    }
}

enum RecordingOutput {
    Video(VideoRecorder),
    Images(ImageRecorder),
}

impl RecordingOutput {
    fn frame(&mut self, rgba: &[u8], annotated: Option<Vec<u8>>) -> io::Result<()> {
        match self {
            Self::Video(recorder) => recorder.frame(annotated.as_deref().unwrap_or(rgba)),
            Self::Images(recorder) => recorder.frame(rgba, annotated),
        }
    }

    fn finish(self) -> Result<Vec<PathBuf>> {
        match self {
            Self::Video(recorder) => recorder.finish().map(|path| vec![path]),
            Self::Images(recorder) => recorder.finish(),
        }
    }
}

struct VideoRecorder {
    child: Child,
    input: Option<ChildStdin>,
    path: PathBuf,
}

impl VideoRecorder {
    fn start(path: &Path, width: i32, height: i32, fps: u32) -> Result<Self> {
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo"])
            .args(["-pixel_format", "rgba", "-video_size"])
            .arg(format!("{width}x{height}"))
            .args(["-framerate", &fps.to_string(), "-i", "pipe:0", "-an"])
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("start ffmpeg recorder")?;
        let input = child.stdin.take().context("open ffmpeg input")?;
        Ok(Self {
            child,
            input: Some(input),
            path: path.into(),
        })
    }

    fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        self.input.as_mut().unwrap().write_all(rgba)
    }

    fn finish(mut self) -> Result<PathBuf> {
        drop(self.input.take());
        let status = self.child.wait()?;
        if !status.success() {
            bail!("ffmpeg exited with {status}");
        }
        Ok(self.path)
    }
}

struct ImageRecorder {
    base_path: PathBuf,
    source_size: (u32, u32),
    thumbnail_size: (u32, u32),
    frames_per_image: usize,
    pending: Vec<Vec<u8>>,
    files: Vec<PathBuf>,
}

impl ImageRecorder {
    fn new(path: &Path, width: u32, height: u32, frames_per_image: u32) -> Result<Self> {
        if !(1..=10).contains(&frames_per_image) {
            bail!("frames per image must be between 1 and 10");
        }
        let thumbnail_width = width.min(320);
        let thumbnail_height = (height * thumbnail_width / width).max(1);
        Ok(Self {
            base_path: path.into(),
            source_size: (width, height),
            thumbnail_size: (thumbnail_width, thumbnail_height),
            frames_per_image: frames_per_image as usize,
            pending: Vec::with_capacity(frames_per_image as usize),
            files: Vec::new(),
        })
    }

    fn frame(&mut self, rgba: &[u8], annotated: Option<Vec<u8>>) -> io::Result<()> {
        self.pending.push(
            annotated.unwrap_or_else(|| resize_rgba(rgba, self.source_size, self.thumbnail_size)),
        );
        if self.pending.len() == self.frames_per_image {
            self.flush().map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        const MARGIN: u32 = 4;
        const PADDING: u32 = 4;
        let count = self.pending.len() as u32;
        let columns = count.min(5);
        let rows = count.div_ceil(columns);
        let (thumbnail_width, thumbnail_height) = self.thumbnail_size;
        let width = MARGIN * 2 + columns * thumbnail_width + (columns - 1) * PADDING;
        let height = MARGIN * 2 + rows * thumbnail_height + (rows - 1) * PADDING;
        let mut sheet = vec![0_u8; (width * height * 4) as usize];
        for pixel in sheet.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[20, 22, 27, 255]);
        }
        for (index, thumbnail) in self.pending.iter().enumerate() {
            let column = index as u32 % columns;
            let row = index as u32 / columns;
            let x = MARGIN + column * (thumbnail_width + PADDING);
            let y = MARGIN + row * (thumbnail_height + PADDING);
            for thumbnail_y in 0..thumbnail_height {
                let source = (thumbnail_y * thumbnail_width * 4) as usize;
                let target = (((y + thumbnail_y) * width + x) * 4) as usize;
                let bytes = (thumbnail_width * 4) as usize;
                sheet[target..target + bytes].copy_from_slice(&thumbnail[source..source + bytes]);
            }
        }
        let path = numbered_png(&self.base_path, self.files.len() + 1);
        write_png(&path, width, height, &sheet)?;
        self.files.push(path);
        self.pending.clear();
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<PathBuf>> {
        self.flush()?;
        Ok(self.files)
    }
}

fn resize_rgba(rgba: &[u8], source: (u32, u32), target: (u32, u32)) -> Vec<u8> {
    let mut resized = vec![0; (target.0 * target.1 * 4) as usize];
    for y in 0..target.1 {
        let source_y = y * source.1 / target.1;
        for x in 0..target.0 {
            let source_x = x * source.0 / target.0;
            let from = ((source_y * source.0 + source_x) * 4) as usize;
            let to = ((y * target.0 + x) * 4) as usize;
            resized[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
        }
    }
    resized
}

fn numbered_png(base: &Path, sequence: usize) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or("recording");
    base.with_file_name(format!("{stem}-{sequence:03}.png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_sampler_is_independent_from_render_fps() {
        let mut sampler = FrameSampler::new(60, 1);
        let captured: Vec<_> = (0..=120).filter(|_| sampler.take()).collect();
        assert_eq!(captured, [0, 60, 120]);

        let mut sampler = FrameSampler::new(60, 30);
        assert_eq!((0..60).filter(|_| sampler.take()).count(), 30);
    }

    #[test]
    fn motion_defaults_and_explicit_wire_choices_agree_with_cli() {
        use clap::Parser;
        #[derive(Parser)]
        struct Args {
            #[command(flatten)]
            options: RecordingOptions,
            path: Option<PathBuf>,
        }
        for (wire, arguments, expected) in [
            ("{}", vec!["record"], MotionStyle::Arrows),
            (
                r#"{"motion":null}"#,
                vec!["record", "--motion"],
                MotionStyle::Arrows,
            ),
            (
                r#"{"motion":"arrows"}"#,
                vec!["record", "--motion=arrows"],
                MotionStyle::Arrows,
            ),
            (
                r#"{"motion":"trails"}"#,
                vec!["record", "--motion=trails"],
                MotionStyle::Trails,
            ),
            (
                r#"{"motion":"off"}"#,
                vec!["record", "--motion=off"],
                MotionStyle::Off,
            ),
        ] {
            assert_eq!(
                serde_json::from_str::<RecordingOptions>(wire)
                    .unwrap()
                    .motion,
                expected
            );
            assert_eq!(
                Args::try_parse_from(arguments).unwrap().options.motion,
                expected
            );
        }
        assert_eq!(
            serde_json::from_str::<RecordingOptions>(r#"{"motion":false}"#)
                .unwrap()
                .motion,
            MotionStyle::Off
        );
        assert_eq!(
            serde_json::from_str::<RecordingOptions>(r#"{"motion":true}"#)
                .unwrap()
                .motion,
            MotionStyle::Arrows
        );
        assert!(serde_json::from_str::<RecordingOptions>(r#"{"motion":"invalid"}"#).is_err());
        let legacy = Args::try_parse_from(["record", "--motion", "/tmp/video.mp4"]).unwrap();
        assert_eq!(legacy.options.motion, MotionStyle::Arrows);
        assert_eq!(legacy.path, Some(PathBuf::from("/tmp/video.mp4")));
    }
}
