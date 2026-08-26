use std::{
    ffi::OsStr,
    fs::File,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::launch::HostSession;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Info,
    Move {
        x: f64,
        y: f64,
        #[serde(default)]
        normalize: bool,
    },
    Click {
        button: String,
        x: Option<f64>,
        y: Option<f64>,
        #[serde(default)]
        normalize: bool,
    },
    Button {
        button: String,
        pressed: bool,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    Type {
        text: String,
    },
    Key {
        combo: String,
    },
    Screenshot {
        path: PathBuf,
    },
    RecordStart {
        path: PathBuf,
        #[serde(default)]
        fps: Option<u32>,
        #[serde(default)]
        mode: RecordingMode,
        #[serde(default = "default_frames_per_image")]
        frames_per_image: u32,
    },
    RecordStop,
    Quit,
}

fn default_frames_per_image() -> u32 {
    10
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RecordingMode {
    #[default]
    Video,
    Images,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingResult {
    pub mode: RecordingMode,
    pub fps: u32,
    pub frames: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

pub struct Envelope {
    pub request: Request,
    pub reply: Sender<Response>,
}

pub fn start_server(path: &Path) -> Result<Receiver<Envelope>> {
    let listener = UnixListener::bind(path).context("bind control socket")?;
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .name("autoscope-control".into())
        .spawn(move || serve(listener, tx))?;
    Ok(rx)
}

fn serve(listener: UnixListener, tx: Sender<Envelope>) {
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else { break };
        let response = read_request(&stream)
            .and_then(|request| {
                let (reply_tx, reply_rx) = mpsc::channel();
                tx.send(Envelope {
                    request,
                    reply: reply_tx,
                })
                .map_err(|_| "compositor stopped".to_string())?;
                reply_rx
                    .recv_timeout(Duration::from_secs(30))
                    .map_err(|_| "compositor did not answer".to_string())
            })
            .unwrap_or_else(Response::error);
        let _ = serde_json::to_writer(&mut stream, &response);
        let _ = stream.write_all(b"\n");
    }
}

fn read_request(stream: &UnixStream) -> std::result::Result<Request, String> {
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    serde_json::from_str(&line).map_err(|error| format!("invalid request: {error}"))
}

pub fn send_request(path: &Path, request: &Request) -> Result<Response> {
    let mut stream = UnixStream::connect(path).context("connect to control socket")?;
    serde_json::to_writer(&mut stream, request)?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

pub fn stream_to_stdout(path: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(path).context("connect to frame stream")?;
    match io::copy(&mut stream, &mut io::stdout().lock()) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let file = File::create(path).with_context(|| format!("create {}", path.display()))?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(rgba)
        .context("encode screenshot")?;
    Ok(())
}

pub struct Recorder {
    pub path: PathBuf,
    pub fps: u32,
    pub mode: RecordingMode,
    sampler: FrameSampler,
    output: RecordingOutput,
    frames: u64,
}

impl Recorder {
    pub fn start(
        path: &Path,
        width: i32,
        height: i32,
        source_fps: u32,
        fps: u32,
        mode: RecordingMode,
        frames_per_image: u32,
    ) -> Result<Self> {
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
        Ok(Self {
            path: path.to_owned(),
            fps,
            mode,
            sampler: FrameSampler::new(source_fps, fps),
            output,
            frames: 0,
        })
    }

    pub fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        if self.sampler.take() {
            self.output.frame(rgba)?;
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
    fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        match self {
            Self::Video(recorder) => recorder.frame(rgba),
            Self::Images(recorder) => recorder.frame(rgba),
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

    fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        self.pending
            .push(resize_rgba(rgba, self.source_size, self.thumbnail_size));
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

pub struct Viewer {
    child: Child,
    input: ChildStdin,
}

impl Viewer {
    pub fn start(
        width: i32,
        height: i32,
        fps: u32,
        name: &str,
        host: &HostSession,
    ) -> Result<Self> {
        let mut child = Command::new("ffplay")
            .args(["-hide_banner", "-loglevel", "warning", "-f", "rawvideo"])
            .args(["-pixel_format", "rgba", "-video_size"])
            .arg(format!("{width}x{height}"))
            .args(["-framerate", &fps.to_string(), "-i", "pipe:0", "-an"])
            .args(["-x", &width.min(800).to_string()])
            .args(["-y", &height.min(500).to_string()])
            .args(["-window_title", &format!("autoscope — {name}")])
            .env("XDG_RUNTIME_DIR", &host.runtime_dir)
            .env("WAYLAND_DISPLAY", &host.wayland_display)
            .env_remove("DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .context("start ffplay viewer")?;
        let input = child.stdin.take().context("open ffplay input")?;
        Ok(Self { child, input })
    }

    pub fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        self.input.write_all(rgba)
    }

    pub fn stop(mut self) {
        drop(self.input);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{FrameSampler, RecordingMode, Request};

    #[test]
    fn wire_commands_are_explicitly_tagged() {
        let request = Request::Click {
            button: "left".into(),
            x: Some(10.0),
            y: Some(20.0),
            normalize: true,
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
        assert!(encoded.contains("\"cmd\":\"click\""));
        assert!(encoded.contains("\"normalize\":true"));

        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"move","x":10,"y":20}"#).unwrap(),
            Request::Move {
                x: 10.0,
                y: 20.0,
                normalize: false,
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"record-start","path":"/tmp/legacy.mp4"}"#)
                .unwrap(),
            Request::RecordStart {
                path: PathBuf::from("/tmp/legacy.mp4"),
                fps: None,
                mode: RecordingMode::Video,
                frames_per_image: 10,
            }
        );
    }

    #[test]
    fn recording_sampler_is_independent_from_render_fps() {
        let mut sampler = FrameSampler::new(60, 1);
        let captured: Vec<_> = (0..=120).filter(|_| sampler.take()).collect();
        assert_eq!(captured, [0, 60, 120]);

        let mut sampler = FrameSampler::new(60, 30);
        assert_eq!((0..60).filter(|_| sampler.take()).count(), 30);
    }
}
