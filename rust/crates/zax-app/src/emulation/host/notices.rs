//! The notifications, the drawings over the view and the editor's requests.

use super::{EditorRequest, EmulationHost, GameView};
use crate::emulation::game_catalog::GameCatalog;

impl<C: GameCatalog, V: GameView> EmulationHost<C, V> {
    /// The drawings over the view (EDITOR-1).
    #[must_use]
    pub const fn annotations(&self) -> &crate::emulation::annotations::Annotations {
        &self.annotations
    }

    /// The drawings over the view, to draw or wipe.
    pub const fn annotations_mut(&mut self) -> &mut crate::emulation::annotations::Annotations {
        &mut self.annotations
    }

    /// What a command asked of the editor mode since the last call.
    pub const fn take_editor_request(&mut self) -> Option<EditorRequest> {
        self.editor_request.take()
    }

    pub(super) fn notify(&mut self, message: &str) {
        self.notifications.show_normal(message);
    }

    /// Shows `message` in the notification area, as an error when `error`.
    pub fn show_notification(&mut self, message: &str, error: bool) {
        if error {
            self.notifications.show_error(message);
        } else {
            self.notifications.show_normal(message);
        }
    }
}
