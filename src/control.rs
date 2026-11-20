use std::{
    fs::File,
    io::{self, BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod recording;
pub use recording::{MotionStyle, Recorder, RecordingMode, RecordingOptions, RecordingResult};

mod gesture;
mod viewer;
pub(crate) use viewer::Viewer;
mod pacing;
pub(crate) use gesture::DragPath;
pub(crate) mod timing;
pub(crate) use pacing::button_code;

fn default_wait() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "kebab-case")]
pub enum Request {
    Info,
    Move {
        x: f64,
        y: f64,
        #[serde(default)]
        normalize: bool,
        #[serde(default = "default_wait")]
        wait: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Click {
        button: String,
        x: Option<f64>,
        y: Option<f64>,
        #[serde(default)]
        normalize: bool,
        #[serde(default = "default_wait")]
        wait: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Drag {
        #[serde(flatten)]
        path: DragPath,
        #[serde(default = "default_wait")]
        wait: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Button {
        button: String,
        pressed: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Scroll {
        dx: f64,
        dy: f64,
        #[serde(default = "default_wait")]
        wait: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Type {
        text: String,
        #[serde(default = "default_wait")]
        wait: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Key {
        combo: String,
        #[serde(default = "default_wait")]
        wait: bool,
        #[serde(default)]
        view: Option<u64>,
    },
    Wait {
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
        #[serde(default = "default_quiet_ms")]
        quiet_ms: u64,
    },
    Screenshot {
        path: PathBuf,
    },
    RecordStart {
        path: PathBuf,
        #[serde(flatten)]
        options: RecordingOptions,
    },
    RecordStop,
    Quit,
}

fn default_timeout_ms() -> u64 {
    5_000
}

fn default_quiet_ms() -> u64 {
    600
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
    let timeout = Some(Duration::from_secs(2));
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else { break };
        if stream.set_read_timeout(timeout).is_err() || stream.set_write_timeout(timeout).is_err() {
            continue;
        }
        let response = read_request(&stream)
            .and_then(|request| pacing::execute(&tx, request))
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
    send_request_with_timeout(path, request, None)
}

pub fn send_request_with_timeout(
    path: &Path,
    request: &Request,
    timeout: Option<std::time::Duration>,
) -> Result<Response> {
    let mut stream = UnixStream::connect(path).context("connect to control socket")?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{RecordingOptions, Request};

    #[test]
    fn wire_commands_are_explicitly_tagged() {
        let request = Request::Click {
            button: "left".into(),
            x: Some(10.0),
            y: Some(20.0),
            normalize: true,
            wait: true,
            view: Some(7),
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
                wait: true,
                view: None,
            }
        );
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"cmd":"record-start","path":"/tmp/legacy.mp4"}"#)
                .unwrap(),
            Request::RecordStart {
                path: PathBuf::from("/tmp/legacy.mp4"),
                options: RecordingOptions::default(),
            }
        );
    }
}
