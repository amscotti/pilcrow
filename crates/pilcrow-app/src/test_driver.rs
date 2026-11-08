//! Test-only local control channel for the real Pilcrow window.
//!
//! The protocol types are always compiled so their tests run in a normal
//! `cargo test --workspace`; the Unix socket and GPUI wiring only exist behind
//! the `test-driver` feature. See `docs/TESTING.md` §5.
//!
//! Security properties:
//!
//! - the listener is a Unix socket created with mode `0600`;
//! - every request must carry the random token from `PILCROW_TEST_TOKEN`;
//! - the token is never logged, echoed, or included in an error message;
//! - there is no shell, no network listener, and no arbitrary file access.
//!
//! Deliberately unsupported for now: `screenshot` and the app/file commands
//! (`open`, `save`, `resize`, `click`, ...). They return `unsupported`; the
//! hello capabilities list stays honest. `frame` is a limited last-frame
//! snapshot; `trace.start`/`trace.stop` collect frame records (with
//! `input_seq`, frame timing, and a paint hash) from the editor's trace ring.
//! `scroll` is vertical only.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Commands this driver actually implements. Keep this list honest: a command
/// that returns `unsupported` is not a capability. `frame` is a limited
/// last-frame snapshot; the trace commands return the full frame records.
pub const CAPABILITIES: &[&str] = &[
    "hello",
    "status",
    "text",
    "caret",
    "selection",
    "lines",
    "frame",
    "type",
    "key",
    "chord",
    "scroll",
    "trace.start",
    "trace.stop",
    "quit",
];

/// The advertised capabilities as an owned list suitable for JSON.
pub fn capabilities() -> Vec<&'static str> {
    CAPABILITIES.to_vec()
}

/// A protocol error body: `{"code": "...", "message": "..."}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

impl ErrorBody {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// One newline-delimited response.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(ErrorBody::new(code, message)),
        }
    }

    /// The exact wire form: one JSON object plus a trailing newline.
    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

/// A validated request. The token is intentionally absent: it is checked at
/// the socket boundary and never travels further into the app.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: Value,
    pub cmd: String,
    pub args: Value,
}

#[derive(Deserialize)]
struct WireRequest {
    #[serde(default)]
    id: Value,
    #[serde(default)]
    token: String,
    #[serde(default)]
    cmd: String,
    #[serde(default)]
    args: Value,
}

/// The result of decoding one request line.
#[derive(Clone, Debug, PartialEq)]
pub enum Decoded {
    Request(Request),
    Reject { response: Response, close: bool },
}

/// Parse and authenticate one newline-delimited request.
///
/// A wrong or missing token is rejected before `cmd` or `args` are inspected,
/// and the connection is closed so a caller cannot brute-force the token.
pub fn decode_line(line: &str, expected_token: &str) -> Decoded {
    let line = line.trim_end_matches(['\r', '\n']);
    let wire: WireRequest = match serde_json::from_str(line) {
        Ok(wire) => wire,
        Err(_) => {
            return Decoded::Reject {
                response: Response::error(
                    Value::Null,
                    "invalid_request",
                    "request is not valid JSON",
                ),
                close: false,
            };
        }
    };

    if !constant_time_eq(wire.token.as_bytes(), expected_token.as_bytes()) {
        return Decoded::Reject {
            response: Response::error(wire.id, "unauthorized", "invalid token"),
            close: true,
        };
    }

    let cmd = wire.cmd.trim();
    if cmd.is_empty() {
        return Decoded::Reject {
            response: Response::error(wire.id, "invalid_request", "cmd is required"),
            close: false,
        };
    }

    let args = match wire.args {
        Value::Null => json!({}),
        Value::Object(_) => wire.args,
        _ => {
            return Decoded::Reject {
                response: Response::error(wire.id, "invalid_request", "args must be an object"),
                close: false,
            };
        }
    };

    Decoded::Request(Request {
        id: wire.id,
        cmd: cmd.to_owned(),
        args,
    })
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

/// Build a GPUI chord from `key` plus a modifier list.
pub fn key_chord(name: &str, mods: &[String]) -> Result<String, ErrorBody> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ErrorBody::new("invalid_args", "key name is required"));
    }

    let mut parts = Vec::with_capacity(mods.len() + 1);
    for modifier in mods {
        let modifier = modifier.trim().to_ascii_lowercase();
        let normalized = match modifier.as_str() {
            "cmd" | "command" | "platform" | "meta" | "super" => "cmd",
            "ctrl" | "control" => "ctrl",
            "alt" | "option" | "opt" => "alt",
            "shift" => "shift",
            "fn" | "function" => "fn",
            _ => {
                return Err(ErrorBody::new(
                    "invalid_args",
                    format!("unknown modifier {modifier:?}"),
                ));
            }
        };
        parts.push(normalized.to_owned());
    }
    parts.push(name.to_owned());
    Ok(parts.join("-"))
}

