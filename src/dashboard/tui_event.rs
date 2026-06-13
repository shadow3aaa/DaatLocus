/// TUI event abstraction that decouples the event loop from crossterm internals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiEvent {
    /// A terminal key event.
    Key(crossterm::event::KeyEvent),
    /// Vertical mouse-wheel movement in activity-feed rows. Positive scrolls down.
    MouseWheel { rows: i16 },
    /// A bracketed paste payload.
    Paste(String),
    /// Terminal resized.
    Resize,
}

const MOUSE_WHEEL_ROWS: i16 = 3;

impl TuiEvent {
    /// Map a raw crossterm Event into a TuiEvent, returning None for uninteresting events.
    pub fn from_crossterm(event: crossterm::event::Event) -> Option<Self> {
        match event {
            crossterm::event::Event::Key(key) => Some(TuiEvent::Key(key)),
            crossterm::event::Event::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::ScrollUp => Some(TuiEvent::MouseWheel {
                    rows: -MOUSE_WHEEL_ROWS,
                }),
                crossterm::event::MouseEventKind::ScrollDown => Some(TuiEvent::MouseWheel {
                    rows: MOUSE_WHEEL_ROWS,
                }),
                _ => None,
            },
            crossterm::event::Event::Paste(data) => Some(TuiEvent::Paste(data)),
            crossterm::event::Event::Resize(..) => Some(TuiEvent::Resize),
            // FocusGained, FocusLost, and non-wheel mouse events are ignored for now.
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    fn mouse_event(kind: MouseEventKind) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: 10,
            row: 4,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn vertical_mouse_wheel_maps_to_dedicated_scroll_event() {
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::ScrollUp)),
            Some(TuiEvent::MouseWheel {
                rows: -MOUSE_WHEEL_ROWS
            })
        );
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::ScrollDown)),
            Some(TuiEvent::MouseWheel {
                rows: MOUSE_WHEEL_ROWS
            })
        );
    }

    #[test]
    fn mouse_wheel_is_not_forwarded_as_key_input() {
        assert!(!matches!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::ScrollDown)),
            Some(TuiEvent::Key(_))
        ));
    }

    #[test]
    fn non_vertical_mouse_events_are_ignored() {
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::ScrollLeft)),
            None
        );
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::Down(MouseButton::Left))),
            None
        );
    }
}
