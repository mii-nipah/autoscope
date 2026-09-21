use std::{
    borrow::Cow,
    process::Stdio,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use smithay::{
    backend::input::{InputTime, KeyState},
    desktop::{Window, WindowSurface},
    input::{
        Seat,
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
    },
    reexports::{calloop::EventLoop, wayland_server::protocol::wl_surface::WlSurface},
    utils::{IsAlive, Logical, Rectangle, SERIAL_COUNTER, Serial, Size},
    wayland::{
        seat::WaylandFocus,
        xwayland_shell::{XWaylandShellHandler, XWaylandShellState},
    },
    xwayland::{
        X11Surface, X11Wm, XWayland, XwmHandler,
        xwm::{Reorder, ResizeEdge, XwmId},
    },
};

use crate::state::Autoscope;

pub enum X11State {
    Ready { _server: XWayland, wm: Box<X11Wm> },
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyboardFocus(Window);

impl From<Window> for KeyboardFocus {
    fn from(window: Window) -> Self {
        Self(window)
    }
}

impl KeyboardFocus {
    fn target(&self) -> &dyn KeyboardTarget<Autoscope> {
        match self.0.underlying_surface() {
            WindowSurface::Wayland(surface) => surface.wl_surface(),
            WindowSurface::X11(surface) => surface,
        }
    }
}

impl IsAlive for KeyboardFocus {
    fn alive(&self) -> bool {
        self.0.alive()
    }
}

impl WaylandFocus for KeyboardFocus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        self.0.wl_surface()
    }
}

impl KeyboardTarget<Autoscope> for KeyboardFocus {
    fn enter(
        &self,
        seat: &Seat<Autoscope>,
        state: &mut Autoscope,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        self.target().enter(seat, state, keys, serial);
    }

    fn leave(&self, seat: &Seat<Autoscope>, state: &mut Autoscope, serial: Serial) {
        self.target().leave(seat, state, serial);
    }

    fn key(
        &self,
        seat: &Seat<Autoscope>,
        state: &mut Autoscope,
        key: KeysymHandle<'_>,
        key_state: KeyState,
        serial: Serial,
        time: InputTime,
    ) {
        self.target().key(seat, state, key, key_state, serial, time);
    }

    fn modifiers(
        &self,
        seat: &Seat<Autoscope>,
        state: &mut Autoscope,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        self.target().modifiers(seat, state, modifiers, serial);
    }
}

pub fn start(event_loop: &mut EventLoop<'static, Autoscope>, state: &mut Autoscope) -> Result<u32> {
    let (mut xwayland, client) = XWayland::spawn(
        &state.display_handle,
        None,
        std::iter::empty::<(String, String)>(),
        std::iter::empty::<String>(),
        false,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    )
    .context("start private XWayland server")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        event_loop
            .dispatch(Some(Duration::from_millis(50)), state)
            .context("wait for XWayland readiness")?;
        match xwayland.take_socket() {
            Ok(Some(x11_socket)) => {
                let display = xwayland.display_number();
                let wm = X11Wm::start_wm(
                    event_loop.handle(),
                    &state.display_handle,
                    x11_socket,
                    client,
                )
                .map_err(|error| anyhow::anyhow!("start private X11 window manager: {error}"))?;
                state.x11 = X11State::Ready {
                    _server: xwayland,
                    wm: Box::new(wm),
                };
                return Ok(display);
            }
            Ok(None) if Instant::now() >= deadline => {
                bail!("XWayland did not become ready within 5 seconds")
            }
            Ok(None) => {}
            Err(error) => return Err(error).context("wait for XWayland readiness"),
        }
    }
}

pub fn stop(state: &mut Autoscope) {
    state.x11 = X11State::Stopped;
}

