use std::{
    fs::OpenOptions,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, ensure};
use smithay::{
    backend::{
        allocator::{Fourcc, dmabuf::Dmabuf},
        egl::{EGLContext, EGLDevice, EGLDisplay},
        renderer::{
            Bind, ExportMem, Frame, ImportAll, ImportDma, ImportMem, Offscreen, Texture,
            damage::OutputDamageTracker,
            element::{
                Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
            },
            gles::{GlesRenderer, GlesTexture},
            pixman::PixmanRenderer,
        },
    },
    desktop::{Space, Window},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        pixman::Image,
    },
    utils::{Buffer, Rectangle, Transform},
    wayland::dmabuf::{DmabufFeedback, DmabufFeedbackBuilder},
};

use crate::state::Autoscope;

pub(crate) struct Backend {
    renderer: HeadlessRenderer,
    size: (i32, i32),
}

enum HeadlessRenderer {
    Software(Capture<PixmanRenderer, Image<'static, 'static>>),
    Gpu(Box<GpuRenderer>),
}

struct GpuRenderer {
    capture: Capture<GlesRenderer, GlesTexture>,
    node: PathBuf,
    feedback: DmabufFeedback,
}

struct Capture<R, T> {
    renderer: R,
    target: T,
}

impl Backend {
    pub(crate) fn new(size: (i32, i32), no_gpu: bool) -> Result<Self> {
        if no_gpu {
            tracing::info!("GPU rendering disabled for this session");
        } else {
            match GpuRenderer::detect(size) {
                Ok(gpu) => {
                    tracing::info!(node = %gpu.node.display(), "using GPU rendering");
                    return Ok(Self {
                        renderer: HeadlessRenderer::Gpu(Box::new(gpu)),
                        size,
                    });
                }
                Err(error) => {
                    tracing::info!(%error, "GPU rendering unavailable, using software rendering");
                }
            }
        }
        let renderer = HeadlessRenderer::Software(Capture::new(PixmanRenderer::new()?, size)?);
        Ok(Self { renderer, size })
    }

    pub(crate) fn size(&self) -> (i32, i32) {
        self.size
    }

    pub(crate) fn render_node(&self) -> Option<&Path> {
        match &self.renderer {
            HeadlessRenderer::Gpu(gpu) => Some(&gpu.node),
            HeadlessRenderer::Software(_) => None,
        }
    }

    pub(crate) fn dmabuf_feedback(&self) -> Option<&DmabufFeedback> {
        match &self.renderer {
            HeadlessRenderer::Gpu(gpu) => Some(&gpu.feedback),
            HeadlessRenderer::Software(_) => None,
        }
    }

    pub(crate) fn import_dmabuf(&mut self, buffer: &Dmabuf) -> Result<()> {
        match &mut self.renderer {
            HeadlessRenderer::Gpu(gpu) => {
                gpu.capture.renderer.import_dmabuf(buffer, None)?;
                Ok(())
            }
            HeadlessRenderer::Software(_) => anyhow::bail!("GPU buffer support is disabled"),
        }
    }

    fn draw(
        &mut self,
        output: &Output,
        tracker: &mut OutputDamageTracker,
        space: &Space<Window>,
        cursor: &MemoryRenderBuffer,
        pointer: (f64, f64),
    ) -> Result<Vec<u8>> {
        match &mut self.renderer {
            HeadlessRenderer::Gpu(gpu) => gpu.capture.draw(output, tracker, space, cursor, pointer),
            HeadlessRenderer::Software(capture) => {
                capture.draw(output, tracker, space, cursor, pointer)
            }
        }
    }
}

impl GpuRenderer {
    fn detect(size: (i32, i32)) -> Result<Self> {
        // Smithay's EGL loader assumes the library is installed. Keep it optional.
        let _egl = unsafe { libloading::Library::new("libEGL.so.1") }.context("load system EGL")?;
        let mut failure = anyhow!("no accessible GPU render device");
        for device in EGLDevice::enumerate()? {
            let Ok(node) = device.render_device_path() else {
                continue;
            };
            match Self::new(device, node, size) {
                Ok(renderer) => return Ok(renderer),
                Err(error) => failure = error,
            }
        }
        Err(failure)
    }

