use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    desktop::{PopupKind, find_popup_root_surface, get_popup_toplevel_coords},
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DndGrabHandler, GrabType, Source},
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Resource,
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::SERIAL_COUNTER,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent,
            is_sync_subsurface, with_states,
        },
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        selection::{
            SelectionHandler,
            data_device::{
                DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
            },
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
    },
};

use crate::state::{Autowayland, ClientState};

impl CompositorHandler for Autowayland {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.space.elements().find(|window| {
                window
                    .toplevel()
                    .is_some_and(|top| top.wl_surface() == &root)
            }) {
                window.on_commit();
            }
        }
        handle_xdg_commit(self, surface);
    }
}

impl BufferHandler for Autowayland {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for Autowayland {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for Autowayland {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.size = Some(self.size.into());
            state.bounds = Some(self.size.into());
            state.states.set(xdg_toplevel::State::Maximized);
            state.states.set(xdg_toplevel::State::Activated);
        });
        let focus = surface.wl_surface().clone();
        let window = smithay::desktop::Window::new_wayland_window(surface);
        window.set_activated(true);
        self.space.map_element(window, (0, 0), true);
        self.seat.get_keyboard().unwrap().set_focus(
            self,
            Some(focus),
            SERIAL_COUNTER.next_serial(),
        );
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.configure_full_output(&surface, true);
    }
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.configure_full_output(&surface, true);
    }
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        self.configure_full_output(&surface, false);
    }
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.configure_full_output(&surface, true);
    }

    fn move_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }
    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
    }
    fn grab(
        &mut self,
        _surface: PopupSurface,
        _seat: wl_seat::WlSeat,
        _serial: smithay::utils::Serial,
    ) {
    }
}

impl Autowayland {
    fn configure_full_output(&mut self, surface: &ToplevelSurface, maximized: bool) {
        surface.with_pending_state(|state| {
            state.size = Some(self.size.into());
            if maximized {
                state.states.set(xdg_toplevel::State::Maximized);
            } else {
                state.states.set(xdg_toplevel::State::Fullscreen);
            }
        });
        surface.send_pending_configure();
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.space.elements().find(|window| {
            window
                .toplevel()
                .is_some_and(|top| top.wl_surface() == &root)
        }) else {
            return;
        };
        let Some(output) = self.space.outputs().next() else {
            return;
        };
        let mut target = self.space.output_geometry(output).unwrap();
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= self.space.element_geometry(window).unwrap().loc;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

fn handle_xdg_commit(state: &mut Autowayland, surface: &WlSurface) {
    if let Some(window) = state
        .space
        .elements()
        .find(|window| {
            window
                .toplevel()
                .is_some_and(|top| top.wl_surface() == surface)
        })
        .cloned()
    {
        let configured = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .unwrap()
                .lock()
                .unwrap()
                .initial_configure_sent
        });
        if !configured {
            window.toplevel().unwrap().send_configure();
        }
    }
    state.popups.commit(surface);
    if let Some(PopupKind::Xdg(popup)) = state.popups.find_popup(surface)
        && !popup.is_initial_configure_sent()
    {
        let _ = popup.send_configure();
    }
}

impl SeatHandler for Autowayland {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display_handle.get_client(surface.id()).ok());
        set_data_device_focus(&self.display_handle, seat, client);
    }
}

impl PointerConstraintsHandler for Autowayland {}
impl SelectionHandler for Autowayland {
    type SelectionUserData = ();
}
impl DataDeviceHandler for Autowayland {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}
impl DndGrabHandler for Autowayland {}
impl WaylandDndGrabHandler for Autowayland {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        _seat: Seat<Self>,
        _serial: smithay::utils::Serial,
        _kind: GrabType,
    ) {
        source.cancel();
    }
}
impl OutputHandler for Autowayland {}

smithay::delegate_dispatch2!(Autowayland);
