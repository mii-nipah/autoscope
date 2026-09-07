mod coordinator;

use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context, Result};
use base64::Engine;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::wrapper::{Json, Parameters},
    model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::control::{DragPath, RecordingMode, Request};
use coordinator::{
    ActionResult, Application, Capture, Coordinator, SandboxMode, SessionInfo, SessionOptions,
};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SpawnCommandParams {
    /// Program followed by its arguments. No shell interpretation is performed.
    command: Vec<String>,
    /// Host-command sandbox policy (default auto).
    sandbox: Option<SandboxMode>,
    /// Optional display, network, and viewer settings.
    options: Option<SessionOptions>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SpawnFlatpakParams {
    /// Installed Flatpak application ID, for example com.google.Chrome.
    application_id: String,
    /// Arguments passed to the application.
    arguments: Option<Vec<String>>,
    /// Optional display, network, and viewer settings.
    options: Option<SessionOptions>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct SessionParam {
    /// Coordinator session ID returned by spawn_command or spawn_flatpak.
    session: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct MoveParams {
    session: String,
    /// View number returned with the most recent screenshot or input result.
    view: u64,
    x: f64,
    y: f64,
    /// Interpret x and y as 0.0 through 1.0 instead of absolute pixels.
    normalize: Option<bool>,
    /// Wait for hover-driven visual stability (default true; false is useful during a drag).
    wait: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClickParams {
    session: String,
    /// View number returned with the most recent screenshot or input result.
    view: u64,
    /// left, right, or middle (default left).
    button: Option<String>,
    /// Optional click position; x and y must be supplied together.
    x: Option<f64>,
    /// Optional click position; x and y must be supplied together.
    y: Option<f64>,
    /// Interpret x and y as 0.0 through 1.0 instead of absolute pixels.
    normalize: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ButtonParams {
    session: String,
    /// View number returned with the most recent screenshot or input result.
    view: u64,
    /// left, right, or middle.
    button: String,
    /// true presses and holds; false releases.
    pressed: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DragParams {
    session: String,
    /// View from the most recent screenshot; checked before the gesture starts.
    view: u64,
    #[serde(flatten)]
    path: DragPath,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScrollParams {
    session: String,
    /// View number returned with the most recent screenshot or input result.
    view: u64,
    /// Horizontal wheel amount.
    dx: f64,
    /// Vertical wheel amount.
    dy: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TextParams {
    session: String,
    /// View number returned with the most recent screenshot or input result.
    view: u64,
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeyParams {
    session: String,
    /// View number returned with the most recent screenshot or input result.
    view: u64,
    /// Named key or modifier combination, for example ENTER or CTRL+L.
    combo: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum RecordingModeParam {
    Video,
    Images,
}

impl From<RecordingModeParam> for RecordingMode {
    fn from(value: RecordingModeParam) -> Self {
        match value {
            RecordingModeParam::Video => Self::Video,
            RecordingModeParam::Images => Self::Images,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct StartRecordingParams {
    session: String,
    /// Recording FPS; defaults to the session FPS and cannot exceed it.
    fps: Option<u32>,
    /// video creates an MP4; images creates chronological contact-sheet PNGs.
    mode: Option<RecordingModeParam>,
    /// Frames per contact sheet in images mode (default and maximum 10).
    frames_per_image: Option<u32>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SessionsResult {
    sessions: Vec<SessionInfo>,
}

#[derive(Clone)]
struct AutoscopeMcp {
    coordinator: Arc<Mutex<Coordinator>>,
}

impl AutoscopeMcp {
    fn new() -> Result<Self> {
        Ok(Self {
            coordinator: Arc::new(Mutex::new(Coordinator::new()?)),
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Coordinator>, String> {
        self.coordinator
            .lock()
            .map_err(|_| "MCP coordinator state is poisoned".into())
    }

    fn action(&self, session: &str, request: Request) -> Result<Json<ActionResult>, String> {
        self.lock()?
            .action(session, request)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    fn observed_action(&self, session: &str, view: u64, request: Request) -> CallToolResult {
        match self.lock().and_then(|mut coordinator| {
            coordinator
                .observed_action(session, view, request)
                .map_err(|error| error.to_string())
        }) {
            Ok((action, capture)) => observation_result(Some(action), capture),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        }
    }

    fn observed_spawn(&self, application: Application, options: SessionOptions) -> CallToolResult {
        match self.lock().and_then(|mut coordinator| {
            let info = coordinator
                .spawn(application, options)
                .map_err(|error| error.to_string())?;
            match coordinator.screenshot(&info.session) {
                Ok(capture) => Ok(capture),
                Err(error) => {
                    let _ = coordinator.close(&info.session);
                    Err(error.to_string())
                }
            }
        }) {
            Ok(capture) => observation_result(None, capture),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        }
    }

    fn recording_result(&self, session: &str) -> CallToolResult {
        let recording = match self.lock().and_then(|mut coordinator| {
            coordinator
                .stop_recording(session)
                .map_err(|error| error.to_string())
        }) {
            Ok(recording) => recording,
            Err(error) => return CallToolResult::error(vec![ContentBlock::text(error)]),
        };
        let mode = match recording.mode {
            RecordingMode::Video => "video",
            RecordingMode::Images => "images",
        };
        let ordering = if recording.mode == RecordingMode::Images {
            " Contact-sheet frames read left-to-right, then top-to-bottom."
        } else {
            ""
        };
        let mut content = vec![ContentBlock::text(format!(
            "Recorded {} frames at {} FPS as {mode}.{ordering}",
            recording.frames, recording.fps,
        ))];
        if recording.mode == RecordingMode::Images {
            for (index, path) in recording.files.iter().enumerate() {
                let bytes = match std::fs::read(path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        return CallToolResult::error(vec![ContentBlock::text(format!(
                            "read contact sheet {}: {error}",
                            path.display()
                        ))]);
                    }
                };
                content.push(ContentBlock::text(format!(
                    "Contact sheet {} of {} ({})",
                    index + 1,
                    recording.files.len(),
                    path.display()
                )));
                content.push(ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                    "image/png",
                ));
            }
        } else if let Some(path) = recording.files.first() {
            content.push(ContentBlock::text(format!("MP4: {}", path.display())));
        }
        let mut result = CallToolResult::success(content);
        result.structured_content = match serde_json::to_value(recording) {
            Ok(recording) => Some(recording),
            Err(error) => {
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "encode recording result: {error}"
                ))]);
            }
        };
        result
    }
}

fn observation_result(action: Option<ActionResult>, capture: Capture) -> CallToolResult {
    let session = capture.info.session.clone();
    let wait = action
        .as_ref()
        .and_then(|action| action.result["wait"]["status"].as_str())
        .map(|status| format!(" Visual wait: {status}."))
        .unwrap_or_default();
    let mut result = CallToolResult::success(vec![
        ContentBlock::text(format!(
            "{session} is at view {} (frame {}).{wait} Use view={} for the next input tool. Shared files: {} (same path on host and in app; retained after exit).",
            capture.view,
            capture.frame,
            capture.view,
            capture.info.shared_dir.display(),
        )),
        ContentBlock::image(
            base64::engine::general_purpose::STANDARD.encode(capture.bytes),
            "image/png",
        ),
    ]);
    let mut structured = serde_json::to_value(capture.info).expect("serialize session info");
    if let Value::Object(fields) = &mut structured {
        fields.insert("frame".into(), capture.frame.into());
        fields.insert("view".into(), capture.view.into());
        if let Some(action) = action {
            fields.insert("result".into(), action.result);
        }
    }
    result.structured_content = Some(structured);
    result
}

#[tool_router]
impl AutoscopeMcp {
    #[tool(
        description = "Spawn a host command, allow bounded visual settling of its first window, and return its screenshot and view number"
    )]
    fn spawn_command(&self, Parameters(params): Parameters<SpawnCommandParams>) -> CallToolResult {
        let application = Application::Command {
            argv: params.command,
            sandbox: params.sandbox.unwrap_or(SandboxMode::Auto),
        };
        self.observed_spawn(application, params.options.unwrap_or_default())
    }

    #[tool(
        description = "Spawn a Flatpak application, allow bounded visual settling of its first window, and return its screenshot and view number"
    )]
    fn spawn_flatpak(&self, Parameters(params): Parameters<SpawnFlatpakParams>) -> CallToolResult {
        let application = Application::Flatpak {
            id: params.application_id,
            args: params.arguments.unwrap_or_default(),
        };
        self.observed_spawn(application, params.options.unwrap_or_default())
    }

    #[tool(description = "List active application sessions owned by this MCP server")]
    fn list_sessions(&self) -> Result<Json<SessionsResult>, String> {
        self.lock()?
            .list()
            .map(|sessions| Json(SessionsResult { sessions }))
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Get display, cursor, surface, frame, and recording state for a session")]
    fn session_info(
        &self,
        Parameters(params): Parameters<SessionParam>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(&params.session, Request::Info)
    }

    #[tool(
        description = "Move from the supplied observed view, wait for hover UI by default, and return the resulting screenshot and view"
    )]
    fn move_pointer(&self, Parameters(params): Parameters<MoveParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Move {
                x: params.x,
                y: params.y,
                normalize: params.normalize.unwrap_or(false),
                wait: params.wait.unwrap_or(true),
                view: Some(params.view),
            },
        )
    }

    #[tool(
        description = "Click only if the supplied observed view is still current, then wait for visual stability and return a new screenshot and view"
    )]
    fn click(&self, Parameters(params): Parameters<ClickParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Click {
                button: params.button.unwrap_or_else(|| "left".into()),
                x: params.x,
                y: params.y,
                normalize: params.normalize.unwrap_or(false),
                wait: true,
                view: Some(params.view),
            },
        )
    }

    #[tool(
        description = "Press or release a mouse button from the supplied observed view and return the resulting screenshot and view; use move_pointer wait=false between drag endpoints"
    )]
    fn mouse_button(&self, Parameters(params): Parameters<ButtonParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Button {
                button: params.button,
                pressed: params.pressed,
                view: Some(params.view),
            },
        )
    }

    #[tool(
        description = "Drag through a full path with one held button, then release, settle, and return a screenshot. All points are validated before input. Optional origin and scale map canvas pixels to the screen; for example origin=[235,149], scale=4 for a canvas at 400% zoom."
    )]
    fn drag(&self, Parameters(params): Parameters<DragParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Drag {
                path: params.path,
                wait: true,
                view: Some(params.view),
            },
        )
    }

    #[tool(
        description = "Scroll only if the supplied observed view is current, wait for visual stability, and return a new screenshot and view"
    )]
    fn scroll(&self, Parameters(params): Parameters<ScrollParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Scroll {
                dx: params.dx,
                dy: params.dy,
                wait: true,
                view: Some(params.view),
            },
        )
    }

    #[tool(
        description = "Type printable US-ASCII at UI-safe speed only if the supplied observed view is current, then return the settled screenshot and new view"
    )]
    fn type_text(&self, Parameters(params): Parameters<TextParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Type {
                text: params.text,
                wait: true,
                view: Some(params.view),
            },
        )
    }

    #[tool(
        description = "Press a key only if the supplied observed view is current, wait for visual stability, and return a new screenshot and view"
    )]
    fn press_key(&self, Parameters(params): Parameters<KeyParams>) -> CallToolResult {
        self.observed_action(
            &params.session,
            params.view,
            Request::Key {
                combo: params.combo,
                wait: true,
                view: Some(params.view),
            },
        )
    }

    #[tool(description = "Capture the current session frame and return it as an MCP image")]
    fn screenshot(&self, Parameters(params): Parameters<SessionParam>) -> CallToolResult {
        match self.lock().and_then(|mut coordinator| {
            coordinator
                .screenshot(&params.session)
                .map_err(|error| error.to_string())
        }) {
            Ok(capture) => observation_result(None, capture),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        }
    }

    #[tool(description = "Start an independently sampled MP4 or contact-sheet image recording")]
    fn start_recording(
        &self,
        Parameters(params): Parameters<StartRecordingParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.lock()?
            .start_recording(
                &params.session,
                params.fps,
                params.mode.map(Into::into).unwrap_or_default(),
                params.frames_per_image.unwrap_or(10),
            )
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Stop recording and return an MP4 path or chronological MCP image sheets")]
    fn stop_recording(&self, Parameters(params): Parameters<SessionParam>) -> CallToolResult {
        self.recording_result(&params.session)
    }

    #[tool(description = "Close an application session and reap all of its processes")]
    fn close_session(
        &self,
        Parameters(params): Parameters<SessionParam>,
    ) -> Result<Json<SessionInfo>, String> {
        self.lock()?
            .close(&params.session)
            .map(Json)
            .map_err(|error| error.to_string())
    }
}

