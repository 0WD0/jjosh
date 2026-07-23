//! A fast, interactive commit graph for terminals.

#![forbid(unsafe_code)]

mod app;
mod history;
mod ui;

use std::{
    ffi::OsString,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result};
use app::{Action, App, Effect};
use crossterm::{
    clipboard::CopyToClipboard,
    event::{self, Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute,
};
use history::{Decorations, Event};

const EVENT_BATCH_SIZE: usize = 256;
const POLL_INTERVAL: Duration = Duration::from_millis(16);

/// Options for [`run()`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Exit once all commits and graph lanes have been computed.
    pub quit_on_finish: bool,
}

/// Run the interactive commit graph for `repository`.
pub fn run(repository: gix::ThreadSafeRepository, revisions: Vec<OsString>, options: Options) -> Result<()> {
    let mut terminal = ratatui::try_init().context("could not initialize terminal")?;
    let result = event_loop(&mut terminal, repository, revisions, options);
    let restore = ratatui::try_restore().context("could not restore terminal");
    let lane_time = result?;
    restore?;
    if let Some(lane_time) = lane_time {
        eprintln!("lane computation: {:.3}s", lane_time.as_secs_f64());
    }
    Ok(())
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    repository: gix::ThreadSafeRepository,
    revisions: Vec<OsString>,
    options: Options,
) -> Result<Option<Duration>> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let repository = repository.to_thread_local();
        let result = history::load(&repository, &revisions, &worker_cancelled, |event| {
            sender.send(Ok(event)).is_ok()
        });
        if let Err(err) = result {
            let _ = sender.send(Err(err));
        }
    });

    let mut app = App::new(1);
    let mut decorations = Decorations::new();
    loop {
        let mut events = 0;
        for message in receiver.try_iter().take(EVENT_BATCH_SIZE) {
            events += 1;
            match message? {
                Event::Decorations(value) => decorations = value,
                Event::Commits(rows) => app.extend_commits(rows),
                Event::Complete => {
                    drop(app.update(Action::Complete));
                    if options.quit_on_finish {
                        return Ok(app.lane_time);
                    }
                }
                Event::Cancelled => drop(app.update(Action::Cancelled)),
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app, &decorations))?;
        if !event::poll(poll_timeout(events))? {
            continue;
        }
        let TerminalEvent::Key(key) = event::read()? else {
            continue;
        };
        if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            continue;
        }
        let Some(action) = action(key) else { continue };
        for effect in app.update(action) {
            match effect {
                Effect::Cancel => cancelled.store(true, Ordering::Relaxed),
                Effect::Copy(id) => execute!(
                    terminal.backend_mut(),
                    CopyToClipboard::to_clipboard_from(id.to_hex().to_string())
                )?,
                Effect::Quit => return Ok(None),
            }
        }
    }
}

fn poll_timeout(events: usize) -> Duration {
    if events == EVENT_BATCH_SIZE {
        Duration::ZERO
    } else {
        POLL_INTERVAL
    }
}

fn action(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDown),
        KeyCode::Char('h') => Some(Action::ScrollLeft),
        KeyCode::Char('l') => Some(Action::ScrollRight),
        KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::PageUp),
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::PageDown),
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::HalfPageUp),
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::HalfPageDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::First),
        KeyCode::End | KeyCode::Char('G') => Some(Action::Last),
        KeyCode::Char('d') => Some(Action::ToggleDate),
        KeyCode::Char('n') => Some(Action::ToggleName),
        KeyCode::Char('r') => Some(Action::ToggleSpecialRefs),
        KeyCode::Char('[') => Some(Action::PinMetadata),
        KeyCode::Char(']') => Some(Action::UnpinMetadata),
        KeyCode::Char('y') => Some(Action::Copy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_navigation_and_control_c() {
        assert_eq!(
            action(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(Action::PageUp)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            Some(Action::PageUp)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            Some(Action::PageDown)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(Action::HalfPageUp)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Action::HalfPageDown)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE)),
            Some(Action::ScrollLeft)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(Action::ScrollRight)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Action::ToggleDate)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(Action::ToggleName)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(Action::ToggleSpecialRefs)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('['), KeyModifiers::NONE)),
            Some(Action::PinMetadata)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE)),
            Some(Action::UnpinMetadata)
        );
        assert_eq!(
            action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(action(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn saturated_event_batches_do_not_wait() {
        assert_eq!(poll_timeout(EVENT_BATCH_SIZE), Duration::ZERO);
        assert_eq!(poll_timeout(EVENT_BATCH_SIZE - 1), POLL_INTERVAL);
    }
}