    fn new(device: EGLDevice, node: PathBuf, size: (i32, i32)) -> Result<Self> {
        let render_node = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&node)
            .with_context(|| format!("open GPU render device {}", node.display()))?;
        let display = unsafe { EGLDisplay::new(device)? };
        let context = EGLContext::new(&display)?;
        // The newly created context belongs to this compositor thread.
        let renderer = unsafe { GlesRenderer::new(context)? };
        ensure!(!renderer.is_software(), "EGL selected a software renderer");
        let formats = renderer.dmabuf_formats();
        ensure!(
            formats.iter().next().is_some(),
            "GPU cannot import Wayland buffers"
        );
        let feedback =
            DmabufFeedbackBuilder::new(render_node.metadata()?.rdev(), formats).build()?;
        let mut capture = Capture::new(renderer, size)?;
        capture
            .capture(|_, _| Ok(()))
            .context("read a frame from the GPU")?;
        Ok(Self {
            capture,
            node,
            feedback,
        })
    }
}

pub(crate) fn install(
    event_loop: &mut EventLoop<'static, Autoscope>,
    state: &mut Autoscope,
    fps: u32,
) -> Result<()> {
    let mode = Mode {
        size: state.backend.size().into(),
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
            match state.backend.draw(
                &output,
                &mut damage_tracker,
                &state.space,
                &state.cursor,
                (state.pointer.x.round(), state.pointer.y.round()),
            ) {
                Ok(frame) => state.publish_frame(frame),
                Err(error) => state.capture_failed(error),
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

impl<R, T> Capture<R, T>
where
    R: Offscreen<T> + ExportMem,
    R::Error: Send + Sync + 'static,
{
    fn new(mut renderer: R, size: (i32, i32)) -> Result<Self> {
        let target = renderer.create_buffer(Fourcc::Abgr8888, size.into())?;
        Ok(Self { renderer, target })
    }
}

impl<R, T> Capture<R, T>
where
    R: Bind<T> + ExportMem,
    R::Error: Send + Sync + 'static,
{
    fn capture(
        &mut self,
        draw: impl FnOnce(&mut R, &mut R::Framebuffer<'_>) -> Result<()>,
    ) -> Result<Vec<u8>> {
        let mut framebuffer = self.renderer.bind(&mut self.target)?;
        let size = framebuffer.size();
        // Each capture is a complete repaint (buffer age 0 below). Pixman still
        // blends alpha inside opaque regions, which the damage tracker skips when
        // clearing. Start from opaque black so undefined alpha in those regions
        // cannot accumulate the previous frame's RGB (e.g. native Wayland games).
        let mut frame =
            self.renderer
                .render(&mut framebuffer, (size.w, size.h).into(), Transform::Normal)?;
        frame.clear(
            [0.0, 0.0, 0.0, 1.0].into(),
            &[Rectangle::from_size((size.w, size.h).into())],
        )?;
        frame.finish()?.wait()?;
        draw(&mut self.renderer, &mut framebuffer)?;
        let mapping = self.renderer.copy_framebuffer(
            &framebuffer,
            Rectangle::<i32, Buffer>::from_size(size),
            Fourcc::Abgr8888,
        )?;
        Ok(self.renderer.map_texture(&mapping)?.to_vec())
    }
}

impl<R, T> Capture<R, T>
where
    R: Bind<T> + ExportMem + ImportAll + ImportMem,
    R::TextureId: Clone + Send + 'static,
    R::Error: Send + Sync + 'static,
{
    fn draw(
        &mut self,
        output: &Output,
        tracker: &mut OutputDamageTracker,
        space: &Space<Window>,
        cursor: &MemoryRenderBuffer,
        pointer: (f64, f64),
    ) -> Result<Vec<u8>> {
        self.capture(|renderer, framebuffer| {
            let cursor = [MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                pointer,
                cursor,
                Some(crate::cursor::ALPHA),
                None,
                None,
                Kind::Cursor,
            )?];
            smithay::desktop::space::render_output(
                output,
                renderer,
                framebuffer,
                1.0,
                0,
                [space],
                &cursor,
                tracker,
                [0.08, 0.09, 0.11, 1.0],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::element::memory::MemoryRenderBuffer;

    #[test]
    fn opaque_pixels_ignore_alpha_without_accumulating_previous_frames() {
        let mut backend: Capture<_, Image<'static, 'static>> =
            Capture::new(PixmanRenderer::new().unwrap(), (2, 1)).unwrap();
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
