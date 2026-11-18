//! The tools of the MCP server and their execution on the main thread, once per displayed frame
//! after it is drawn (so a screenshot is of what the window shows).
//!
//! Every action goes through the host's commands, the same ones the window's menus send: what
//! Claude does shows in the window. A call that changes the game is answered once a few frames
//! have been drawn after it.

use std::fmt::Write as _;

use serde_json::{json, Value};

use super::server::{Content, McpServer, ToolCall, ToolResult};
use crate::application::AppHost;
use crate::emulation::commands::{Command, CommandArgs};

/// What the server tells the client about itself.
pub const INSTRUCTIONS: &str = "XSnes3D+, a SSNES emulator that renders games in 3D through Lua \
profiles. These tools drive the running window: pause and resume the game, look at it \
(screenshot, 3d or 2d), dump the tiles of the frame on screen, reload the profile's scripts after \
editing them (they restart on the frame on screen), and read the log (Lua output and errors).";

/// The tools, as `tools/list` gives them.
#[must_use]
pub fn definitions() -> Vec<Value> {
    let empty = json!({ "type": "object", "properties": {} });
    vec![
        json!({ "name": "editor_info", "description": "What this editor session is about: the \
            game and its ROM identity, the editor folder and its documents (AGENT.md \
            first), the project of the game's profile, the harness. The same text as the prompt \
            the user copies; ask it when you lost it or start afresh.",
            "inputSchema": empty }),
        json!({ "name": "status", "description": "The game, the frame count, pause state and \
            whether the scripts drive the core.",
            "inputSchema": empty }),
        json!({ "name": "play", "description": "Resumes the game.", "inputSchema": empty }),
        json!({ "name": "pause", "description": "Pauses the game.", "inputSchema": empty }),
        json!({ "name": "reload_script", "description": "Reads the profile file again and \
            restarts its scripts on the frame on screen. Answers once done.",
            "inputSchema": empty }),
        json!({ "name": "screenshot", "description": "The picture on screen as a PNG: the 3D view \
            (scaled to width x height, default 960 x 720) with the user's drawings over it (the \
            editor's pens; annotations: false leaves them out), or the NES picture (2d, 256 x \
            240, wider with widescreen margins, no drawings).",
            "inputSchema": { "type": "object", "properties": {
                "view": { "type": "string", "enum": ["3d", "2d"], "default": "3d" },
                "annotations": { "type": "boolean", "default": true },
                "width": { "type": "integer", "minimum": 16, "maximum": 3840 },
                "height": { "type": "integer", "minimum": 16, "maximum": 2160 } } } }),
        json!({ "name": "read_ram", "description": "Reads CPU memory without side effects \
            ($0000-$07FF is the work RAM), as hexadecimal lines of 16 bytes.",
            "inputSchema": { "type": "object", "properties": {
                "address": { "type": "integer", "minimum": 0, "maximum": 65535 },
                "length": { "type": "integer", "minimum": 1, "maximum": 4096, "default": 16 } },
                "required": ["address"] } }),
        json!({ "name": "dump_frame", "description": "Writes the tile records of the frame on \
            screen to a JSON file, as zax-harness render --dump-frame does (palette, then low and \
            high planes, x, y, palette, mirrors, hidden, sprite of each tile).",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string", "description": "The JSON file to write" } },
                "required": ["path"] } }),
        json!({ "name": "annotations", "description": "The strokes the user drew over the view \
            with the editor's pens (red, blue, black, white): for each, its pen and its points, \
            from 0 to 1 from the top left of the view and in pixels of a width x height view \
            (default 960 x 720, as screenshot).",
            "inputSchema": { "type": "object", "properties": {
                "width": { "type": "integer", "minimum": 16, "maximum": 3840 },
                "height": { "type": "integer", "minimum": 16, "maximum": 2160 } } } }),
        json!({ "name": "clear_annotations", "description": "Wipes the user's drawings over the \
            view.", "inputSchema": empty }),
        json!({ "name": "get_log", "description": "The last lines of the log: Lua print, warnings \
            and errors, and the program's own lines.",
            "inputSchema": { "type": "object", "properties": {
                "lines": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 } } } }),
        json!({ "name": "run_command", "description": "Runs a command of the menus by its name \
            (e.g. emulation.pause_resume, video.toggle_2d, emulation.reset), with an optional \
            number and text.",
            "inputSchema": { "type": "object", "properties": {
                "name": { "type": "string" },
                "number": { "type": "integer" },
                "text": { "type": "string" } }, "required": ["name"] } }),
    ]
}

/// A call answered later: when enough frames were drawn.
struct Deferred {
    call: ToolCall,
    /// Frames still to draw before it may be answered (the command has to be applied first).
    frames: u32,
}