/// Clamp a byte range to UTF-8 boundaries for the `text` command.
pub fn text_slice(text: &str, start: Option<usize>, end: Option<usize>) -> Result<&str, ErrorBody> {
    let len = text.len();
    let start = start.unwrap_or(0).min(len);
    let end = end.unwrap_or(len).min(len);
    if start > end {
        return Err(ErrorBody::new(
            "invalid_args",
            "start must be less than or equal to end",
        ));
    }
    let start = pilcrow_core::boundary_at_or_before(text, start);
    let end = pilcrow_core::boundary_at_or_before(text, end);
    Ok(&text[start..end])
}

/// One line read from the driver socket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineRead {
    Eof,
    Line,
    TooLong,
}

/// Read one newline-terminated line without ever buffering more than `max`
/// bytes. A line that crosses the cap is reported immediately; the caller must
/// send an error and close the connection.
pub fn read_line_limited(
    reader: &mut impl std::io::BufRead,
    max: usize,
    buffer: &mut Vec<u8>,
) -> std::io::Result<LineRead> {
    buffer.clear();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if buffer.is_empty() {
                LineRead::Eof
            } else {
                LineRead::Line
            });
        }

        let (take, newline) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (available.len(), false),
        };
        if buffer.len() + take > max {
            return Ok(LineRead::TooLong);
        }

        buffer.extend_from_slice(&available[..take]);
        reader.consume(take);
        if newline {
            return Ok(LineRead::Line);
        }
    }
}

#[cfg(all(feature = "test-driver", unix))]
mod runtime {
    use std::io::{BufReader, BufWriter, Write};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::time::Duration;

    use async_channel::{Sender, bounded};
    use gpui::{
        AnyWindowHandle, App, Entity, EntityInputHandler, Focusable, Keystroke, Window, px,
    };
    use pilcrow_editor::{EditorView, FrameRecord};
    use serde_json::{Value, json};

    use super::{
        Decoded, ErrorBody, LineRead, Request, Response, capabilities, decode_line, key_chord,
        read_line_limited, text_slice,
    };

    const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;
    const COMMAND_QUEUE: usize = 64;

