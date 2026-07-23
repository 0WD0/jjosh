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

fn main() -> Result<()> {
    let (revisions, quit_on_finish) = arguments(gix::env::args_os().skip(1));
    if revisions.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!(
            "Usage: tix [--quit-on-finish] [REVISION]...\n\nBrowse commits reachable from HEAD or the given revisions."
        );
        return Ok(());
    }

    let mut terminal = ratatui::try_init().context("could not initialize terminal")?;
    let result = run(&mut terminal, revisions, quit_on_finish);
    let restore = ratatui::try_restore().context("could not restore terminal");
    result.and(restore)
}

fn arguments(args: impl Iterator<Item = OsString>) -> (Vec<OsString>, bool) {
    let mut quit_on_finish = false;
    let revisions = args
        .filter(|arg| {
            let is_option = arg == "--quit-on-finish";
            quit_on_finish |= is_option;
            !is_option
        })
        .collect();
    (revisions, quit_on_finish)
}

fn run(terminal: &mut ratatui::DefaultTerminal, revisions: Vec<OsString>, quit_on_finish: bool) -> Result<()> {
    let repository = match std::env::var_os("GIT_DIR") {
        Some(git_dir) => git_dir.into(),
        None => std::env::current_dir().context("could not determine current directory")?,
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = Arc::clone(&cancelled);
    let (sender, receiver) = mpsc::sync_channel(1024);
    std::thread::spawn(move || {
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
        for message in receiver.try_iter().take(256) {
            match message? {
                Event::Decorations(value) => decorations = value,
                Event::Commit(row) => drop(app.update(Action::Commit(row))),
                Event::Complete => {
                    drop(app.update(Action::Complete));
                    if quit_on_finish {
                        return Ok(());
                    }
                }
                Event::Cancelled => drop(app.update(Action::Cancelled)),
            }
        }
        terminal.draw(|frame| ui::draw(frame, &mut app, &decorations))?;
        if !event::poll(Duration::from_millis(16))? {
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
                Effect::Quit => return Ok(()),
            }
        }
    }
}

fn action(key: KeyEvent) -> Option<Action> {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(Action::Quit),
        KeyCode::Char('q') => Some(Action::Quit),
        KeyCode::Esc => Some(Action::Cancel),
        KeyCode::Up | KeyCode::Char('k') => Some(Action::MoveUp),
        KeyCode::Down | KeyCode::Char('j') => Some(Action::MoveDown),
        KeyCode::PageUp => Some(Action::PageUp),
        KeyCode::PageDown => Some(Action::PageDown),
        KeyCode::Home | KeyCode::Char('g') => Some(Action::First),
        KeyCode::End | KeyCode::Char('G') => Some(Action::Last),
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
            action(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        assert_eq!(action(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)), None);
    }

    #[test]
    fn quit_on_finish_is_not_a_revision() {
        let (revisions, quit_on_finish) = arguments(["--quit-on-finish", "main"].into_iter().map(OsString::from));
        assert!(quit_on_finish, "the option is enabled");
        assert_eq!(revisions, ["main"], "only revisions remain");
    }
}