#[tool_handler]
impl ServerHandler for AutoscopeMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("autoscope", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Spawn allows bounded visual settling, then returns a screenshot, session ID, and view number. Every input tool requires that latest view, waits for bounded visual stability by default, and returns the resulting screenshot and next view; stale or concurrently planned actions are rejected before injection. Re-read each returned image before choosing another coordinate, and treat wait status timed-out as an instruction to observe or wait again. Text is paced at UI-safe speed. Absolute pixels are the coordinate default; set normalize=true for 0.0 through 1.0. Use drag for a whole held-button path, with optional origin and scale for local canvas coordinates; move_pointer wait=false remains available for raw drag steps. Each session reports shared_dir, a writable folder at the same absolute path on host and in the app; place inputs and save exports there. Recordings select FPS independently, and images mode returns chronological contact sheets. Sessions close automatically when this server exits; shared files, logs, and recordings are retained.",
            )
    }
}

pub fn run() -> Result<()> {
    let server = AutoscopeMcp::new()?;
    let coordinator = server.coordinator.clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .context("create MCP async runtime")?;
    let result = runtime.block_on(async move {
        let service = server
            .serve(stdio())
            .await
            .context("start MCP stdio server")?;
        service.waiting().await.context("run MCP stdio server")?;
        Ok(())
    });
    match coordinator.lock() {
        Ok(mut coordinator) => coordinator.shutdown_all(),
        Err(poisoned) => poisoned.into_inner().shutdown_all(),
    }
    result
}
