use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

use super::{app::App, event::Event};

pub fn update(app: &mut App, event: Event) -> Option<String> {
    // A sensitive tool call is parked awaiting the human's decision (C 档体感): while a prompt is
    // shown, only the decision keys act (`y`/`Enter` allow, `n`/`Esc` deny). Everything else is
    // swallowed so the user cannot accidentally type over / submit while the runtime is blocked.
    if app.is_pending_approval() {
        if let Event::Key(key_event) = &event
            && key_event.kind == KeyEventKind::Press
        {
            match key_event.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    app.resolve_pending_approval(true);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.resolve_pending_approval(false);
                }
                _ => {}
            }
        }
        return None;
    }
    match event {
        // When the model picker is open, keys go to the panel first (up/down to move,
        // Enter to confirm, Esc to cancel). Other keys are swallowed so the input box isn't
        // edited underneath the panel, which would corrupt the state.
        Event::Key(key_event) if key_event.kind == KeyEventKind::Press && app.is_picker_open() => {
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
        // Same for the session picker: keys go to the panel first (up/down, Enter to switch, Esc to cancel).
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
        // Subagent transcript preview overlay (`docs/subagent.md` decision 10): the preview is
        // read-only, so it only needs scrolling + close. `c` cancels the subagent if it is running.
        Event::Key(key_event)
            if key_event.kind == KeyEventKind::Press && app.is_subagent_preview_open() =>
        {
            match key_event.code {
                KeyCode::Up => app.subagent_preview_scroll(-1),
                KeyCode::Down => app.subagent_preview_scroll(1),
                KeyCode::PageUp => app.subagent_preview_scroll(-10),
                KeyCode::PageDown => app.subagent_preview_scroll(10),
                KeyCode::Char('c') => app.subagent_preview_cancel(),
                KeyCode::Esc => app.subagent_preview_close(),
                _ => {}
            }
            return None;
        }
        // Subagent picker: keys go to the panel first (up/down, Enter to open the preview, Esc to cancel).
        Event::Key(key_event)
            if key_event.kind == KeyEventKind::Press && app.is_subagent_picker_open() =>
        {
            match key_event.code {
                KeyCode::Up => app.subagent_picker_move(-1),
                KeyCode::Down => app.subagent_picker_move(1),
                KeyCode::Enter => {
                    app.subagent_picker_confirm();
                }
                KeyCode::Esc => app.subagent_picker_cancel(),
                _ => {}
            }
            return None;
        }
        // When the command-suggestion popup is open: arrows move the highlight, Tab / Enter
        // accepts, Esc dismisses the popup (without exiting). Other keys fall through to the
        // normal editing logic below (the user can keep typing to refine).
        Event::Key(key_event)
            if key_event.kind == KeyEventKind::Press
                && !app.suggestions().is_empty()
                && matches!(
                    key_event.code,
                    KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Tab
                        | KeyCode::Enter
                        | KeyCode::Esc
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
            // Esc: while the AI is replying -> interrupt this turn (the session is kept);
            // in rewind-edit state -> cancel this edit (don't exit);
            // otherwise idle -> exit the program.
            KeyCode::Esc => {
                if app.is_waiting() {
                    app.request_interrupt();
                } else if app.is_login() {
                    // In the login flow: Esc only cancels login, it doesn't exit (same as rewind-edit).
                    app.cancel_login();
                } else if app.is_rewind_edit() {
                    app.cancel_rewind_edit();
                } else {
                    app.exit();
                }
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => app.exit(),
            KeyCode::Char('o') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.toggle_tool_args();
            }
            KeyCode::Char('s') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.toggle_latest_compact();
            }
            // Tab: accept the first fuzzy command suggestion (e.g. /inti -> /init).
            KeyCode::Tab => app.accept_suggestion(),
            // Enter doesn't submit while waiting for a reply (submit also guards internally),
            // but editing is unaffected by the wait — you can still type while the AI replies.
            KeyCode::Enter => return app.submit(),
            KeyCode::Backspace => app.pop_input(),
            KeyCode::Delete => app.delete_input(),
            KeyCode::Left => app.move_cursor_left(),
            KeyCode::Right => app.move_cursor_right(),
            KeyCode::Home => app.move_cursor_home(),
            KeyCode::End => app.move_cursor_end(),
            KeyCode::Up => app.history_prev(),
            KeyCode::Down => app.history_next(),
            // Keyboard paging: equivalent to the mouse wheel, for scrolling without a mouse (see event.rs).
            KeyCode::PageUp => app.scroll_by(-3),
            KeyCode::PageDown => app.scroll_by(3),
            KeyCode::Char('a') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.move_cursor_home();
            }
            KeyCode::Char('e') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.move_cursor_end();
            }
            KeyCode::Char(ch) if !key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                app.push_input(ch);
            }
            _ => {}
        },
        // Bracketed paste inserts as a whole: newlines in the pasted text are written to the
        // input box as ordinary characters and never trigger a send, so pasting multiple lines
        // doesn't fire the message off automatically.
        Event::Paste(text) => app.insert_input(&text),
        // Mouse events are handled directly by `Tui::handle_mouse` (it needs the buffer and the
        // clipboard) and never reach here; these empty arms just exhaust the enum.
        Event::Key(_) | Event::Mouse(_) | Event::Resize(..) => {}
    }
    None
}