    /// The artifact path from the latest `trace.start`, echoed by
    /// `trace.stop`. The driver itself never writes it: records travel in the
    /// `trace.stop` response and the client owns artifact files.
    fn trace_path_cell() -> &'static std::sync::Mutex<Option<String>> {
        static CELL: std::sync::OnceLock<std::sync::Mutex<Option<String>>> =
            std::sync::OnceLock::new();
        CELL.get_or_init(|| std::sync::Mutex::new(None))
    }

    fn note_input(editor: &Entity<EditorView>, cx: &mut App) {
        editor.update(cx, |editor, _cx| editor.note_driver_input());
    }

    struct Envelope {
        request: Request,
        reply: Sender<Reply>,
    }

    struct Reply {
        response: Response,
        quit_after_write: bool,
        written: Option<Sender<()>>,
    }

    /// Start the socket listener and the foreground command task. Called only
    /// from `open_note_window` when the feature and environment flag are on.
    pub fn install(cx: &mut App, window: AnyWindowHandle, note: Entity<crate::NoteWindow>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        if INSTALLED.swap(true, Ordering::SeqCst) {
            eprintln!("pilcrow: test driver is single-window; ignoring a second note window");
            return;
        }
        let Some(socket_path) =
            std::env::var_os("PILCROW_TEST_SOCKET").map(std::path::PathBuf::from)
        else {
            eprintln!("pilcrow: PILCROW_TEST_SOCKET is not set; test driver stays off");
            return;
        };
        if socket_path.as_os_str().is_empty() {
            eprintln!("pilcrow: PILCROW_TEST_SOCKET is empty; test driver stays off");
            return;
        }
        let token = match std::env::var("PILCROW_TEST_TOKEN") {
            Ok(token) if !token.is_empty() => token,
            _ => {
                eprintln!("pilcrow: PILCROW_TEST_TOKEN is not set; test driver stays off");
                return;
            }
        };

        let (commands, receiver) = bounded::<Envelope>(COMMAND_QUEUE);
        if let Err(error) = start_listener(&socket_path, token, commands) {
            eprintln!("pilcrow: test driver did not start: {error}");
            return;
        }

        let task = cx.spawn(async move |cx| {
            while let Ok(envelope) = receiver.recv().await {
                let is_quit = envelope.request.cmd == "quit";
                let response = match window.update(cx, |_root, window, cx| {
                    dispatch(window, cx, &note, &envelope.request)
                }) {
                    Ok(Ok(result)) => Response::success(envelope.request.id.clone(), result),
                    Ok(Err(error)) => {
                        Response::error(envelope.request.id.clone(), error.code, error.message)
                    }
                    Err(error) => Response::error(
                        envelope.request.id.clone(),
                        "internal",
                        format!("window update failed: {error}"),
                    ),
                };

                let (written_tx, written_rx) = if is_quit {
                    let (tx, rx) = bounded(1);
                    (Some(tx), Some(rx))
                } else {
                    (None, None)
                };
                let reply = Reply {
                    response,
                    quit_after_write: is_quit,
                    written: written_tx,
                };
                if envelope.reply.send_blocking(reply).is_err() {
                    break;
                }

                if is_quit {
                    // Do not quit until the background thread has flushed the
                    // response; the client is allowed to read it before close.
                    if let Some(written) = written_rx {
                        let _ = written.recv().await;
                    }
                    cx.update(|cx| cx.quit());
                    return;
                }
            }
        });
        task.detach();
    }

    fn start_listener(
        path: &Path,
        token: String,
        commands: Sender<Envelope>,
    ) -> std::io::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && !parent.is_dir()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("socket directory does not exist: {}", parent.display()),
            ));
        }

        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("refusing to replace non-socket path {}", path.display()),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;

        std::thread::Builder::new()
            .name("pilcrow-test-driver".to_owned())
            .spawn(move || accept_loop(listener, token, commands))
            .map(|_| ())
    }

    fn accept_loop(listener: UnixListener, token: String, commands: Sender<Envelope>) {
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    let token = token.clone();
                    let commands = commands.clone();
                    let _ = std::thread::Builder::new()
                        .name("pilcrow-test-driver-conn".to_owned())
                        .spawn(move || serve_connection(stream, token, commands));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    fn serve_connection(stream: UnixStream, token: String, commands: Sender<Envelope>) {
        let Ok(reader_stream) = stream.try_clone() else {
            return;
        };
        let mut reader = BufReader::new(reader_stream);
        let mut writer = BufWriter::new(stream);
        let mut buffer = Vec::new();

        loop {
            match read_line_limited(&mut reader, MAX_LINE_BYTES, &mut buffer) {
                Ok(LineRead::Eof) => break,
                Ok(LineRead::Line) => {}
                Ok(LineRead::TooLong) => {
                    let response = Response::error(
                        Value::Null,
                        "invalid_request",
                        "request line is too large",
                    );
                    let _ = write_response(&mut writer, &response);
                    break;
                }
                Err(_) => break,
            }

            let Ok(line) = std::str::from_utf8(&buffer) else {
                let response =
                    Response::error(Value::Null, "invalid_request", "request is not valid UTF-8");
                let _ = write_response(&mut writer, &response);
                continue;
            };

            match decode_line(line, &token) {
                Decoded::Reject { response, close } => {
                    let _ = write_response(&mut writer, &response);
                    if close {
                        break;
                    }
                }
                Decoded::Request(request) => {
                    let (reply_tx, reply_rx) = bounded(1);
                    if commands
                        .send_blocking(Envelope {
                            request,
                            reply: reply_tx,
                        })
                        .is_err()
                    {
                        let response =
                            Response::error(Value::Null, "internal", "the driver is not running");
                        let _ = write_response(&mut writer, &response);
                        break;
                    }

                    let Ok(reply) = reply_rx.recv_blocking() else {
                        break;
                    };
                    let Reply {
                        response,
                        quit_after_write,
                        written,
                    } = reply;
                    let wrote = write_response(&mut writer, &response).is_ok();
                    if wrote && let Some(written) = written {
                        let _ = written.send_blocking(());
                    }
                    if quit_after_write {
                        break;
                    }
                }
            }
        }
    }

    fn write_response(writer: &mut impl Write, response: &Response) -> std::io::Result<()> {
        let bytes = response.to_bytes().map_err(std::io::Error::other)?;
        writer.write_all(&bytes)?;
        writer.flush()
    }

    fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a Value, ErrorBody> {
        args.get(name)
            .ok_or_else(|| ErrorBody::new("invalid_args", format!("missing {name:?}")))
    }

    fn arg_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, ErrorBody> {
        arg(args, name)?
            .as_str()
            .ok_or_else(|| ErrorBody::new("invalid_args", format!("{name:?} must be a string")))
    }

    fn arg_opt_usize(args: &Value, name: &str) -> Result<Option<usize>, ErrorBody> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .map(|value| Some(value as usize))
                .ok_or_else(|| {
                    ErrorBody::new(
                        "invalid_args",
                        format!("{name:?} must be a non-negative integer"),
                    )
                }),
        }
    }

    fn arg_f32(args: &Value, name: &str) -> Result<f32, ErrorBody> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(0.0),
            Some(value) => value.as_f64().map(|value| value as f32).ok_or_else(|| {
                ErrorBody::new("invalid_args", format!("{name:?} must be a number"))
            }),
        }
    }

    fn arg_strings(args: &Value, name: &str) -> Result<Vec<String>, ErrorBody> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(values)) => values
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).ok_or_else(|| {
                        ErrorBody::new(
                            "invalid_args",
                            format!("{name:?} must be an array of strings"),
                        )
                    })
                })
                .collect(),
            Some(_) => Err(ErrorBody::new(
                "invalid_args",
                format!("{name:?} must be an array of strings"),
            )),
        }
    }

    fn unsupported(cmd: &str) -> ErrorBody {
        ErrorBody::new(
            "unsupported",
            format!("the {cmd:?} command is not implemented by this driver"),
        )
    }

    pub(super) fn dispatch(
        window: &mut Window,
        cx: &mut App,
        note: &Entity<crate::NoteWindow>,
        request: &Request,
    ) -> Result<Value, ErrorBody> {
        let editor = note.read(cx).editor().clone();
        match request.cmd.as_str() {
            "hello" => Ok(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
                "capabilities": capabilities(),
                // One socket drives the first window; a second note window is
                // refused rather than silently stealing the route.
                "single_window": true,
            })),
            "status" => Ok(status(window, cx, note)),
            "frame" => Ok(frame(cx, &editor)),
            "text" => {
                let text = editor.read(cx).text();
                let start = arg_opt_usize(&request.args, "start")?;
                let end = arg_opt_usize(&request.args, "end")?;
                let slice = text_slice(text, start, end)?;
                Ok(json!({ "text": slice }))
            }
            "caret" => Ok(caret(cx, &editor)),
            "selection" => {
                let editor = editor.read(cx);
                let caret = editor.caret();
                let (start, end) = editor
                    .selection()
                    .map_or((caret, caret), |range| (range.start, range.end));
                Ok(json!({ "start": start, "end": end }))
            }
            "lines" => {
                let editor = editor.read(cx);
                let layout_done = editor.layout_done();
                let revision = editor.revision();
                let frame_seq = editor.frame_seq();
                let lines = if layout_done {
                    editor
                        .visible_lines()
                        .into_iter()
                        .map(|line| {
                            json!({
                                "line": line.line,
                                "start": line.start,
                                "end": line.end,
                                "x": line.x,
                                "y": line.y,
                                "h": line.h,
                                "rule": line.rule,
                            })
                        })
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                Ok(json!({
                    "lines": lines,
                    "stale": !layout_done,
                    "layout_done": layout_done,
                    "revision": revision,
                    "frame_seq": frame_seq,
                }))
            }
            "type" => {
                let text = arg_str(&request.args, "text")?;
                focus_editor(&editor, window, cx);
                editor.update(cx, |editor, cx| {
                    editor.replace_text_in_range(None, text, window, cx);
                });
                note_input(&editor, cx);
                Ok(status(window, cx, note))
            }
            "key" => {
                let name = arg_str(&request.args, "name")?;
                let mods = arg_strings(&request.args, "mods")?;
                let chord = key_chord(name, &mods)?;
                dispatch_keystroke(&chord, window, cx, &editor, note)
            }
            "chord" => {
                let chord = arg_str(&request.args, "chord")?;
                if chord.trim().is_empty() {
                    return Err(ErrorBody::new("invalid_args", "chord is required"));
                }
                dispatch_keystroke(chord, window, cx, &editor, note)
            }
            "scroll" => {
                let dx = arg_f32(&request.args, "dx")?;
                let dy = arg_f32(&request.args, "dy")?;
                if dx != 0.0 {
                    return Err(unsupported("scroll.dx"));
                }
                let applied = editor.update(cx, |editor, cx| {
                    let applied = editor.scroll_by(dy);
                    if applied {
                        cx.notify();
                    }
                    applied
                });
                if applied {
                    note_input(&editor, cx);
                }
                if !applied {
                    return Err(ErrorBody::new(
                        "not_ready",
                        "the editor has no viewport yet; retry after layout",
                    ));
                }
                Ok(status(window, cx, note))
            }
            "trace.start" => {
                let path = match request.args.get("path") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(path)) => Some(path.clone()),
                    Some(_) => {
                        return Err(ErrorBody::new("invalid_args", "\"path\" must be a string"));
                    }
                };
                if let Ok(mut cell) = trace_path_cell().lock() {
                    *cell = path.clone();
                }
                editor.update(cx, |editor, cx| {
                    editor.trace_start();
                    cx.notify();
                });
                Ok(json!({ "tracing": true, "path": path }))
            }
            "trace.stop" => {
                let records = editor.update(cx, |editor, _cx| editor.trace_stop());
                let frames: Vec<Value> = records.iter().map(frame_record_value).collect();
                let path = trace_path_cell()
                    .lock()
                    .ok()
                    .and_then(|mut cell| cell.take());
                Ok(json!({ "frames": frames, "path": path }))
            }
            "quit" => Ok(json!({ "quitting": true })),
            "screenshot" | "trace" | "perf.reset" | "open" | "save" | "resize" | "click"
            | "focus" | "menu" | "hit_test" => Err(unsupported(&request.cmd)),
            _ => Err(ErrorBody::new(
                "not_found",
                format!("unknown command {:?}", request.cmd),
            )),
        }
    }

    fn focus_editor(editor: &Entity<EditorView>, window: &mut Window, cx: &mut App) {
        let focus = editor.read(cx).focus_handle(cx);
        focus.focus(window, cx);
    }

    fn dispatch_keystroke(
        chord: &str,
        window: &mut Window,
        cx: &mut App,
        editor: &Entity<EditorView>,
        note: &Entity<crate::NoteWindow>,
    ) -> Result<Value, ErrorBody> {
        if chord.trim().is_empty() {
            return Err(ErrorBody::new("invalid_args", "chord is required"));
        }
        let keystroke = Keystroke::parse_with_os(chord, cx.platform_info().os)
            .map_err(|error| ErrorBody::new("invalid_args", error.to_string()))?;
        focus_editor(editor, window, cx);
        let handled = window.dispatch_keystroke(keystroke, cx);
        note_input(editor, cx);
        let mut status = status(window, cx, note);
        status["handled"] = json!(handled);
        Ok(status)
    }

    fn status(window: &Window, cx: &App, note: &Entity<crate::NoteWindow>) -> Value {
        let note = note.read(cx);
        let editor = note.editor().read(cx);
        let caret = editor.caret();
        let anchor = editor.anchor();
        let lines = editor.visible_lines();
        let stats = editor.stats();
        let viewport = editor
            .viewport()
            .map(|(width, height)| json!({ "w": width, "h": height }))
            .unwrap_or(Value::Null);
        json!({
            "title": note.window_title(cx),
            "platform_title": window.window_title(),
            "dirty": note.is_dirty(cx),
            "path": note.path().map(|path| path.to_string_lossy().into_owned()),
            "identity": note.identity.as_ref().map(|path| path.to_string_lossy().into_owned()),
            "error": note.error,
            "conflict": note.conflict,
            "saving": note.save.in_flight,
            "save_pending": note.save.pending.is_some(),
            "focused": editor.focus_handle(cx).is_focused(window),
            "text_len": editor.text().len(),
            "revision": editor.revision(),
            "caret": caret,
            "anchor": anchor,
            "scroll": editor.scroll(),
            "viewport": viewport,
            "layout_done": editor.layout_done(),
            "visible_lines": lines.len(),
            "frame_seq": editor.frame_seq(),
            "stats": {
                "words": stats.words,
                "chars": stats.chars,
                "reading_seconds": stats.reading_seconds,
            },
        })
    }

    fn caret(cx: &App, editor: &Entity<EditorView>) -> Value {
        let editor = editor.read(cx);
        let offset = editor.caret();
        let rect = editor.caret_rect();
        let visible = rect.is_some();
        let rect = rect.map(caret_rect_value);
        json!({
            "offset": offset,
            "x": rect.as_ref().and_then(|rect| rect.get("x")).cloned().unwrap_or(Value::Null),
            "y": rect.as_ref().and_then(|rect| rect.get("y")).cloned().unwrap_or(Value::Null),
            "w": rect.as_ref().and_then(|rect| rect.get("w")).cloned().unwrap_or(Value::Null),
            "h": rect.as_ref().and_then(|rect| rect.get("h")).cloned().unwrap_or(Value::Null),
            "visible": visible,
            "rect": rect,
            "layout_done": editor.layout_done(),
            "revision": editor.revision(),
            "frame_seq": editor.frame_seq(),
        })
    }

    fn frame(cx: &App, editor: &Entity<EditorView>) -> Value {
        let editor = editor.read(cx);
        let caret = editor.caret();
        let caret_rect = editor.caret_rect().map(caret_rect_value);
        json!({
            "seq": editor.frame_seq(),
            "revision": editor.revision(),
            "caret": caret,
            "anchor": editor.anchor(),
            "scroll": editor.scroll(),
            "layout_done": editor.layout_done(),
            "caret_rect": caret_rect,
            "visible_lines": editor.visible_lines().len(),
        })
    }

    fn frame_record_value(record: &FrameRecord) -> Value {
        let caret_rect = record
            .caret_rect
            .map(|(x, y, w, h)| json!({ "x": x, "y": y, "w": w, "h": h }));
        json!({
            "seq": record.seq,
            "input_seq": record.input_seq,
            "revision": record.revision,
            "caret": record.caret,
            "anchor": record.anchor,
            "scroll": record.scroll,
            "layout_done": record.layout_done,
            "caret_visible": caret_rect.is_some(),
            "caret_rect": caret_rect,
            "frame_ms": record.frame_ms,
            "paint_hash": record.paint_hash,
        })
    }

    fn caret_rect_value(rect: gpui::Bounds<gpui::Pixels>) -> Value {
        json!({
            "x": rect.origin.x / px(1.0),
            "y": rect.origin.y / px(1.0),
            "w": rect.size.width / px(1.0),
            "h": rect.size.height / px(1.0),
        })
    }
}

