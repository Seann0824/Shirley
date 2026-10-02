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
            KeyCode::Char('o') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.toggle_tool_args()
            }
            // Tab：采纳第一个模糊指令候选（如 /inti → /init ）。
            KeyCode::Tab => app.accept_suggestion(),
            // Enter 在等待回复时不提交（submit 内部也会拦截），
            // 但编辑能力不受等待影响，AI 回复期间照样能打字。
            KeyCode::Enter => return app.submit(),
            KeyCode::Backspace => app.pop_input(),
            KeyCode::Delete => app.delete_input(),
            KeyCode::Left => app.move_cursor_left(),
            KeyCode::Right => app.move_cursor_right(),
            KeyCode::Home => app.move_cursor_home(),
            KeyCode::End => app.move_cursor_end(),
            KeyCode::Up => app.history_prev(),
            KeyCode::Down => app.history_next(),
            KeyCode::Char('a') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.move_cursor_home()
            }
            KeyCode::Char('e') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.move_cursor_end()
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.push_input(ch)
            }
            _ => {}
        },
        Event::Key(_) => {}
        // 括号粘贴整体插入：粘贴文本里的换行只当作普通字符写入输入框，
        // 绝不触发发送，避免粘贴多行内容时消息被自动发出去。
        Event::Paste(text) => app.insert_input(&text),
        Event::Mouse(mouse_event) => match mouse_event.kind {
            MouseEventKind::ScrollUp => app.scroll_by(-3),
            MouseEventKind::ScrollDown => app.scroll_by(3),
            _ => {}
        },
        Event::Resize(_width, _height) => {}
    }
    None
}
