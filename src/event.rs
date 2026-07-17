use ratatui::crossterm::event::Event as CrosstermEvent;

use crate::model::PatchEntry;

/// Everything the main loop reacts to, delivered over a single channel.
pub enum AppEvent {
    /// A terminal input event from the blocking reader thread.
    Input(CrosstermEvent),
    /// Periodic timer used to animate loading indicators.
    Tick,
    /// The initial patch list finished loading.
    PatchesLoaded(Result<Vec<PatchEntry>, String>),
}
