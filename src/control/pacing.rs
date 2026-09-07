use std::{
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{
    Envelope, Request, Response, default_quiet_ms, default_timeout_ms,
    timing::{SettleStatus, VisualSettler},
};

pub(super) fn execute(tx: &Sender<Envelope>, request: Request) -> Result<Response, String> {
    const CLICK_PHASE: Duration = Duration::from_millis(50);
    const TYPE_INTERVAL: Duration = Duration::from_millis(25);

    let (response, wait) = match request {
        Request::Drag { path, wait, view } => (super::gesture::execute(tx, path, view)?, wait),
        Request::Move {
            x,
            y,
            normalize,
            wait,
            view,
        } => (
            dispatch(
                tx,
                Request::Move {
                    x,
                    y,
                    normalize,
                    wait: false,
                    view,
                },
            )?,
            wait,
        ),
        Request::Click {
            button,
            x,
            y,
            normalize,
            wait,
            view,
        } => {
            button_code(&button)?;
            let (button_view, at) = match (x, y) {
                (Some(x), Some(y)) => {
                    let moved = dispatch(
                        tx,
                        Request::Move {
                            x,
                            y,
                            normalize,
                            wait: false,
                            view,
                        },
                    )?;
                    if !moved.ok {
                        return Ok(moved);
                    }
                    let at = moved
                        .result
                        .as_ref()
                        .map(|result| result["cursor"].clone())
                        .unwrap_or(Value::Null);
                    thread::sleep(CLICK_PHASE);
                    (None, at)
                }
                (None, None) if !normalize => {
                    let info = dispatch(tx, Request::Info)?;
                    let at = info
                        .result
                        .as_ref()
                        .map(|result| result["cursor"].clone())
                        .unwrap_or(Value::Null);
                    (view, at)
                }
                (None, None) => return Err("normalize needs click coordinates".into()),
                _ => return Err("click needs both x and y".into()),
            };
            let pressed = dispatch(
                tx,
                Request::Button {
                    button: button.clone(),
                    pressed: true,
                    view: button_view,
                },
            )?;
            if !pressed.ok {
                return Ok(pressed);
            }
            thread::sleep(CLICK_PHASE);
            let released = dispatch(
                tx,
                Request::Button {
                    button: button.clone(),
                    pressed: false,
                    view: None,
                },
            )?;
            if !released.ok {
                return Ok(released);
            }
            (
                Response::success(json!({"clicked": button, "at": at})),
                wait,
            )
        }
        Request::Scroll { dx, dy, wait, view } => (
            dispatch(
                tx,
                Request::Scroll {
                    dx,
                    dy,
                    wait: false,
                    view,
                },
            )?,
            wait,
        ),
        Request::Type { text, wait, view } => {
            crate::input::validate_text(&text)?;
            let characters: Vec<_> = text.chars().collect();
            for (index, character) in characters.iter().copied().enumerate() {
                let typed = dispatch(
                    tx,
                    Request::Type {
                        text: character.to_string(),
                        wait: false,
                        view: if index == 0 { view } else { None },
                    },
                )?;
                if !typed.ok {
                    return Ok(typed);
                }
                if index + 1 < characters.len() {
                    thread::sleep(TYPE_INTERVAL);
                }
            }
            (Response::success(json!({"typed": characters.len()})), wait)
        }
        Request::Key { combo, wait, view } => (
            dispatch(
                tx,
                Request::Key {
                    combo,
                    wait: false,
                    view,
                },
            )?,
            wait,
        ),
        Request::Wait {
            timeout_ms,
            quiet_ms,
        } => {
            let timeout = timeout_ms.clamp(1, 30_000);
            return wait_until_stable(
                tx,
                Duration::from_millis(timeout),
                Duration::from_millis(quiet_ms.clamp(1, timeout)),
            )
            .map(Response::success);
        }
        request => return dispatch(tx, request),
    };
    if !wait || !response.ok {
        return Ok(response);
    }
    let observation = wait_until_stable(
        tx,
        Duration::from_millis(default_timeout_ms()),
        Duration::from_millis(default_quiet_ms()),
    )
    .unwrap_or_else(|error| json!({"status": "unavailable", "error": error}));
    Ok(with_wait(response, observation))
}

pub(super) fn dispatch(tx: &Sender<Envelope>, request: Request) -> Result<Response, String> {
    let (reply_tx, reply_rx) = mpsc::channel();
    tx.send(Envelope {
        request,
        reply: reply_tx,
    })
    .map_err(|_| "compositor stopped".to_string())?;
    reply_rx
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| "compositor did not answer".to_string())
}

fn wait_until_stable(
    tx: &Sender<Envelope>,
    timeout: Duration,
    quiet: Duration,
) -> Result<Value, String> {
    let start = Instant::now();
    let mut settler = VisualSettler::new(timeout, quiet);
    loop {
        let response = dispatch(tx, Request::Info)?;
        if !response.ok {
            return Err(response.error.unwrap_or_else(|| "info failed".into()));
        }
        let info = response.result.unwrap_or(Value::Null);
        let frame = info["frame"].as_u64().ok_or("info omitted frame")?;
        let view = info["view"].as_u64().ok_or("info omitted view")?;
        let elapsed = start.elapsed();
        if let Some(status) = settler.observe(elapsed, view) {
            return Ok(json!({
                "status": match status {
                    SettleStatus::Stable => "stable",
                    SettleStatus::TimedOut => "timed-out",
                },
                "elapsed_ms": elapsed.as_millis() as u64,
                "frame": frame,
                "view": view,
            }));
        }
    }
}

fn with_wait(mut response: Response, observation: Value) -> Response {
    if let Some(Value::Object(result)) = response.result.as_mut() {
        result.insert("wait".into(), observation);
    }
    response
}

pub(crate) fn button_code(name: &str) -> Result<u32, String> {
    match name.to_ascii_lowercase().as_str() {
        "left" => Ok(0x110),
        "right" => Ok(0x111),
        "middle" => Ok(0x112),
        _ => Err(format!("unknown mouse button {name:?}")),
    }
}