impl XWaylandShellHandler for Autoscope {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }

    fn surface_associated(&mut self, _xwm: XwmId, _surface: WlSurface, x11: X11Surface) {
        let window = self
            .space
            .elements()
            .find(|window| window.x11_surface() == Some(&x11))
            .cloned();
        if let Some(window) = window {
            window.on_commit();
            self.focus_window(window);
        }
    }
}

impl Autoscope {
    fn full_x11_geometry(&self) -> Rectangle<i32, Logical> {
        Rectangle::from_size(Size::from(self.size))
    }

    fn x11_geometry(&self, surface: &X11Surface) -> Rectangle<i32, Logical> {
        if surface.is_transient_for().is_none() && !surface.is_modal() {
            return self.full_x11_geometry();
        }
        let requested = surface.geometry().size;
        let size = Size::from((
            requested.w.clamp(1, self.size.0),
            requested.h.clamp(1, self.size.1),
        ));
        Rectangle::new(
            ((self.size.0 - size.w) / 2, (self.size.1 - size.h) / 2).into(),
            size,
        )
    }

    fn focus_window(&mut self, window: Window) {
        if window.wl_surface().is_some() {
            window.set_activated(true);
            self.seat.get_keyboard().unwrap().set_focus(
                self,
                Some(window.into()),
                SERIAL_COUNTER.next_serial(),
            );
        }
    }

    fn find_x11_window(&self, surface: &X11Surface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| window.x11_surface() == Some(surface))
            .cloned()
    }
}

impl XwmHandler for Autoscope {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        match &mut self.x11 {
            X11State::Ready { wm, .. } => wm,
            _ => panic!("X11 window manager requested before readiness"),
        }
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {}
    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        let geometry = self.x11_geometry(&surface);
        if surface.set_mapped(true).is_err() || surface.configure(Some(geometry)).is_err() {
            return;
        }
        let window = Window::new_x11_window(surface);
        self.space.map_element(window.clone(), geometry.loc, true);
        self.focus_window(window);
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        let location = surface.last_configure().loc;
        self.space
            .map_element(Window::new_x11_window(surface), location, true);
    }

    fn unmapped_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.find_x11_window(&surface) {
            self.space.unmap_elem(&window);
        }
        if !surface.is_override_redirect() {
            let _ = surface.set_mapped(false);
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, surface: X11Surface) {
        if let Some(window) = self.find_x11_window(&surface) {
            self.space.unmap_elem(&window);
        }
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        _x: Option<i32>,
        _y: Option<i32>,
        width: Option<u32>,
        height: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        let mut geometry = self.x11_geometry(&surface);
        if surface.is_transient_for().is_some() || surface.is_modal() {
            if let Some(width) = width {
                geometry.size.w = (width as i32).clamp(1, self.size.0);
            }
            if let Some(height) = height {
                geometry.size.h = (height as i32).clamp(1, self.size.1);
            }
            geometry.loc = (
                (self.size.0 - geometry.size.w) / 2,
                (self.size.1 - geometry.size.h) / 2,
            )
                .into();
        }
        let _ = surface.configure(Some(geometry));
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<u32>,
    ) {
        if let Some(window) = self.find_x11_window(&surface) {
            self.space.map_element(window, geometry.loc, false);
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        let _ = surface.configure(Some(self.full_x11_geometry()));
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, surface: X11Surface) {
        let _ = surface.configure(Some(self.full_x11_geometry()));
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        _window: X11Surface,
        _button: u32,
        _resize_edge: ResizeEdge,
    ) {
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {}

    fn active_window_request(
        &mut self,
        _xwm: XwmId,
        surface: X11Surface,
        _timestamp: u32,
        _currently_active_window: Option<X11Surface>,
    ) {
        if let Some(window) = self.find_x11_window(&surface) {
            self.focus_window(window);
        }
    }

    fn disconnected(&mut self, _xwm: XwmId) {
        tracing::error!("private XWayland server disconnected");
        self.stop(crate::session::ExitReason::Error {
            message: "private XWayland server disconnected".into(),
        });
    }
}
