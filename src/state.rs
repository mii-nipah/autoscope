use std::{
    ffi::OsString,
    io::Write,
    os::unix::net::{UnixListener, UnixStream},
    process::Child,
    sync::{Arc, mpsc::Receiver},
    time::Instant,
};

use serde_json::json;
use smithay::{
    backend::{
        input::{Axis, AxisSource, ButtonState, InputTime},
        renderer::element::{
            Kind,
            solid::{SolidColorBuffer, SolidColorRenderElement},
        },
    },
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{
        Seat, SeatState,
        keyboard::XkbConfig,
        pointer::{AxisFrame, ButtonEvent, MotionEvent},
    },
    reexports::{
        calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        output::OutputManagerState,
        selection::data_device::DataDeviceState,
        shell::xdg::XdgShellState,
        shm::ShmState,
        socket::ListeningSocketSource,
    },
};

use crate::control::{self, Envelope, Recorder, Request, Response, Viewer};

pub struct Autoscope {
    pub start_time: Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,
    pub space: Space<Window>,
    pub loop_signal: LoopSignal,
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,
    pub seat: Seat<Self>,
    pub size: (i32, i32),
    pub pointer: Point<f64, Logical>,
    pub app: Option<Child>,
    pub viewer: Option<Viewer>,
    control_rx: Receiver<Envelope>,
    stream_listener: UnixListener,
    streams: Vec<UnixStream>,
    recorder: Option<Recorder>,
    latest_frame: Option<Vec<u8>>,
    frame_seq: u64,
    fps: u32,
    cursor_black: SolidColorBuffer,
    cursor_white: SolidColorBuffer,
}

impl Autoscope {
    pub fn new(
        event_loop: &mut EventLoop<Self>,
        display: Display<Self>,
        control_rx: Receiver<Envelope>,
        stream_listener: UnixListener,
        size: (i32, i32),
        fps: u32,
    ) -> Self {
        let display_handle = display.handle();
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "autoscope");
        seat.add_keyboard(
            XkbConfig {
                layout: "us",
                ..Default::default()
            },
            200,
            25,
        )
        .expect("load US keymap");
        seat.add_pointer();

        let socket_name = init_wayland_listener(display, event_loop);
        Self {
            start_time: Instant::now(),
            socket_name,
            display_handle,
            space: Space::default(),
            loop_signal: event_loop.get_signal(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            _output_manager_state: output_manager_state,
            seat_state,
            data_device_state,
            popups: PopupManager::default(),
            seat,
            size,
            pointer: Point::from((f64::from(size.0) / 2.0, f64::from(size.1) / 2.0)),
            app: None,
            viewer: None,
            control_rx,
            stream_listener,
            streams: Vec::new(),
            recorder: None,
            latest_frame: None,
            frame_seq: 0,
            fps,
            cursor_black: SolidColorBuffer::new((17, 3), [0.0, 0.0, 0.0, 0.9]),
            cursor_white: SolidColorBuffer::new((13, 1), [1.0, 1.0, 1.0, 1.0]),
        }
    }

