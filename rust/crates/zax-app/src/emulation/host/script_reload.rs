//! The script reload of EDITOR-1 (`tools.reload_script`, F5): the profile of the game being
//! played is read again from its file and a new session is made of it, while the game stays
//! where it is.
//!
//! The core state of the frame on screen goes into the new session, whose `Start` hook runs
//! there. (The NES app could also replay a movie for an exact reload; the SNES app has no
//! movies.)

use super::{EmulationHost, GameView};
use crate::emulation::emulation_state::EmulationState;
use crate::emulation::game_catalog::GameCatalog;
use crate::game_session::GameSession;
use crate::persistence::app_settings::AppSettings as _;
use crate::ui::strings::{self, UiLanguage};

impl<C: GameCatalog, V: GameView> EmulationHost<C, V> {
    /// Reloads the profile's scripts (see the module); `exact` asked for the NES app's movie
    /// replay and changes nothing here.
    pub(super) fn reload_script(&mut self, exact: bool) {
        self.complete_frame_in_flight();
        let Some(game_id) = self.current_game_id().map(str::to_owned) else {
            return;
        };
        // The editor mode: a profile made from a project is built again from it first.
        if self.settings.editor_mode() {
            if let Some(Err(message)) = self.games.rebuild_from_project(&game_id) {
                self.log(&format!(
                    "script reload: the project does not build: {message}"
                ));
                self.notifications.show_error(&message);
                return;
            }
        }
        self.games.forget_profile(&game_id);
        let Some(game) = self.games.resolve(&game_id) else {
            return;
        };
        let Some(mut old) = self.session.take() else {
            return;
        };
        let state = old.core().save_state(true);
        let frame_count = old.core().frame_count();
        let Ok(state) = state else {
            self.session = Some(old);
            return;
        };
        old.end(&mut self.settings, &mut self.scene);
        old.dispose(&mut self.settings, &mut self.scene);

        if let Err(message) = self.ensure_session(&game, true, None) {
            self.log(&format!("script reload failed: {message}"));
            self.notifications.show_error(&message);
            self.current_game = None;
            self.set_state(EmulationState::Stopped);
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        if let Err(error) = session.restore_core(&state) {
            self.log(&format!(
                "script reload: the core state does not load: {error}"
            ));
            return;
        }
        self.apply_profile_scene();
        if let Some(session) = self.session.as_mut() {
            session.start(&mut self.settings, &mut self.scene);
        }
        self.rollback.clear();
        self.frame_steps.clear();
        let _ = exact;
        self.log(&format!("scripts reloaded at frame {frame_count}"));
        self.notify(&text("Scripts reloaded", "Scripts rechargés"));
    }

    /// Whether the session of the game being played drives or changes the core from its scripts,
    /// in which case only an exact reload is right (see the module).
    #[must_use]
    pub fn scripts_drive_the_core(&self) -> bool {
        self.session.as_ref().is_some_and(|session: &GameSession| {
            !session.core_follows_the_inputs() || session.scripts_change_the_core()
        })
    }
}

/// `english`, or `french` when the interface is in French.
fn text(english: &str, french: &str) -> String {
    if strings::language() == UiLanguage::French {
        french.to_owned()
    } else {
        english.to_owned()
    }
}
