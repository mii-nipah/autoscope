use crate::launch::HostSession;
use anyhow::{Context, Result, bail};
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    time::Duration,
};

pub struct Viewer {
    child: Child,
    input: ChildStdin,
}

impl Viewer {
    pub fn attach(directory: &Path) -> Result<()> {
        let record = crate::session::status(directory)?;
        if record["status"] != "ready" {
            bail!(
                "session is {}; inspect it with autoscope status",
                record["status"]
            );
        }
        let host = HostSession::capture()?;
        let mut stream = UnixStream::connect(
            record["stream"]
                .as_str()
                .context("session omitted stream")?,
        )?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut header = [0; 20];
        stream.read_exact(&mut header)?;
        if &header[..4] != b"ASF1" {
            bail!("invalid frame stream header");
        }
        let number = |offset| u32::from_le_bytes(header[offset..offset + 4].try_into().unwrap());
        let (width, height, fps) = (number(4) as i32, number(8) as i32, number(12));
        crate::validate_display(width, height, fps)?;
        if number(16) != width as u32 * height as u32 * 4 {
            bail!("invalid frame stream size");
        }
        let mut frame = vec![0; number(16) as usize];
        let mut viewer = Self::start(width, height, fps, "attached session", &host)?;
        let result = loop {
            let mut sequence = [0; 8];
            match stream
                .read_exact(&mut sequence)
                .and_then(|_| stream.read_exact(&mut frame))
            {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break Ok(()),
                Err(error) => break Err(error.into()),
            }
            if viewer.frame(&frame).is_err() {
                break Ok(());
            }
        };
        viewer.stop();
        result
    }

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
            .env_remove("WAYLAND_DISPLAY")
            .envs(
                host.wayland_display
                    .as_ref()
                    .map(|value| ("WAYLAND_DISPLAY", value)),
            )
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
