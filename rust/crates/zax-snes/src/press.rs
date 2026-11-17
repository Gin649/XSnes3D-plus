//! The `--press` button script: which port 1 buttons are held on which frames.
//!
//! Same format as the `Xnes3D+` harness: a comma separated list of `<first>[-<last>]:<buttons>`,
//! frames counted from 0 after the ROM is loaded, buttons joined by `+`. Ranges may overlap,
//! then the held buttons are all of them together. `games.json` scripts carry over, only the
//! button names grow (x, y, l and r are new).

use std::fmt;

/// Port 1 buttons, in the bit order of Mesen's SNES controller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Buttons(u16);

impl Buttons {
    /// No button held.
    pub const NONE: Self = Self(0);

    const NAMES: [(&'static str, u16); 12] = [
        ("a", 1 << 0),
        ("b", 1 << 1),
        ("x", 1 << 2),
        ("y", 1 << 3),
        ("l", 1 << 4),
        ("r", 1 << 5),
        ("select", 1 << 6),
        ("start", 1 << 7),
        ("up", 1 << 8),
        ("down", 1 << 9),
        ("left", 1 << 10),
        ("right", 1 << 11),
    ];

    /// The raw bits handed to the core.
    #[must_use]
    pub fn bits(self) -> u16 {
        self.0
    }

    /// One button by name, any case.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        let name = name.trim().to_ascii_lowercase();
        Self::NAMES
            .iter()
            .find(|(known, _)| *known == name)
            .map(|&(_, bit)| Self(bit))
    }
}

impl std::ops::BitOr for Buttons {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A script entry that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PressError(pub String);

impl fmt::Display for PressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "--press: {}", self.0)
    }
}

impl std::error::Error for PressError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    first: u32,
    last: u32,
    buttons: Buttons,
}

/// The buttons held on each frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PressScript {
    spans: Vec<Span>,
}

impl PressScript {
    /// Reads a script; an empty text holds nothing.
    ///
    /// # Errors
    ///
    /// [`PressError`] for an entry that is not `<frames>:<buttons>`, a bad frame range or an
    /// unknown button.
    pub fn parse(text: &str) -> Result<Self, PressError> {
        let mut spans = Vec::new();
        for entry in text.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let (frames, names) = entry
                .split_once(':')
                .filter(|(frames, _)| !frames.is_empty())
                .ok_or_else(|| PressError(format!("'{entry}' is not <frames>:<buttons>")))?;
            let (first, last) = if let Some((first, last)) = frames.split_once('-') {
                (frame_number(first)?, frame_number(last)?)
            } else {
                let only = frame_number(frames)?;
                (only, only)
            };
            if last < first {
                return Err(PressError(format!("'{frames}' ends before it starts")));
            }
            let mut buttons = Buttons::NONE;
            for name in names.split('+').filter(|n| !n.trim().is_empty()) {
                buttons = buttons
                    | Buttons::named(name)
                        .ok_or_else(|| PressError(format!("unknown button '{}'", name.trim())))?;
            }
            spans.push(Span {
                first,
                last,
                buttons,
            });
        }
        Ok(Self { spans })
    }

    /// The buttons held on frame `frame` (0 is the first frame).
    #[must_use]
    pub fn at(&self, frame: u32) -> Buttons {
        self.spans
            .iter()
            .filter(|span| (span.first..=span.last).contains(&frame))
            .fold(Buttons::NONE, |held, span| held | span.buttons)
    }
}

fn frame_number(text: &str) -> Result<u32, PressError> {
    text.trim()
        .parse()
        .map_err(|_| PressError(format!("'{}' is not a frame number", text.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_spans_add_up() {
        let script = PressScript::parse("100-110:start, 105:a+Right").unwrap();
        assert_eq!(script.at(99), Buttons::NONE);
        assert_eq!(script.at(100).bits(), 1 << 7);
        assert_eq!(script.at(105).bits(), (1 << 7) | 1 | (1 << 11));
    }

    #[test]
    fn bad_entries_are_refused() {
        assert!(PressScript::parse("12").is_err());
        assert!(PressScript::parse("5-2:a").is_err());
        assert!(PressScript::parse("1:jump").is_err());
        assert_eq!(PressScript::parse(" ").unwrap(), PressScript::default());
    }
}