#[cfg(feature = "test-driver")]
use gpui::{AnyWindowHandle, App, Entity};

/// Install the test driver for one window. Inert unless the `test-driver`
/// feature is enabled *and* `PILCROW_TEST_DRIVER=1` is set by the caller.
#[cfg(feature = "test-driver")]
pub fn install(cx: &mut App, window: AnyWindowHandle, note: Entity<crate::NoteWindow>) {
    #[cfg(unix)]
    {
        runtime::install(cx, window, note);
    }
    #[cfg(not(unix))]
    {
        let _ = (cx, window, note);
        tracing::warn!("the Pilcrow test driver is not supported on this platform");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const TOKEN: &str = "0123456789abcdef";

    fn wire(id: u64, token: &str, cmd: &str, args: Value) -> String {
        serde_json::to_string(&json!({
            "id": id,
            "token": token,
            "cmd": cmd,
            "args": args,
        }))
        .expect("serialize request")
    }

    fn request(decoded: Decoded) -> Request {
        match decoded {
            Decoded::Request(request) => request,
            Decoded::Reject { response, .. } => {
                panic!("expected a request, got {response:?}")
            }
        }
    }

    #[test]
    fn accepts_a_matching_token_and_preserves_the_id() {
        let request = request(decode_line(&wire(7, TOKEN, "status", json!({})), TOKEN));
        assert_eq!(request.id, json!(7));
        assert_eq!(request.cmd, "status");
        assert_eq!(request.args, json!({}));
    }

    #[test]
    fn rejects_a_wrong_token_without_echoing_it() {
        let decoded = decode_line(&wire(3, "not-the-token", "status", json!({})), TOKEN);
        let Decoded::Reject { response, close } = decoded else {
            panic!("wrong token was accepted");
        };
        assert!(close);
        assert_eq!(response.id, json!(3));
        assert!(!response.ok);
        let error = response.error.as_ref().expect("error body");
        assert_eq!(error.code, "unauthorized");
        let wire = String::from_utf8(response.to_bytes().expect("serialize")).expect("utf8");
        assert!(!wire.contains("not-the-token"));
        assert!(!wire.contains(TOKEN));
    }

    #[test]
    fn rejects_a_missing_token() {
        let decoded = decode_line(r#"{"id":1,"cmd":"status","args":{}}"#, TOKEN);
        let Decoded::Reject { response, close } = decoded else {
            panic!("missing token was accepted");
        };
        assert!(close);
        assert_eq!(response.error.expect("error body").code, "unauthorized");
    }

    #[test]
    fn rejects_malformed_json_without_an_id() {
        let decoded = decode_line("{not json", TOKEN);
        let Decoded::Reject { response, close } = decoded else {
            panic!("invalid JSON was accepted");
        };
        assert!(!close);
        assert_eq!(response.id, Value::Null);
        assert_eq!(response.error.expect("error body").code, "invalid_request");
    }

    #[test]
    fn requires_a_command() {
        let decoded = decode_line(&wire(1, TOKEN, "  ", json!({})), TOKEN);
        let Decoded::Reject { response, .. } = decoded else {
            panic!("empty command was accepted");
        };
        assert_eq!(response.error.expect("error body").code, "invalid_request");
    }

    #[test]
    fn null_args_become_an_empty_object() {
        let request = request(decode_line(
            r#"{"id":1,"token":"0123456789abcdef","cmd":"hello","args":null}"#,
            TOKEN,
        ));
        assert_eq!(request.args, json!({}));
    }

    #[test]
    fn response_shapes_match_the_protocol() {
        let success = Response::success(json!(1), json!({"ok": true}));
        let wire: Value =
            serde_json::from_slice(&success.to_bytes().expect("serialize")).expect("parse");
        assert_eq!(wire["id"], json!(1));
        assert_eq!(wire["ok"], json!(true));
        assert_eq!(wire["result"]["ok"], json!(true));
        assert!(wire.get("error").is_none());

        let failure = Response::error(json!(2), "not_found", "no such command");
        let wire: Value =
            serde_json::from_slice(&failure.to_bytes().expect("serialize")).expect("parse");
        assert_eq!(wire["id"], json!(2));
        assert_eq!(wire["ok"], json!(false));
        assert_eq!(wire["error"]["code"], json!("not_found"));
        assert_eq!(wire["error"]["message"], json!("no such command"));
        assert!(wire.get("result").is_none());
    }

    #[test]
    fn capabilities_are_honest_about_the_smoke_commands() {
        for command in [
            "hello",
            "status",
            "text",
            "caret",
            "selection",
            "lines",
            "type",
            "key",
            "chord",
            "trace.start",
            "trace.stop",
            "quit",
        ] {
            assert!(CAPABILITIES.contains(&command), "missing {command}");
        }
        assert!(!CAPABILITIES.contains(&"screenshot"));
    }

    #[test]
    fn key_chords_accept_common_modifier_aliases() {
        assert_eq!(key_chord("b", &["cmd".to_owned()]).expect("chord"), "cmd-b");
        assert_eq!(
            key_chord("left", &["shift".to_owned(), "alt".to_owned()]).expect("chord"),
            "shift-alt-left"
        );
        assert_eq!(key_chord("end", &[]).expect("chord"), "end");
        assert_eq!(
            key_chord("a", &["bogus".to_owned()])
                .expect_err("bad modifier")
                .code,
            "invalid_args"
        );
        assert_eq!(
            key_chord("", &[]).expect_err("empty key").code,
            "invalid_args"
        );
    }

    #[test]
    fn text_slices_clamp_to_utf8_boundaries() {
        let text = "e\u{301}x";
        assert_eq!(text_slice(text, None, None).expect("all"), text);
        assert_eq!(text_slice(text, Some(0), Some(1)).expect("grapheme"), "e");
        assert_eq!(text_slice(text, Some(99), None).expect("clamped"), "");
        assert_eq!(
            text_slice(text, Some(3), Some(1))
                .expect_err("reversed")
                .code,
            "invalid_args"
        );
    }

    #[test]
    fn line_reader_stops_at_the_cap_before_buffering_the_rest() {
        let mut reader = Cursor::new(vec![b'a'; 32]);
        let mut buffer = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, 16, &mut buffer).expect("read"),
            LineRead::TooLong
        );
        assert!(buffer.len() <= 16);
        assert!(reader.position() <= 16);
    }

    #[test]
    fn line_reader_rejects_a_newline_that_crosses_the_cap() {
        let mut reader = Cursor::new(b"0123456789\n".to_vec());
        let mut buffer = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, 10, &mut buffer).expect("read"),
            LineRead::TooLong
        );
    }

    #[test]
    fn line_reader_accepts_a_line_at_the_cap_and_a_partial_eof() {
        let mut reader = Cursor::new(b"012345678\n".to_vec());
        let mut buffer = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, 10, &mut buffer).expect("read"),
            LineRead::Line
        );
        assert_eq!(buffer, b"012345678\n");

        let mut reader = Cursor::new(b"partial".to_vec());
        let mut buffer = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, 10, &mut buffer).expect("read"),
            LineRead::Line
        );
        assert_eq!(buffer, b"partial");

        let mut reader = Cursor::new(Vec::new());
        let mut buffer = Vec::new();
        assert_eq!(
            read_line_limited(&mut reader, 10, &mut buffer).expect("read"),
            LineRead::Eof
        );
    }

    #[test]
    fn constant_time_comparison_is_length_safe() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"short", b"longer"));
        assert!(constant_time_eq(b"", b""));
    }
}

