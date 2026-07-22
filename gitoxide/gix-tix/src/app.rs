use gix::{bstr::BString, ObjectId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommitRow {
    pub id: ObjectId,
    pub subject: BString,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum State {
    Loading,
    Cancelling,
    Complete,
    Cancelled,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Commit(CommitRow),
    Complete,
    Cancelled,
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    First,
    Last,
    Cancel,
    Copy,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Effect {
    Cancel,
    Copy(ObjectId),
    Quit,
}

#[derive(Debug)]
pub(crate) struct App {
    pub rows: Vec<CommitRow>,
    pub selected: Option<usize>,
    pub offset: usize,
    pub state: State,
    pub viewport_rows: usize,
    follow_tail: bool,
}

impl App {
    pub fn new(viewport_rows: usize) -> Self {
        App {
            rows: Vec::new(),
            selected: None,
            offset: 0,
            state: State::Loading,
            viewport_rows,
            follow_tail: false,
        }
    }

    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Commit(row) if self.state == State::Loading => {
                self.rows.push(row);
                if self.selected.is_none() {
                    self.selected = Some(0);
                } else if self.follow_tail {
                    self.selected = Some(self.rows.len() - 1);
                }
                self.ensure_visible();
            }
            Action::Complete if self.state == State::Loading => {
                self.state = State::Complete;
                self.follow_tail = false;
            }
            Action::Cancelled if self.state == State::Cancelling => self.state = State::Cancelled,
            Action::MoveUp => self.move_selection(1, false),
            Action::MoveDown => self.move_selection(1, true),
            Action::PageUp => self.move_selection(self.viewport_rows.max(1), false),
            Action::PageDown => self.move_selection(self.viewport_rows.max(1), true),
            Action::First => self.select(0),
            Action::Last if !self.rows.is_empty() => {
                self.selected = Some(self.rows.len() - 1);
                self.follow_tail = self.state == State::Loading;
                self.ensure_visible();
            }
            Action::Cancel if self.state == State::Loading => {
                self.state = State::Cancelling;
                return vec![Effect::Cancel];
            }
            Action::Copy => {
                if let Some(row) = self.selected.and_then(|index| self.rows.get(index)) {
                    return vec![Effect::Copy(row.id)];
                }
            }
            Action::Quit => {
                return if matches!(self.state, State::Loading | State::Cancelling) {
                    vec![Effect::Cancel, Effect::Quit]
                } else {
                    vec![Effect::Quit]
                };
            }
            _ => {}
        }
        Vec::new()
    }

    fn move_selection(&mut self, distance: usize, down: bool) {
        let Some(selected) = self.selected else { return };
        self.selected = Some(if down {
            selected.saturating_add(distance).min(self.rows.len() - 1)
        } else {
            selected.saturating_sub(distance)
        });
        self.follow_tail = false;
        self.ensure_visible();
    }

    fn select(&mut self, selected: usize) {
        if !self.rows.is_empty() {
            self.selected = Some(selected.min(self.rows.len() - 1));
            self.follow_tail = false;
            self.ensure_visible();
        }
    }

    fn ensure_visible(&mut self) {
        let Some(selected) = self.selected else { return };
        let height = self.viewport_rows.max(1);
        if selected < self.offset {
            self.offset = selected;
        } else if selected >= self.offset.saturating_add(height) {
            self.offset = selected + 1 - height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: u8) -> CommitRow {
        let mut bytes = [0; 20];
        bytes[19] = n;
        CommitRow {
            id: ObjectId::Sha1(bytes),
            subject: format!("commit {n}").into(),
        }
    }

    #[test]
    fn selection_follows_the_oldest_commit_until_the_user_moves() {
        let mut app = App::new(2);
        app.update(Action::Commit(row(1)));
        app.update(Action::Commit(row(2)));
        app.update(Action::Commit(row(3)));

        app.update(Action::Last);
        assert_eq!(app.selected, Some(2), "Last selects the oldest loaded commit");
        assert_eq!(app.offset, 1, "the selection remains visible");

        app.update(Action::Commit(row(4)));
        assert_eq!(app.selected, Some(3), "new commits extend the followed tail");
        assert_eq!(app.offset, 2, "the viewport follows the tail");

        app.update(Action::MoveUp);
        app.update(Action::Commit(row(5)));
        assert_eq!(app.selected, Some(2), "manual navigation stops following the tail");
    }

    #[test]
    fn navigation_is_clamped_and_uses_the_viewport_for_pages() {
        let mut app = App::new(2);
        for n in 1..=5 {
            app.update(Action::Commit(row(n)));
        }

        app.update(Action::PageDown);
        assert_eq!(app.selected, Some(2), "page-down advances by the viewport height");
        app.update(Action::PageDown);
        assert_eq!(app.selected, Some(4), "page-down clamps at the last row");
        app.update(Action::MoveDown);
        assert_eq!(app.selected, Some(4), "moving past the last row is a no-op");
        app.update(Action::First);
        assert_eq!(app.selected, Some(0), "First selects the newest commit");
        assert_eq!(app.offset, 0, "the newest commit is visible");
    }

    #[test]
    fn cancellation_preserves_rows_and_ignores_late_worker_events() {
        let mut app = App::new(10);
        app.update(Action::Commit(row(1)));

        assert_eq!(app.update(Action::Cancel), vec![Effect::Cancel]);
        assert_eq!(app.state, State::Cancelling);
        app.update(Action::Commit(row(2)));
        assert_eq!(app.rows.len(), 1, "commits arriving after cancellation are ignored");

        app.update(Action::Cancelled);
        assert_eq!(app.state, State::Cancelled);
        assert_eq!(app.rows.len(), 1, "cancellation keeps already displayed commits");
    }

    #[test]
    fn completion_and_copy_effects_use_the_current_selection() {
        let mut app = App::new(10);
        assert!(app.update(Action::Copy).is_empty(), "there is nothing to copy without a selection");
        app.update(Action::Commit(row(7)));

        assert_eq!(app.update(Action::Copy), vec![Effect::Copy(row(7).id)]);
        app.update(Action::Complete);
        assert_eq!(app.state, State::Complete);
        assert_eq!(app.rows.len(), 1, "the loaded row count is the completed total");
        assert_eq!(app.update(Action::Quit), vec![Effect::Quit]);
    }
}
