use std::time::Duration;

use anyhow::{Result, anyhow};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Offscreen, damage::OutputDamageTracker,
            element::solid::SolidColorRenderElement, pixman::PixmanRenderer,
        },
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        pixman::Image,
    },
    utils::{Buffer, Rectangle, Transform},
};

use crate::state::Autoscope;

pub struct Backend {
    renderer: PixmanRenderer,
    target: Image<'static, 'static>,
    size: (i32, i32),
}

pub fn create_headless_backend(width: i32, height: i32) -> Result<Backend> {
    let mut renderer = PixmanRenderer::new().map_err(|error| anyhow!(error.to_string()))?;
    let target = renderer
        .create_buffer(Fourcc::Abgr8888, (width, height).into())
        .map_err(|error| anyhow!(error.to_string()))?;
    Ok(Backend {
        renderer,
        target,
        size: (width, height),
    })
}

pub fn install(
    event_loop: &mut EventLoop<Autoscope>,
    state: &mut Autoscope,
    mut backend: Backend,
    fps: u32,
) -> Result<()> {
    let mode = Mode {
        size: backend.size.into(),
        refresh: (fps * 1000) as i32,
    };
    let output = Output::new(
        "autoscope".into(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "autoscope".into(),
            model: "virtual-agent-output".into(),
            serial_number: "1".into(),
        },
    );
    output.create_global::<Autoscope>(&state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Normal),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    state.space.map_output(&output, (0, 0));

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let frame_duration = Duration::from_nanos(1_000_000_000 / u64::from(fps));
    event_loop
        .handle()
        .insert_source(Timer::immediate(), move |_, _, state| {
            state.process_control();
            state.poll_app();
            match draw(&mut backend, &output, &mut damage_tracker, state) {
                Ok(frame) => state.publish_frame(frame),
                Err(error) => {
                    tracing::error!(%error, "render failed");
                    state.loop_signal.stop();
                }
            }
            state.space.elements().for_each(|window| {
                window.send_frame(
                    &output,
                    state.start_time.elapsed(),
                    Some(frame_duration),
                    |_, _| Some(output.clone()),
                )
            });
            state.space.refresh();
            state.popups.cleanup();
            let _ = state.display_handle.flush_clients();
            TimeoutAction::ToDuration(frame_duration)
        })
        .map_err(|error| anyhow!(error.to_string()))?;
    Ok(())
}

fn draw(
    backend: &mut Backend,
    output: &Output,
    damage_tracker: &mut OutputDamageTracker,
    state: &Autoscope,
) -> Result<Vec<u8>> {
    let mut framebuffer = backend.renderer.bind(&mut backend.target)?;
    let cursor = state.cursor_elements();
    smithay::desktop::space::render_output::<_, SolidColorRenderElement, _, _>(
        output,
        &mut backend.renderer,
        &mut framebuffer,
        1.0,
        0,
        [&state.space],
        &cursor,
        damage_tracker,
        [0.08, 0.09, 0.11, 1.0],
    )?;
    let mapping = backend.renderer.copy_framebuffer(
        &framebuffer,
        Rectangle::<i32, Buffer>::from_size(backend.size.into()),
        Fourcc::Abgr8888,
    )?;
    Ok(backend.renderer.map_texture(&mapping)?.to_vec())
}
