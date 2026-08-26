use std::{
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
    },
    Click {
        button: String,
        x: Option<f64>,
        y: Option<f64>,
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
    },
    RecordStop,
    Quit,
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
        .name("autowayland-control".into())
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
    child: Child,
    input: Option<ChildStdin>,
    pub path: PathBuf,
}

impl Recorder {
    pub fn start(path: &Path, width: i32, height: i32, fps: u32) -> Result<Self> {
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
            path: path.to_owned(),
        })
    }

    pub fn frame(&mut self, rgba: &[u8]) -> io::Result<()> {
        self.input.as_mut().unwrap().write_all(rgba)
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        drop(self.input.take());
        let status = self.child.wait()?;
        if !status.success() {
            bail!("ffmpeg exited with {status}");
        }
        Ok(self.path)
    }
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
            .args(["-window_title", &format!("autowayland — {name}")])
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
    use super::Request;

    #[test]
    fn wire_commands_are_explicitly_tagged() {
        let request = Request::Click {
            button: "left".into(),
            x: Some(10.0),
            y: Some(20.0),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
        assert!(encoded.contains("\"cmd\":\"click\""));
    }
}
