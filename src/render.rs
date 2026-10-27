use std::time::Duration;

use anyhow::{Result, anyhow};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            Bind, ExportMem, Frame, Offscreen, Renderer,
            damage::OutputDamageTracker,
            element::{Kind, memory::MemoryRenderBufferRenderElement},
            pixman::{PixmanRenderer, PixmanTarget},
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
    event_loop: &mut EventLoop<'static, Autoscope>,
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
                    state.stop(crate::session::ExitReason::Error {
                        message: format!("render failed: {error}"),
                    });
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
    backend.capture(|renderer, framebuffer| {
        let cursor = [MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (state.pointer.x.round(), state.pointer.y.round()),
            &state.cursor,
            Some(crate::cursor::ALPHA),
            None,
            None,
            Kind::Cursor,
        )?];
        smithay::desktop::space::render_output::<
            _,
            MemoryRenderBufferRenderElement<PixmanRenderer>,
            _,
            _,
        >(
            output,
            renderer,
            framebuffer,
            1.0,
            0,
            [&state.space],
            &cursor,
            damage_tracker,
            [0.08, 0.09, 0.11, 1.0],
        )?;
        Ok(())
    })
}

impl Backend {
    fn capture(
        &mut self,
        draw: impl FnOnce(&mut PixmanRenderer, &mut PixmanTarget<'_>) -> Result<()>,
    ) -> Result<Vec<u8>> {
        let mut framebuffer = self.renderer.bind(&mut self.target)?;
        // Each capture is a complete repaint (buffer age 0 below). Pixman still
        // blends alpha inside opaque regions, which the damage tracker skips when
        // clearing. Start from opaque black so undefined alpha in those regions
        // cannot accumulate the previous frame's RGB (e.g. native Wayland games).
        let mut frame =
            self.renderer
                .render(&mut framebuffer, self.size.into(), Transform::Normal)?;
        frame.clear(
            [0.0, 0.0, 0.0, 1.0].into(),
            &[Rectangle::from_size(self.size.into())],
        )?;
        frame.finish()?.wait()?;
        draw(&mut self.renderer, &mut framebuffer)?;
        let mapping = self.renderer.copy_framebuffer(
            &framebuffer,
            Rectangle::<i32, Buffer>::from_size(self.size.into()),
            Fourcc::Abgr8888,
        )?;
        Ok(self.renderer.map_texture(&mapping)?.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;

    #[test]
    fn opaque_pixels_ignore_alpha_without_accumulating_previous_frames() {
        let mut backend = create_headless_backend(2, 1).unwrap();
        let mut tracker = OutputDamageTracker::new((2, 1), 1.0, Transform::Normal);
        for alpha in [0, 64, 128, 255] {
            let buffer = MemoryRenderBuffer::from_slice(
                &[12, 34, 56, alpha, 64, 32, 0, 128],
                Fourcc::Abgr8888,
                (2, 1),
                1,
                Transform::Normal,
                Some(vec![Rectangle::from_size((1, 1).into())]),
            );
            for _ in 0..16 {
                let pixels = backend
                    .capture(|renderer, framebuffer| {
                        let element = MemoryRenderBufferRenderElement::from_buffer(
                            renderer,
                            (0.0, 0.0),
                            &buffer,
                            None,
                            None,
                            None,
                            Kind::Unspecified,
                        )?;
                        tracker.render_output(
                            renderer,
                            framebuffer,
                            0,
                            &[element],
                            [0.2, 0.4, 0.6, 1.0],
                        )?;
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(&pixels[..4], &[12, 34, 56, 255]);
                // The pixel outside the opaque region still blends over the background.
                for (actual, expected) in pixels[4..].iter().zip([89_u8, 83, 76, 255]) {
                    assert!(actual.abs_diff(expected) <= 1, "{pixels:?}");
                }
            }
        }
    }
}
