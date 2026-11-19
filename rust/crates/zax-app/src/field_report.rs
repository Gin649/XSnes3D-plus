//! XSnes3D+: play reports. While a game runs, everything the log says goes to a report file in
//! `<user data>/reports`, with a head saying which game, which build and which device. The
//! frame profiler is on while reports are on, so the reports carry the time per frame of each
//! stage and the audio counters every couple of seconds.
//!
//! A report is written as `<name>.part` while the game runs and renamed `<name>.txt` when it
//! ends normally. A `.part` left at the next start means the program died during that game (a
//! crash, or Android closing it), so it gets a last line saying so and is renamed too. On
//! Android the Java side uploads the finished `.txt` reports to GitHub when a token is set
//! (`ReportUploader`), and moves them to `reports/sent`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use std::fmt::Write as _;

use crate::app_log;
use crate::emulation::game_catalog::GameCatalog as _;

/// The folder of the reports in the user data.
pub const FOLDER: &str = "reports";

/// The build, for the head of each report.
const VERSION: &str = env!("XSNES3D_VERSION");

/// The report of the game being played.
#[derive(Debug, Default)]
pub struct FieldReport {
    /// The report being written: its `.part` path, and the game's id.
    open: Option<(PathBuf, String)>,
}

impl FieldReport {
    /// Follows the game being played: closes the report of a game that ended and opens one for
    /// a game that started. Called once per displayed frame.
    pub fn follow(&mut self, root: &Path, game: Option<(&str, &str)>) {
        let same = match (&self.open, game) {
            (Some((_, open)), Some((id, _))) => open == id,
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.finish("game ended normally");
        if let Some((id, title)) = game {
            self.begin(root, id, title);
        }
    }

    /// Closes the report being written, `last` as its last line.
    pub fn finish(&mut self, last: &str) {
        if let Some((part, _)) = self.open.take() {
            app_log::end_report(&format!("== {last}"));
            let _ = fs::rename(&part, part.with_extension("txt"));
        }
    }

    fn begin(&mut self, root: &Path, id: &str, title: &str) {
        let folder = root.join(FOLDER);
        if fs::create_dir_all(&folder).is_err() {
            return;
        }
        let stamp = time::OffsetDateTime::now_utc();
        let name = format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}-{}",
            stamp.year(),
            u8::from(stamp.month()),
            stamp.day(),
            stamp.hour(),
            stamp.minute(),
            stamp.second(),
            safe_name(title)
        );
        let part = folder.join(format!("{name}.part"));
        let Ok(file) = File::create(&part) else {
            return;
        };
        app_log::start_report(file);
        self.open = Some((part, id.to_owned()));
        // The head, through the log so it lands in the report in order.
        app_log::file_line(&format!("== XSnes3D+ {VERSION} play report"));
        app_log::file_line(&format!("== game: {title} (id {id})"));
        app_log::file_line(&format!("== started (UTC): {stamp}"));
        app_log::file_line("== start-up of the app:");
        for line in app_log::first_lines() {
            app_log::file_line(&format!("   {line}"));
        }
        app_log::file_line("== play:");
    }
}

/// Finishes the reports a past run left open: that run ended during the game. Called at start.
pub fn close_left_over(root: &Path) {
    let Ok(entries) = fs::read_dir(root.join(FOLDER)) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "part") {
            if let Ok(mut file) = fs::OpenOptions::new().append(true).open(&path) {
                use std::io::Write as _;
                let _ = writeln!(
                    file,
                    "== THE APP ENDED DURING THIS GAME (a crash, or the system closed it): the \
                     lines above are the last it wrote"
                );
            }
            let _ = fs::rename(&path, path.with_extension("txt"));
        }
    }
}

/// Logs a panic's message and place before the program goes down, so the report has them.
pub fn log_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let place = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()))
            .unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let thread = std::thread::current();
        app_log::error(&format!(
            "== PANIC on thread '{}' at {place}: {message}",
            thread.name().unwrap_or("?")
        ));
        let backtrace = std::backtrace::Backtrace::force_capture();
        for line in backtrace.to_string().lines().take(60) {
            app_log::file_line(&format!("   {line}"));
        }
        previous(info);
    }));
}

/// The folder of the moments, in the reports folder.
pub const MOMENTS: &str = "moments";