/// The server and the calls waiting on the main thread.
pub struct McpBridge {
    server: McpServer,
    deferred: Vec<Deferred>,
}

impl McpBridge {
    /// Starts the server on `127.0.0.1:port`, and the log's memory for `get_log`.
    ///
    /// # Errors
    /// The port could not be bound.
    pub fn start(port: u16) -> std::io::Result<Self> {
        crate::app_log::keep_recent();
        Ok(Self {
            server: McpServer::start(port)?,
            deferred: Vec::new(),
        })
    }

    /// The port listened on.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.server.port()
    }

    /// Once a displayed frame, after it is drawn: answers what can be answered. `editor_info`
    /// gives the text of `editor_info` (the agent's prompt of the editor mode), made only when
    /// asked.
    pub fn serve(&mut self, host: &mut AppHost, editor_info: &dyn Fn(&AppHost) -> String) {
        let mut waiting = Vec::new();
        for mut deferred in std::mem::take(&mut self.deferred) {
            deferred.frames = deferred.frames.saturating_sub(1);
            if deferred.frames > 0 {
                waiting.push(deferred);
            } else {
                let result = finish(&deferred.call, host);
                let _ = deferred.call.reply.send(result);
            }
        }
        self.deferred = waiting;
        for call in self.server.take_calls() {
            if call.name == "editor_info" {
                let _ = call.reply.send(ToolResult::text(editor_info(host)));
                continue;
            }
            match start(&call, host) {
                Started::Answer(result) => {
                    let _ = call.reply.send(result);
                }
                Started::Later(frames) => self.deferred.push(Deferred { call, frames }),
            }
        }
    }
}

/// What starting a call gives.
enum Started {
    Answer(ToolResult),
    /// Answered by [`finish`] after this many frames.
    Later(u32),
}

/// Starts `call`: commands are sent, quick reads answered at once.
fn start(call: &ToolCall, host: &mut AppHost) -> Started {
    let arguments = &call.arguments;
    let number = |name: &str| arguments.get(name).and_then(Value::as_i64);
    match call.name.as_str() {
        "status" | "get_log" | "read_ram" | "dump_frame" | "annotations" | "clear_annotations"
        | "screenshot" => Started::Answer(finish(call, host)),
        "play" => {
            host.enqueue(Command::Resume, CommandArgs::default());
            Started::Later(2)
        }
        "pause" => {
            host.enqueue(Command::Pause, CommandArgs::default());
            Started::Later(2)
        }
        "reload_script" => {
            host.enqueue(Command::ReloadScript, CommandArgs::of_number(0));
            Started::Later(3)
        }
        "run_command" => {
            let name = arguments.get("name").and_then(Value::as_str).unwrap_or("");
            let Some(command) = Command::from_name(name) else {
                return Started::Answer(ToolResult::error(format!("No command '{name}'.")));
            };
            let args = CommandArgs {
                number: number("number")
                    .and_then(|value| i32::try_from(value).ok())
                    .unwrap_or(0),
                text: arguments
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                number2: 0,
            };
            if !host.can_execute(command, &args) {
                return Started::Answer(ToolResult::error(format!(
                    "The command '{name}' cannot run now."
                )));
            }
            host.enqueue(command, args);
            Started::Later(2)
        }
        other => Started::Answer(ToolResult::error(format!("No tool '{other}'."))),
    }
}

/// The answer of `call` once its command took effect: what the tool reads, or the status.
fn finish(call: &ToolCall, host: &mut AppHost) -> ToolResult {
    let arguments = &call.arguments;
    let number = |name: &str| arguments.get(name).and_then(Value::as_i64);
    match call.name.as_str() {
        "screenshot" => screenshot(host, arguments),
        "read_ram" => {
            let Some(session) = host.session() else {
                return ToolResult::error("No game is running.");
            };
            let address = number("address").unwrap_or(0).clamp(0, 0xFFFF);
            let length = number("length").unwrap_or(16).clamp(1, 4096);
            let mut text = String::new();
            for row in (address..(address + length).min(0x1_0000)).step_by(16) {
                let _ = write!(text, "{row:04X}:");
                for address in row..(row + 16).min(address + length).min(0x1_0000) {
                    let value = session
                        .core()
                        .read_memory(u16::try_from(address).unwrap_or(0));
                    let _ = write!(text, " {value:02X}");
                }
                text.push('\n');
            }
            ToolResult::text(text)
        }
        "dump_frame" => dump_frame(host, arguments),
        "annotations" => {
            let size = |name: &str, default: i64| {
                u32::try_from(number(name).unwrap_or(default).clamp(16, 3840)).unwrap_or(16)
            };
            let listed = host
                .annotations()
                .to_json(size("width", 960), size("height", 720));
            ToolResult::text(listed.to_string())
        }
        "clear_annotations" => {
            host.annotations_mut().clear();
            ToolResult::text("The drawings are wiped.")
        }
        "get_log" => {
            let count = usize::try_from(number("lines").unwrap_or(50).clamp(1, 1000)).unwrap_or(50);
            ToolResult::text(crate::app_log::recent(count).join("\n"))
        }
        _ => ToolResult::text(status(host).to_string()),
    }
}

