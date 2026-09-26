use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use super::{app::App, event::Event};

pub fn update(app: &mut App, event: Event) -> Option<String> {
    match event {
        Event::Key(key_event) if key_event.kind == KeyEventKind::Press => match key_event.code {
            KeyCode::Esc => app.exit(),
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => app.exit(),
            KeyCode::Char('t') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.toggle_thinking()
            }
            KeyCode::Enter => return app.submit(),
            KeyCode::Backspace if !app.is_waiting() => app.pop_input(),
            KeyCode::Char(ch)
                if !app.is_waiting() && !key_event.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                app.push_input(ch)
            }
            _ => {}
        },
        Event::Key(_) => {}
        Event::Mouse(_mouse_event) => {}
        Event::Resize(_width, _height) => {}
    }
    None
}