#[cfg(all(test, feature = "test-driver", unix))]
mod runtime_tests {
    use super::*;
    use crate::NoteWindow;
    use gpui::{AppContext, TestAppContext, VisualTestContext};
    use pilcrow_editor::{install_fonts, install_keybindings};
    use pilcrow_ui::theme::light;

    fn request(id: u64, cmd: &str, args: Value) -> Request {
        Request {
            id: json!(id),
            cmd: cmd.to_owned(),
            args,
        }
    }

    #[gpui::test]
    fn type_key_status_and_caret_use_the_real_editor(cx: &mut TestAppContext) {
        cx.update(install_fonts);
        cx.update(install_keybindings);
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| NoteWindow::new(cx, "hi", light(), None))
                })
            })
            .expect("open note window");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let note = window.root(&mut cx).expect("note root");
        let editor = note.read_with(&cx, |note, _| note.editor().clone());

        let status = cx
            .update(|window, cx| {
                runtime::dispatch(window, cx, &note, &request(1, "status", json!({})))
            })
            .expect("status");
        assert_eq!(status["text_len"], json!(2));
        assert_eq!(status["caret"], json!(0));
        assert_eq!(status["title"], json!("hi"));
        assert_eq!(status["dirty"], json!(false));
        assert!(status["path"].is_null(), "an untitled note has no path");
        assert!(
            status["anchor"].is_null(),
            "no selection must report a null anchor"
        );
        assert_eq!(status["layout_done"], json!(true));

        let lines = cx
            .update(|window, cx| {
                runtime::dispatch(window, cx, &note, &request(2, "lines", json!({})))
            })
            .expect("lines");
        assert_eq!(lines["layout_done"], json!(true));
        assert!(lines["revision"].is_number(), "{}", lines);
        assert!(lines["frame_seq"].is_number(), "{}", lines);
        let first = &lines["lines"][0];
        assert!(first.get("line").is_some(), "{}", first);
        assert!(first.get("block").is_none(), "{}", first);
        for key in ["start", "end", "x", "y", "h", "rule"] {
            assert!(first.get(key).is_some(), "{key} missing from {first}");
        }

        let key = cx
            .update(|window, cx| {
                runtime::dispatch(
                    window,
                    cx,
                    &note,
                    &request(3, "key", json!({"name": "end"})),
                )
            })
            .expect("key");
        assert_eq!(key["handled"], json!(true));
        assert_eq!(key["caret"], json!(2));

        let typed = cx
            .update(|window, cx| {
                runtime::dispatch(window, cx, &note, &request(4, "type", json!({"text": "!"})))
            })
            .expect("type");
        assert_eq!(typed["text_len"], json!(3));
        assert_eq!(typed["caret"], json!(3));
        assert_eq!(typed["dirty"], json!(true));
        assert_eq!(
            editor.read_with(&cx, |editor, _| editor.text().to_owned()),
            "hi!"
        );

        let caret = cx
            .update(|window, cx| {
                runtime::dispatch(window, cx, &note, &request(5, "caret", json!({})))
            })
            .expect("caret");
        assert_eq!(caret["offset"], json!(3));
        assert!(caret["revision"].is_number(), "{}", caret);
        assert!(caret["frame_seq"].is_number(), "{}", caret);
        assert!(caret.get("layout_done").is_some(), "{}", caret);

        let select_all = if cfg!(target_os = "macos") {
            "cmd-a"
        } else {
            "ctrl-a"
        };
        let selected = cx
            .update(|window, cx| {
                runtime::dispatch(
                    window,
                    cx,
                    &note,
                    &request(6, "chord", json!({"chord": select_all})),
                )
            })
            .expect("select all");
        assert_eq!(selected["caret"], json!(3));
        assert_eq!(selected["anchor"], json!(0));
    }

    #[gpui::test]
    fn scroll_before_first_paint_is_retryable_and_empty_chord_is_invalid(cx: &mut TestAppContext) {
        // Keep the note entity separate from the window root: this test is
        // about a view that has never been painted, not about repainting the
        // root.
        let note = cx.update(|cx| cx.new(|cx| NoteWindow::new(cx, "hello", light(), None)));
        let window = cx
            .update(|cx| {
                cx.open_window(Default::default(), |_window, cx| {
                    cx.new(|cx| NoteWindow::new(cx, "", light(), None))
                })
            })
            .expect("open note window");

        let scroll = window
            .update(cx, |_root, window, cx| {
                runtime::dispatch(
                    window,
                    cx,
                    &note,
                    &request(1, "scroll", json!({"dy": 10.0})),
                )
            })
            .expect("window update");
        let error = scroll.expect_err("scroll before the first paint");
        assert_eq!(error.code, "not_ready");

        let chord = window
            .update(cx, |_root, window, cx| {
                runtime::dispatch(
                    window,
                    cx,
                    &note,
                    &request(2, "chord", json!({"chord": ""})),
                )
            })
            .expect("window update");
        let error = chord.expect_err("empty chord");
        assert_eq!(error.code, "invalid_args");

        let dx = window
            .update(cx, |_root, window, cx| {
                runtime::dispatch(
                    window,
                    cx,
                    &note,
                    &request(3, "scroll", json!({"dx": 1.0, "dy": 0.0})),
                )
            })
            .expect("window update");
        let error = dx.expect_err("horizontal scroll");
        assert_eq!(error.code, "unsupported");
    }
}