/// `dump_frame`: the tile records of the frame on screen, layer by layer, and its 256 finished
/// colours as 0xRRGGBB.
fn dump_frame(host: &AppHost, arguments: &Value) -> ToolResult {
    let Some(path) = arguments.get("path").and_then(Value::as_str) else {
        return ToolResult::error("dump_frame needs a path.");
    };
    let Some(input) = host.session().and_then(|session| session.core().engine_input()) else {
        return ToolResult::error("No game is running.");
    };
    let mut text = String::from("{\n  \"colors\": [");
    for (entry, color) in input.colors.iter().enumerate() {
        let value = (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b);
        let _ = write!(text, "{}{value}", if entry > 0 { ", " } else { "" });
    }
    let _ = write!(text, "],\n  \"bg_mode\": {},\n  \"layers\": [\n", input.bg_mode);
    let mut count = 0;
    for (l, layer) in input.layers.iter().enumerate() {
        let _ = write!(text, "    {{ \"layer\": {}, \"tiles\": [\n", layer.layer);
        for (n, record) in layer.records.iter().enumerate() {
            let _ = write!(
                text,
                "      {{ \"index\": {}, \"x\": {}, \"y\": {}, \"palette\": {}, \
                 \"mirrorH\": {}, \"mirrorV\": {}, \"hidden\": {}, \"sprite\": {} }}{}",
                record.index,
                record.x,
                record.y,
                record.palette,
                record.mirror_h,
                record.mirror_v,
                record.hidden,
                !record.background,
                if n + 1 < layer.records.len() { ",\n" } else { "\n" }
            );
        }
        count += layer.records.len();
        let more = if l + 1 < input.layers.len() { "," } else { "" };
        let _ = writeln!(text, "    ] }}{more}");
    }
    text.push_str("  ]\n}\n");
    match std::fs::write(path, text) {
        Ok(()) => ToolResult::text(format!("{count} tiles written to {path}")),
        Err(error) => ToolResult::error(format!("{path}: {error}")),
    }
}

/// The state the `status` tool gives, and what every action answers with.
fn status(host: &AppHost) -> Value {
    json!({
        "game": host.current_game_title(),
        "state": format!("{:?}", host.state()),
        "core_frame_count": host.session().map(|session| session.core().frame_count()),
        "scripts_drive_the_core": host.scripts_drive_the_core(),
        "two_dimensional": host.showing_two_dimensional(),
    })
}

/// The picture on screen as a PNG.
fn screenshot(host: &mut AppHost, arguments: &Value) -> ToolResult {
    let two_dimensional = arguments.get("view").and_then(Value::as_str) == Some("2d");
    let png = if two_dimensional {
        let Some(session) = host.session() else {
            return ToolResult::error("No game is running.");
        };
        let mut picture = Vec::new();
        let rows = session.copy_nes_picture(&mut picture);
        if rows == 0 || picture.len() % (rows * 4) != 0 {
            return ToolResult::error("No picture yet.");
        }
        let width = picture.len() / rows / 4;
        let rgb: Vec<u8> = picture
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        zax_render::assets::png_image::encode_rgb(
            u32::try_from(width).unwrap_or(0),
            u32::try_from(rows).unwrap_or(0),
            &rgb,
        )
    } else {
        let size = |name: &str, default: u32| {
            arguments
                .get(name)
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(default)
                .clamp(16, 3840)
        };
        let (width, height) = (size("width", 960), size("height", 720));
        let Some(mut rgb) = host
            .view_mut()
            .and_then(|view| view.capture_rgb(width, height))
        else {
            return ToolResult::error("The 3D view could not be read.");
        };
        // The user's drawings over the view (the editor's pens), unless asked without.
        if arguments.get("annotations").and_then(Value::as_bool) != Some(false) {
            host.annotations().draw_into(&mut rgb, width, height);
        }
        zax_render::assets::png_image::encode_rgb(width, height, &rgb)
    };
    match png {
        Ok(png) => ToolResult {
            content: vec![Content::Png(png), Content::Text(status(host).to_string())],
            is_error: false,
        },
        Err(error) => ToolResult::error(format!("PNG: {error}")),
    }
}
