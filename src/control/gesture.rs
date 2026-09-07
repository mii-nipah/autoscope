use std::{sync::mpsc::Sender, thread, time::Duration};

use clap::Args;
use rmcp::schemars;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Envelope, Request, Response, button_code, pacing::dispatch};

#[derive(Debug, Args, Deserialize, Serialize, PartialEq, schemars::JsonSchema)]
pub(crate) struct DragPath {
    /// Mouse button held throughout the path (default left).
    #[arg(long, default_value = "left")]
    #[serde(default = "default_button")]
    pub button: String,
    /// Path vertices as X,Y pairs. Every point is checked before input begins.
    #[arg(required = true, num_args = 2.., value_parser = parse_point, allow_hyphen_values = true)]
    pub points: Vec<[f64; 2]>,
    /// Use 0.0..=1.0 frame coordinates instead of pixels.
    #[arg(long, conflicts_with_all = ["origin", "scale"])]
    #[serde(default)]
    pub normalize: bool,
    /// Screen position of local (0,0), for example --origin 235,149.
    #[arg(long, value_parser = parse_point, allow_hyphen_values = true)]
    #[serde(default)]
    pub origin: Option<[f64; 2]>,
    /// Screen pixels per local unit, for example 4 for a canvas at 400% zoom.
    #[arg(long, default_value_t = 1.0)]
    #[serde(default = "default_scale")]
    pub scale: f64,
}

fn default_button() -> String {
    "left".into()
}
fn default_scale() -> f64 {
    1.0
}

fn parse_point(value: &str) -> Result<[f64; 2], String> {
    let (x, y) = value.split_once(',').ok_or("use X,Y for a point")?;
    Ok([
        x.parse::<f64>().map_err(|e| e.to_string())?,
        y.parse::<f64>().map_err(|e| e.to_string())?,
    ])
}

impl DragPath {
    fn resolve(&self, size: (i32, i32)) -> Result<Vec<[f64; 2]>, String> {
        button_code(&self.button)?;
        if !(2..=4096).contains(&self.points.len()) {
            return Err("a drag needs between 2 and 4096 points".into());
        }
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err("scale must be finite and positive".into());
        }
        if self.normalize && (self.origin.is_some() || self.scale != 1.0) {
            return Err("normalize cannot be combined with origin or scale".into());
        }
        let [ox, oy] = self.origin.unwrap_or([0.0, 0.0]);
        self.points
            .iter()
            .map(|[x, y]| {
                let (x, y) = crate::state::pointer_coordinates(
                    size,
                    ox + x * self.scale,
                    oy + y * self.scale,
                    self.normalize,
                )?;
                Ok([x, y])
            })
            .collect()
    }
}

pub(super) fn execute(
    tx: &Sender<Envelope>,
    path: DragPath,
    view: Option<u64>,
) -> Result<Response, String> {
    let info = dispatch(tx, Request::Info)?;
    if !info.ok {
        return Ok(info);
    }
    let info = info.result.ok_or("info omitted display size")?;
    let size = (
        info["size"][0].as_i64().ok_or("info omitted width")? as i32,
        info["size"][1].as_i64().ok_or("info omitted height")? as i32,
    );
    let points = path.resolve(size)?;
    let move_to = |[x, y]: [f64; 2], view: Option<u64>| {
        dispatch(
            tx,
            Request::Move {
                x,
                y,
                normalize: false,
                wait: false,
                view,
            },
        )
    };
    let moved = move_to(points[0], view)?;
    if !moved.ok {
        return Ok(moved);
    }
    let button = |pressed| {
        dispatch(
            tx,
            Request::Button {
                button: path.button.clone(),
                pressed,
                view: None,
            },
        )
    };
    let pressed = button(true)?;
    if !pressed.ok {
        return Ok(pressed);
    }
    thread::sleep(Duration::from_millis(50));
    let movement = points.iter().skip(1).try_for_each(|point| {
        let response = move_to(*point, None)?;
        if response.ok {
            Ok(())
        } else {
            Err(response.error.unwrap_or_else(|| "drag interrupted".into()))
        }
    });
    // Even a failed intermediate move must release the held button.
    let released = button(false)?;
    if !released.ok {
        return Ok(released);
    }
    movement?;
    Ok(Response::success(
        json!({"dragged": path.button, "points": points.len(), "from": points[0], "to": points.last()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn path(points: Vec<[f64; 2]>) -> DragPath {
        DragPath {
            button: "left".into(),
            points,
            normalize: false,
            origin: None,
            scale: 1.0,
        }
    }

    #[test]
    fn local_canvas_coordinates_preserve_every_vertex() {
        let mut path = path(vec![[10.0, 20.0], [127.0, 127.0]]);
        path.origin = Some([235.0, 149.0]);
        path.scale = 4.0;
        assert_eq!(
            path.resolve((1280, 900)).unwrap(),
            [[275.0, 229.0], [743.0, 657.0]]
        );
        path.normalize = true;
        assert!(path.resolve((1280, 900)).is_err());
    }

    #[test]
    fn invalid_later_vertices_do_not_inject_any_input() {
        let (tx, rx) = mpsc::channel::<Envelope>();
        let worker = thread::spawn(move || {
            let request = rx.recv().unwrap();
            assert_eq!(request.request, Request::Info);
            request
                .reply
                .send(Response::success(json!({"size": [1280, 900]})))
                .unwrap();
            assert!(rx.recv().is_err());
        });
        assert!(execute(&tx, path(vec![[10.0, 20.0], [f64::NAN, 30.0]]), Some(7)).is_err());
        drop(tx);
        worker.join().unwrap();
    }

    #[test]
    fn interrupted_gesture_releases_the_mouse() {
        let (tx, rx) = mpsc::channel::<Envelope>();
        let worker = thread::spawn(move || {
            let mut events = Vec::new();
            while let Ok(envelope) = rx.recv() {
                let response = match &envelope.request {
                    Request::Info => Response::success(json!({"size": [1280, 900]})),
                    Request::Move { x, view, .. } if *x == 10.0 => {
                        assert_eq!(*view, Some(7));
                        Response::success(json!({}))
                    }
                    Request::Move { .. } => Response::error("app rejected movement"),
                    Request::Button { pressed, .. } => {
                        events.push(*pressed);
                        Response::success(json!({}))
                    }
                    _ => unreachable!(),
                };
                envelope.reply.send(response).unwrap();
            }
            events
        });
        assert!(execute(&tx, path(vec![[10.0, 20.0], [40.0, 50.0]]), Some(7)).is_err());
        drop(tx);
        assert_eq!(worker.join().unwrap(), [true, false]);
    }
}