/// "Report this moment": keeps what is on screen for the 3D work, in
/// `reports/moments/<time>-<game>/`: a save state of the moment (`state.bin`, without the ROM:
/// the game is in the roms repository), the 3D view as the player saw it (`shot-3d.png`), the
/// console's own picture (`shot-2d.png`) and what is known of the moment (`info.txt`). The
/// folder is written as `<name>.part` and renamed when complete, so the uploader never sends
/// half of one. Returns the folder's name.
///
/// # Errors
///
/// No game running, or the folder could not be written.
/// The name a report gives the game being played: its title, or for a game played on one of
/// the generic entries ("Smart 3D (no profile)", "2D Mode") the ROM's file name, since the
/// entry's title says nothing of the game. `None` when no game runs.
#[must_use]
pub fn game_name(host: &crate::application::AppHost) -> Option<String> {
    let title = host.current_game_title()?;
    if host.current_game_is_generic() {
        let id = host.current_game_id().unwrap_or_default();
        if let Some(rom) = host.games().rom_path(id) {
            let stem = Path::new(&rom)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_owned);
            if let Some(stem) = stem.filter(|stem| !stem.is_empty()) {
                return Some(stem);
            }
        }
    }
    Some(title.to_owned())
}

pub fn report_moment(host: &mut crate::application::AppHost) -> Result<String, String> {
    let title = game_name(host).ok_or("no game is running")?;
    let id = host.current_game_id().unwrap_or_default().to_owned();
    let rom = host.games().rom_path(&id).unwrap_or_default();
    let stamp = time::OffsetDateTime::now_utc();
    let name = format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}-{}",
        stamp.year(),
        u8::from(stamp.month()),
        stamp.day(),
        stamp.hour(),
        stamp.minute(),
        stamp.second(),
        safe_name(&title)
    );
    let folder = host.paths().root.join(FOLDER).join(MOMENTS);
    let part = folder.join(format!("{name}.part"));
    fs::create_dir_all(&part).map_err(|error| error.to_string())?;
    let write = |file: &str, bytes: &[u8]| {
        fs::write(part.join(file), bytes).map_err(|error| format!("{file}: {error}"))
    };

    let mut info = format!(
        "XSnes3D+ {VERSION} moment\ngame: {title}\nid: {id}\nrom: {rom}\ntime (UTC): {stamp}\n\
         view: {}\n",
        host.view_name()
    );
    let session = host.session().ok_or("no game is running")?;
    let core = session.core();
    let _ = writeln!(info, "frame: {}", core.frame_count());
    if let Some(input) = core.engine_input() {
        let _ = write!(info, "bg mode: {}\nlayers (shown tiles):", input.bg_mode);
        for layer in &input.layers {
            let shown = layer.records.iter().filter(|r| !r.hidden).count();
            let _ = write!(info, " {}:{shown}", layer.layer);
        }
        info.push('\n');
    }
    let state = core.save_state(false)?;
    let mut picture = Vec::new();
    let rows = core.wide_picture_into(&mut picture);
    write("state.bin", &state)?;
    let width = picture.len() / rows.max(1) / 4;
    if let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(rows)) {
        if let Ok(png) = zax_render::assets::png_image::encode_rgba(width, height, &picture) {
            write("shot-2d.png", &png)?;
        }
    }
    let (width, height) = (960, 720);
    match host.view_mut().and_then(|view| view.capture_rgb(width, height)) {
        Some(rgb) => {
            if let Ok(png) = zax_render::assets::png_image::encode_rgb(width, height, &rgb) {
                write("shot-3d.png", &png)?;
            }
        }
        None => info.push_str("shot-3d: the 3D view could not be read\n"),
    }
    write("info.txt", info.as_bytes())?;
    let done = folder.join(&name);
    fs::rename(&part, &done).map_err(|error| error.to_string())?;
    app_log::line(&format!("== moment reported: {name}"));
    Ok(name)
}

/// The title as a file name: letters, digits and dashes, at most 60 of them.
fn safe_name(title: &str) -> String {
    let mut name: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    name.trim_matches('-').chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_plain_file_names() {
        assert_eq!(
            "Adventures-of-Yogi-Bear-USA",
            safe_name("Adventures of Yogi Bear (USA)")
        );
    }

    #[test]
    fn a_report_left_open_is_finished_at_the_next_start() {
        let root = std::env::temp_dir().join(format!("xs-report-{}", std::process::id()));
        let folder = root.join(FOLDER);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("a.part"), "== play:\nlast line\n").unwrap();
        close_left_over(&root);
        let text = fs::read_to_string(folder.join("a.txt")).unwrap();
        assert!(text.contains("THE APP ENDED DURING THIS GAME"));
        assert!(!folder.join("a.part").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
