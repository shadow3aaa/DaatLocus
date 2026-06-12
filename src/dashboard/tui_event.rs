/// TUI event abstraction that decouples the event loop from crossterm internals.
#[derive(Clone, Debug)]
pub enum TuiEvent {
    /// A terminal key event.
    Key(crossterm::event::KeyEvent),
    /// A bracketed paste payload.
    Paste(String),
    /// Terminal resized.
    Resize,
}

impl TuiEvent {
    /// Map a raw crossterm Event into a TuiEvent, returning None for uninteresting events.
    pub fn from_crossterm(event: crossterm::event::Event) -> Option<Self> {
        match event {
            crossterm::event::Event::Key(key) => Some(TuiEvent::Key(key)),
            crossterm::event::Event::Paste(data) => Some(TuiEvent::Paste(data)),
            crossterm::event::Event::Resize(..) => Some(TuiEvent::Resize),
            // Mouse, FocusGained, FocusLost → ignored for now
            _ => None,
        }
    }
}
