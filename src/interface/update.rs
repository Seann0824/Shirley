use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use super::{app::App, event::Event};

pub fn update(app: &mut App, event: Event) -> Option<String> {
    match event {
        // 模型选择器打开时，按键先交给面板处理（上/下选择、Enter 确认、Esc 取消）。
        // 其余按键吞掉，避免在面板上层继续编辑输入框造成状态错乱。
        Event::Key(key_event)
            if key_event.kind == KeyEventKind::Press && app.is_picker_open() =>
        {
            match key_event.code {
                KeyCode::Up => app.picker_move(-1),
                KeyCode::Down => app.picker_move(1),
                KeyCode::Enter => {
                    app.picker_confirm();
                }
                KeyCode::Esc => app.picker_cancel(),
                _ => {}
            }
            return None;
        }
        // 会话选择器打开时同理：按键先交给面板（上/下选择、Enter 切换、Esc 取消）。
        Event::Key(key_event)
            if key_event.kind == KeyEventKind::Press && app.is_session_picker_open() =>
        {
            match key_event.code {
                KeyCode::Up => app.session_picker_move(-1),
                KeyCode::Down => app.session_picker_move(1),
                KeyCode::Enter => {
                    app.session_picker_confirm();
                }
                KeyCode::Esc => app.session_picker_cancel(),
                _ => {}
            }
            return None;
        }
        // 指令候选浮层打开时：↑↓←→ 移动高亮，Tab / Enter 采纳，Esc 关闭浮层（不退出）。
        // 其余按键交给下方常规编辑逻辑（用户仍可继续打字修正）。
        Event::Key(key_event)
            if key_event.kind == KeyEventKind::Press
                && !app.suggestions().is_empty()
                && matches!(
                    key_event.code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                        | KeyCode::Tab | KeyCode::Enter | KeyCode::Esc
                ) =>
        {
            match key_event.code {
                KeyCode::Up | KeyCode::Left => app.suggestion_move(-1),
                KeyCode::Down | KeyCode::Right => app.suggestion_move(1),
                KeyCode::Tab | KeyCode::Enter => app.accept_suggestion(),
                KeyCode::Esc => app.dismiss_suggestions(),
                _ => {}
            }
            return None;
        }
        Event::Key(key_event) if key_event.kind == KeyEventKind::Press => match key_event.code {
            // Esc：AI 回复中 → 打断本轮回复（会话不丢）；
            // 回溯编辑态 → 取消本次改动（不退出）；
            // 其余空闲 → 退出程序。
            KeyCode::Esc => {
                if app.is_waiting() {
                    app.request_interrupt();
                } else if app.is_login() {
                    // 登录流程中：Esc 只取消登录，不退出程序（与回溯编辑态一致）。
                    app.cancel_login();
                } else if app.is_rewind_edit() {
                    app.cancel_rewind_edit();
                } else {
                    app.exit();
                }
            }
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
            // 滚轮已让位于终端原生选择（见 event.rs），滚动改由 PageUp / PageDown 承担。
            KeyCode::PageUp => app.scroll_by(-3),
            KeyCode::PageDown => app.scroll_by(3),
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
        Event::Resize(_width, _height) => {}
    }
    None
}
