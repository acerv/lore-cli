use ratatui::crossterm::event::Event as CrosstermEvent;

use crate::model::{Email, PatchEntry, PatchStatus};

/// Everything the main loop reacts to, delivered over a single channel.
pub enum AppEvent {
    /// A terminal input event from the blocking reader thread.
    Input(CrosstermEvent),
    /// Periodic timer used to animate loading indicators.
    Tick,
    /// The initial patch list finished loading.
    PatchesLoaded(Result<Vec<PatchEntry>, String>),
    /// A subsequent page of patches finished loading.
    MoreLoaded(Result<Vec<PatchEntry>, String>),
    /// A background status probe determined a patch's merge/review state.
    StatusUpdated { message_id: String, status: PatchStatus },
    /// A thread requested by opening a tab finished loading.
    ThreadLoaded {
        message_id: String,
        result: Result<Vec<Email>, String>,
    },
    /// An `A` (apply-with-b4) run finished; carries b4's combined output on
    /// success or an error message on failure.
    Applied {
        message_id: String,
        result: Result<String, String>,
    },
}