    pub fn surface_under(
        &self,
        pos: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space
            .element_under(pos)
            .and_then(|(window, location)| {
                window
                    .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, offset)| (surface, (offset + location).to_f64()))
            })
    }

    pub fn cursor_elements(&self) -> Vec<SolidColorRenderElement> {
        let x = self.pointer.x.round() as i32;
        let y = self.pointer.y.round() as i32;
        vec![
            SolidColorRenderElement::from_buffer(
                &self.cursor_white,
                (x - 6, y),
                1.0,
                1.0,
                Kind::Cursor,
            ),
            SolidColorRenderElement::from_buffer(
                &self.cursor_white,
                (x, y - 6),
                (1.0 / 13.0, 13.0),
                1.0,
                Kind::Cursor,
            ),
            SolidColorRenderElement::from_buffer(
                &self.cursor_black,
                (x - 8, y - 1),
                1.0,
                1.0,
                Kind::Cursor,
            ),
            SolidColorRenderElement::from_buffer(
                &self.cursor_black,
                (x - 1, y - 8),
                (3.0 / 17.0, 17.0 / 3.0),
                1.0,
                Kind::Cursor,
            ),
        ]
    }

    pub fn process_control(&mut self) {
        while let Ok(envelope) = self.control_rx.try_recv() {
            let response = self.handle_request(envelope.request);
            let _ = envelope.reply.send(response);
        }
    }

    fn handle_request(&mut self, request: Request) -> Response {
        let result = match request {
            Request::Info => Ok(json!({
                "size": [self.size.0, self.size.1],
                "fps": self.fps,
                "frame": self.frame_seq,
                "cursor": [self.pointer.x, self.pointer.y],
                "recording": self.recorder.as_ref().map(|r| &r.path),
                "surfaces": self.space.elements().count(),
            })),
            Request::Move { x, y } => self
                .move_pointer(x, y)
                .map(|_| json!({"cursor": [self.pointer.x, self.pointer.y]})),
            Request::Click { button, x, y } => (|| {
                match (x, y) {
                    (Some(x), Some(y)) => self.move_pointer(x, y)?,
                    (None, None) => {}
                    _ => return Err("click needs both x and y".into()),
                }
                self.pointer_button(&button, true)?;
                self.pointer_button(&button, false)?;
                Ok(json!({"clicked": button, "at": [self.pointer.x, self.pointer.y]}))
            })(),
            Request::Button { button, pressed } => self
                .pointer_button(&button, pressed)
                .map(|_| json!({"button": button, "pressed": pressed})),
            Request::Scroll { dx, dy } => {
                self.scroll(dx, dy);
                Ok(json!({"scrolled": [dx, dy]}))
            }
            Request::Type { text } => self
                .type_text(&text)
                .map(|_| json!({"typed": text.chars().count()})),
            Request::Key { combo } => self.key_combo(&combo).map(|_| json!({"key": combo})),
            Request::Screenshot { path } => self.screenshot(&path).map(|_| json!({"path": path})),
            Request::RecordStart { path } => {
                self.start_recording(&path).map(|_| json!({"path": path}))
            }
            Request::RecordStop => self.stop_recording().map(|path| json!({"path": path})),
            Request::Quit => {
                self.loop_signal.stop();
                Ok(json!({"stopping": true}))
            }
        };
        result
            .map(Response::success)
            .unwrap_or_else(Response::error)
    }

    fn move_pointer(&mut self, x: f64, y: f64) -> Result<(), String> {
        if !x.is_finite() || !y.is_finite() {
            return Err("coordinates must be finite".into());
        }
        self.pointer = Point::from((
            x.clamp(0.0, f64::from(self.size.0 - 1)),
            y.clamp(0.0, f64::from(self.size.1 - 1)),
        ));
        let pointer = self.seat.get_pointer().unwrap();
        pointer.motion(
            self,
            self.surface_under(self.pointer),
            &MotionEvent {
                location: self.pointer,
                serial: SERIAL_COUNTER.next_serial(),
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
        Ok(())
    }

    fn pointer_button(&mut self, name: &str, pressed: bool) -> Result<(), String> {
        let button = match name.to_ascii_lowercase().as_str() {
            "left" => 0x110,
            "right" => 0x111,
            "middle" => 0x112,
            _ => return Err(format!("unknown mouse button {name:?}")),
        };
        let serial = SERIAL_COUNTER.next_serial();
        let pointer = self.seat.get_pointer().unwrap();
        if pressed
            && !pointer.is_grabbed()
            && let Some((window, _)) = self
                .space
                .element_under(self.pointer)
                .map(|(w, p)| (w.clone(), p))
        {
            self.space.raise_element(&window, true);
            self.seat.get_keyboard().unwrap().set_focus(
                self,
                Some(window.toplevel().unwrap().wl_surface().clone()),
                serial,
            );
        }
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state: if pressed {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                },
                serial,
                time: InputTime::now(),
            },
        );
        pointer.frame(self);
        Ok(())
    }

    fn scroll(&mut self, dx: f64, dy: f64) {
        let mut frame = AxisFrame::new(InputTime::now()).source(AxisSource::Wheel);
        if dx != 0.0 {
            frame = frame.value(Axis::Horizontal, dx);
        }
        if dy != 0.0 {
            frame = frame.value(Axis::Vertical, dy);
        }
        let pointer = self.seat.get_pointer().unwrap();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    fn screenshot(&self, path: &std::path::Path) -> Result<(), String> {
        let frame = self
            .latest_frame
            .as_ref()
            .ok_or_else(|| "no frame rendered yet".to_string())?;
        control::write_png(path, self.size.0 as u32, self.size.1 as u32, frame)
            .map_err(|e| e.to_string())
    }

    fn start_recording(&mut self, path: &std::path::Path) -> Result<(), String> {
        if self.recorder.is_some() {
            return Err("a recording is already active".into());
        }
        self.recorder = Some(
            Recorder::start(path, self.size.0, self.size.1, self.fps).map_err(|e| e.to_string())?,
        );
        Ok(())
    }

    fn stop_recording(&mut self) -> Result<std::path::PathBuf, String> {
        self.recorder
            .take()
            .ok_or_else(|| "no recording is active".to_string())?
            .finish()
            .map_err(|e| e.to_string())
    }

    pub fn publish_frame(&mut self, frame: Vec<u8>) {
        self.frame_seq += 1;
        while let Ok((mut stream, _)) = self.stream_listener.accept() {
            let mut header = Vec::with_capacity(20);
            header.extend_from_slice(b"ASF1");
            header.extend_from_slice(&(self.size.0 as u32).to_le_bytes());
            header.extend_from_slice(&(self.size.1 as u32).to_le_bytes());
            header.extend_from_slice(&self.fps.to_le_bytes());
            header.extend_from_slice(&(frame.len() as u32).to_le_bytes());
            if stream.write_all(&header).is_ok()
                && stream
                    .set_write_timeout(Some(std::time::Duration::from_millis(50)))
                    .is_ok()
            {
                self.streams.push(stream);
            }
        }
        self.streams.retain_mut(|stream| {
            stream.write_all(&self.frame_seq.to_le_bytes()).is_ok()
                && stream.write_all(&frame).is_ok()
        });
        if self
            .viewer
            .as_mut()
            .is_some_and(|viewer| viewer.frame(&frame).is_err())
        {
            self.viewer.take().unwrap().stop();
        }
        if self
            .recorder
            .as_mut()
            .is_some_and(|recorder| recorder.frame(&frame).is_err())
        {
            tracing::error!("recorder stopped accepting frames");
            if let Some(recorder) = self.recorder.take() {
                let _ = recorder.finish();
            }
        }
        self.latest_frame = Some(frame);
    }

    pub fn poll_app(&mut self) {
        match self.app.as_mut().map(Child::try_wait) {
            Some(Ok(Some(status))) => {
                tracing::info!(%status, "application exited");
                self.loop_signal.stop();
            }
            Some(Err(error)) => {
                tracing::error!(%error, "failed to poll application");
                self.loop_signal.stop();
            }
            _ => {}
        }
    }

    pub fn shutdown(&mut self) {
        if let Some(recorder) = self.recorder.take() {
            let _ = recorder.finish();
        }
        if let Some(viewer) = self.viewer.take() {
            viewer.stop();
        }
        for window in self.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_close();
            }
        }
        let _ = self.display_handle.flush_clients();
        if let Some(mut app) = self.app.take() {
            let mut exited = false;
            for _ in 0..20 {
                if app.try_wait().ok().flatten().is_some() {
                    exited = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            if !exited {
                let _ = app.kill();
            }
            let _ = app.wait();
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

fn init_wayland_listener(
    display: Display<Autoscope>,
    event_loop: &mut EventLoop<Autoscope>,
) -> OsString {
    let socket = ListeningSocketSource::new_auto().expect("create Wayland socket");
    let name = socket.socket_name().to_os_string();
    let handle = event_loop.handle();
    handle
        .insert_source(socket, |stream, _, state| {
            state
                .display_handle
                .insert_client(stream, Arc::new(ClientState::default()))
                .unwrap();
        })
        .unwrap();
    handle
        .insert_source(
            Generic::new(display, Interest::READ, Mode::Level),
            |_, display, state| {
                unsafe {
                    display.get_mut().dispatch_clients(state).unwrap();
                }
                Ok(PostAction::Continue)
            },
        )
        .unwrap();
    name
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
