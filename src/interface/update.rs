use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

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
            KeyCode::Delete if !app.is_waiting() => app.delete_input(),
            KeyCode::Left if !app.is_waiting() => app.move_cursor_left(),
            KeyCode::Right if !app.is_waiting() => app.move_cursor_right(),
            KeyCode::Home if !app.is_waiting() => app.move_cursor_home(),
            KeyCode::End if !app.is_waiting() => app.move_cursor_end(),
            KeyCode::Char('a') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.move_cursor_home()
            }
            KeyCode::Char('e') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.move_cursor_end()
            }
            KeyCode::Char(ch)
                if !app.is_waiting() && !key_event.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                app.push_input(ch)
            }
            _ => {}
        },
        Event::Key(_) => {}
        Event::Mouse(mouse_event) => match mouse_event.kind {
            MouseEventKind::ScrollUp => app.scroll_by(-3),
            MouseEventKind::ScrollDown => app.scroll_by(3),
            _ => {}
        },
        Event::Resize(_width, _height) => {}
    }
    None
}
