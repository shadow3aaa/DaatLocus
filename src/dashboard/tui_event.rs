/// TUI event abstraction that decouples the event loop from crossterm internals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiEvent {
    /// A terminal key event.
    Key(crossterm::event::KeyEvent),
    /// Vertical mouse-wheel movement in activity-feed rows. Positive scrolls down.
    MouseWheel { rows: i16 },
    /// Mouse selection gesture inside selectable TUI components.
    MouseSelection {
        kind: TuiMouseSelectionKind,
        x: u16,
        y: u16,
        modifiers: crossterm::event::KeyModifiers,
    },
    /// A bracketed paste payload.
    Paste(String),
    /// Terminal resized.
    Resize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TuiMouseSelectionKind {
    Down,
    Drag,
    Up,
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
                crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    Some(TuiEvent::MouseSelection {
                        kind: TuiMouseSelectionKind::Down,
                        x: mouse.column,
                        y: mouse.row,
                        modifiers: mouse.modifiers,
                    })
                }
                crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                    Some(TuiEvent::MouseSelection {
                        kind: TuiMouseSelectionKind::Drag,
                        x: mouse.column,
                        y: mouse.row,
                        modifiers: mouse.modifiers,
                    })
                }
                crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                    Some(TuiEvent::MouseSelection {
                        kind: TuiMouseSelectionKind::Up,
                        x: mouse.column,
                        y: mouse.row,
                        modifiers: mouse.modifiers,
                    })
                }
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
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::Down(MouseButton::Right))),
            None
        );
    }

    #[test]
    fn left_mouse_gesture_maps_to_selection_events() {
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::Down(MouseButton::Left))),
            Some(TuiEvent::MouseSelection {
                kind: TuiMouseSelectionKind::Down,
                x: 10,
                y: 4,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::Drag(MouseButton::Left))),
            Some(TuiEvent::MouseSelection {
                kind: TuiMouseSelectionKind::Drag,
                x: 10,
                y: 4,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(
            TuiEvent::from_crossterm(mouse_event(MouseEventKind::Up(MouseButton::Left))),
            Some(TuiEvent::MouseSelection {
                kind: TuiMouseSelectionKind::Up,
                x: 10,
                y: 4,
                modifiers: KeyModifiers::NONE,
            })
        );
    }
}
