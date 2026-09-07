//! Real compositor/process integration. Run with `cargo test --test session_workflow -- --ignored`.
use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_autoscope");

fn json_command(args: &[&str]) -> Value {
    let output = Command::new(BIN).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Session(Value);

impl Session {
    fn field(&self, key: &str) -> &str {
        self.0[key].as_str().unwrap()
    }
    fn ctl(&self, args: &[&str]) -> Value {
        let mut command = vec!["ctl", self.field("control")];
        command.extend(args);
        json_command(&command)
    }
    fn exited(&self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let record = json_command(&["status", self.field("session")]);
            if record["status"] == "exited" || record["status"] == "failed" {
                return record;
            }
            assert!(
                Instant::now() < deadline,
                "session did not shut down: {record}"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if Path::new(self.field("control")).exists() {
            let _ = Command::new(BIN)
                .args(["ctl", self.field("control"), "quit"])
                .output();
        }
    }
}

#[test]
#[ignore = "requires a Linux desktop environment, bwrap, Xwayland and ffmpeg"]
fn detached_shared_files_and_recording_survive_signal_shutdown() {
    let temp = tempfile::tempdir().unwrap();
    let actual = temp.path().join("actual files");
    fs::create_dir(&actual).unwrap();
    let shared = temp.path().join("shared files");
    std::os::unix::fs::symlink(&actual, &shared).unwrap();
    let input: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();
    fs::write(shared.join("input.bin"), &input).unwrap();

    let output = Command::new(BIN).process_group(0)
        .env("XDG_STATE_HOME", temp.path().join("state"))
        .args(["run", "--detach", "--width", "640", "--height", "480", "--name", "file probe", "--shared-dir"])
        .arg(&shared).args(["--sandbox", "bwrap", "--", "/bin/sh", "-c",
            "set -eu; test \"$AUTOSCOPE_SHARED_DIR\" = \"$2\"; cmp \"$2/input.bin\" \"$HOME/Shared/input.bin\"; cp \"$HOME/Shared/input.bin\" \"$AUTOSCOPE_SHARED_DIR/output.bin\"; printf '%s' \"$1\" > \"$AUTOSCOPE_SHARED_DIR/argument.txt\"; echo probe-started; sleep 300",
            "probe", "--detach"]).arg(&shared)
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let session = Session(serde_json::from_slice(&output.stdout).unwrap());
    let pid = session.0["pid"].as_i64().unwrap() as i32;
    assert_eq!(
        unsafe { libc::getsid(pid) },
        pid,
        "worker must have an independent OS session"
    );
    let live = json_command(&["status", session.field("session")]);
    assert_eq!(live["status"], "ready");
    assert!(live["live"]["frame"].as_u64().is_some());

    let deadline = Instant::now() + Duration::from_secs(5);
    while !shared.join("argument.txt").exists() {
        assert!(
            Instant::now() < deadline,
            "app did not write the shared folder"
        );
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(fs::read(shared.join("output.bin")).unwrap(), input);
    assert_eq!(
        fs::read_to_string(shared.join("argument.txt")).unwrap(),
        "--detach"
    );
    assert!(
        fs::read_to_string(session.field("log"))
            .unwrap()
            .contains("probe-started")
    );

    let video = temp.path().join("process.mp4");
    session.ctl(&["record-start", "--fps", "5", video.to_str().unwrap()]);
    let drag = session.ctl(&[
        "drag", "--origin", "235,149", "--scale", "4", "10,10", "20,20", "30,10",
    ]);
    assert_eq!(drag["result"]["to"], serde_json::json!([355.0, 189.0]));
    assert!(drag["result"]["wait"]["status"].is_string());

    assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
    let record = session.exited();
    assert_eq!(record["exit"]["reason"], "signal");
    assert_eq!(record["exit"]["signal"], libc::SIGTERM);
    assert_eq!(record["exit"]["sender_pid"], std::process::id());
    assert!(!Path::new(session.field("control")).exists());
    assert!(
        Path::new(session.field("session"))
            .join("session.json")
            .exists()
    );
    assert_eq!(fs::read(shared.join("output.bin")).unwrap(), input);
    assert!(fs::metadata(&video).unwrap().len() > 0);
    let decode = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(video)
        .args(["-f", "null", "-"])
        .output()
        .unwrap();
    assert!(
        decode.status.success(),
        "{}",
        String::from_utf8_lossy(&decode.stderr)
    );
}
