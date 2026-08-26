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

use crate::control::Request;
use coordinator::{
    ActionResult, Application, Coordinator, SandboxMode, SessionInfo, SessionOptions,
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
    x: f64,
    y: f64,
    /// Interpret x and y as 0.0 through 1.0 instead of absolute pixels.
    normalize: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ClickParams {
    session: String,
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
    /// left, right, or middle.
    button: String,
    /// true presses and holds; false releases.
    pressed: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ScrollParams {
    session: String,
    /// Horizontal wheel amount.
    dx: f64,
    /// Vertical wheel amount.
    dy: f64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TextParams {
    session: String,
    text: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct KeyParams {
    session: String,
    /// Named key or modifier combination, for example ENTER or CTRL+L.
    combo: String,
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
}

#[tool_router]
impl AutoscopeMcp {
    #[tool(
        description = "Spawn a host command and wait for its first window in a new isolated autoscope session"
    )]
    fn spawn_command(
        &self,
        Parameters(params): Parameters<SpawnCommandParams>,
    ) -> Result<Json<SessionInfo>, String> {
        let application = Application::Command {
            argv: params.command,
            sandbox: params.sandbox.unwrap_or(SandboxMode::Auto),
        };
        self.lock()?
            .spawn(application, params.options.unwrap_or_default())
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(
        description = "Spawn an installed Flatpak application and wait for its first window in a new isolated autoscope session"
    )]
    fn spawn_flatpak(
        &self,
        Parameters(params): Parameters<SpawnFlatpakParams>,
    ) -> Result<Json<SessionInfo>, String> {
        let application = Application::Flatpak {
            id: params.application_id,
            args: params.arguments.unwrap_or_default(),
        };
        self.lock()?
            .spawn(application, params.options.unwrap_or_default())
            .map(Json)
            .map_err(|error| error.to_string())
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
        description = "Move the mouse using absolute pixels or normalized 0.0 through 1.0 coordinates"
    )]
    fn move_pointer(
        &self,
        Parameters(params): Parameters<MoveParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(
            &params.session,
            Request::Move {
                x: params.x,
                y: params.y,
                normalize: params.normalize.unwrap_or(false),
            },
        )
    }

    #[tool(description = "Click the left, right, or middle mouse button, optionally at a position")]
    fn click(
        &self,
        Parameters(params): Parameters<ClickParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(
            &params.session,
            Request::Click {
                button: params.button.unwrap_or_else(|| "left".into()),
                x: params.x,
                y: params.y,
                normalize: params.normalize.unwrap_or(false),
            },
        )
    }

    #[tool(description = "Press or release a mouse button; combine with move_pointer to drag")]
    fn mouse_button(
        &self,
        Parameters(params): Parameters<ButtonParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(
            &params.session,
            Request::Button {
                button: params.button,
                pressed: params.pressed,
            },
        )
    }

    #[tool(description = "Send horizontal and vertical mouse wheel input")]
    fn scroll(
        &self,
        Parameters(params): Parameters<ScrollParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(
            &params.session,
            Request::Scroll {
                dx: params.dx,
                dy: params.dy,
            },
        )
    }

    #[tool(description = "Type printable US-ASCII text into the focused application")]
    fn type_text(
        &self,
        Parameters(params): Parameters<TextParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(&params.session, Request::Type { text: params.text })
    }

    #[tool(description = "Press a named key or modifier combination such as ENTER or CTRL+L")]
    fn press_key(
        &self,
        Parameters(params): Parameters<KeyParams>,
    ) -> Result<Json<ActionResult>, String> {
        self.action(
            &params.session,
            Request::Key {
                combo: params.combo,
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
            Ok((info, bytes)) => CallToolResult::success(vec![
                ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                    "image/png",
                ),
                ContentBlock::text(format!(
                    "Screenshot of {} at {}x{}",
                    info.session, info.width, info.height
                )),
            ]),
            Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
        }
    }

    #[tool(description = "Start recording session frames to a coordinator-managed MP4 file")]
    fn start_recording(
        &self,
        Parameters(params): Parameters<SessionParam>,
    ) -> Result<Json<ActionResult>, String> {
        self.lock()?
            .start_recording(&params.session)
            .map(Json)
            .map_err(|error| error.to_string())
    }

    #[tool(description = "Stop recording and return the completed MP4 file path")]
    fn stop_recording(
        &self,
        Parameters(params): Parameters<SessionParam>,
    ) -> Result<Json<ActionResult>, String> {
        self.lock()?
            .stop_recording(&params.session)
            .map(Json)
            .map_err(|error| error.to_string())
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
                "Spawn an application first, retain its returned session ID, then use that ID for input and capture tools. Screenshots return MCP image content. Absolute pixels are the coordinate default; set normalize=true for 0.0 through 1.0 coordinates. Sessions are closed automatically when this MCP server exits.",
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
