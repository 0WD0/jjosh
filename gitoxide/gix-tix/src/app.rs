use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use gix::{ObjectId, bstr::BString, traverse::commit::ParentIds};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommitRow {
    pub id: ObjectId,
    pub parent_ids: ParentIds,
    pub lane: String,
    pub committer_time: gix::date::Time,
    pub author_name: BString,
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
    Complete,
    Cancelled,
    MoveUp,
    MoveDown,
    HalfPageUp,
    HalfPageDown,
    PageUp,
    PageDown,
    First,
    Last,
    ToggleDate,
    ToggleName,
    PinMetadata,
    UnpinMetadata,
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
    pub lane_time: Option<Duration>,
    pub show_committer_date: bool,
    pub show_author_name: bool,
    pub pin_metadata: Option<bool>,
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
            lane_time: None,
            show_committer_date: true,
            show_author_name: true,
            pin_metadata: None,
            follow_tail: false,
        }
    }

    pub(crate) fn extend_commits(&mut self, rows: Vec<CommitRow>) {
        if self.state != State::Loading || rows.is_empty() {
            return;
        }
        let was_empty = self.rows.is_empty();
        self.rows.extend(rows);
        if was_empty {
            self.selected = Some(0);
            self.ensure_visible();
        } else if self.follow_tail {
            self.selected = Some(self.rows.len() - 1);
            self.ensure_visible();
        }
    }

    pub fn update(&mut self, action: Action) -> Vec<Effect> {
        match action {
            Action::Complete if self.state == State::Loading => {
                let selected = self.selected.map(|index| self.rows[index].id);
                self.lane_time = Some(finish_rows(&mut self.rows));
                self.selected = selected.and_then(|id| self.rows.iter().position(|row| row.id == id));
                self.state = State::Complete;
                self.follow_tail = false;
                self.ensure_visible();
            }
            Action::Cancelled if self.state == State::Cancelling => self.state = State::Cancelled,
            Action::MoveUp => self.move_selection(1, false),
            Action::MoveDown => self.move_selection(1, true),
            Action::HalfPageUp => self.move_selection((self.viewport_rows / 2).max(1), false),
            Action::HalfPageDown => self.move_selection((self.viewport_rows / 2).max(1), true),
            Action::PageUp => self.move_selection(self.viewport_rows.max(1), false),
            Action::PageDown => self.move_selection(self.viewport_rows.max(1), true),
            Action::First => self.select(0),
            Action::Last if !self.rows.is_empty() => {
                self.selected = Some(self.rows.len() - 1);
                self.follow_tail = self.state == State::Loading;
                self.ensure_visible();
            }
            Action::ToggleDate => self.show_committer_date = !self.show_committer_date,
            Action::ToggleName => self.show_author_name = !self.show_author_name,
            Action::PinMetadata => self.pin_metadata = Some(true),
            Action::UnpinMetadata => self.pin_metadata = Some(false),
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

    pub(crate) fn ensure_visible(&mut self) {
        let Some(selected) = self.selected else { return };
        let height = self.viewport_rows.max(1);
        if selected < self.offset {
            self.offset = selected;
        } else if selected >= self.offset.saturating_add(height) {
            self.offset = selected + 1 - height;
        }
    }
}

fn finish_rows(rows: &mut Vec<CommitRow>) -> Duration {
    let positions: HashMap<_, _> = rows.iter().enumerate().map(|(index, row)| (row.id, index)).collect();
    let mut children = vec![0usize; rows.len()];
    for row in rows.iter() {
        for parent in &row.parent_ids {
            if let Some(index) = positions.get(parent) {
                children[*index] += 1;
            }
        }
    }

    let mut ready: Vec<_> = children
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(index, count)| (*count == 0).then_some(index))
        .collect();
    let mut order = Vec::with_capacity(rows.len());
    while let Some(index) = ready.pop() {
        order.push(index);
        for parent in rows[index].parent_ids.iter().rev() {
            if let Some(parent_index) = positions.get(parent) {
                children[*parent_index] -= 1;
                if children[*parent_index] == 0 {
                    ready.push(*parent_index);
                }
            }
        }
    }
    if order.len() == rows.len() {
        let mut old: Vec<_> = std::mem::take(rows).into_iter().map(Some).collect();
        *rows = order
            .into_iter()
            .map(|index| old[index].take().expect("each row is moved exactly once"))
            .collect();
    }
    let start = Instant::now();
    render_lanes(rows, &positions);
    start.elapsed()
}

fn render_lanes(rows: &mut [CommitRow], known: &HashMap<ObjectId, usize>) {
    let mut columns = Vec::new();
    let mut next = Vec::new();
    let mut parents = Vec::new();
    let mut edges = Vec::new();
    for row in rows {
        let current = columns.iter().position(|id| *id == row.id).unwrap_or_else(|| {
            columns.push(row.id);
            columns.len() - 1
        });

        parents.clear();
        for parent in row.parent_ids.iter().copied().filter(|id| known.contains_key(id)) {
            if !parents.iter().any(|(id, _, _)| *id == parent) {
                parents.push((parent, columns.iter().position(|id| *id == parent), 0));
            }
        }
        next.clear();
        edges.clear();
        for (index, id) in columns[..current].iter().copied().enumerate() {
            let destination = next.len();
            next.push(id);
            edges.push((index, destination));
        }
        for (parent, old_position, destination) in &mut parents {
            *destination = match old_position {
                Some(position) if *position < current => *position,
                _ => {
                    let destination = next.len();
                    next.push(*parent);
                    if let Some(position) = old_position
                        && *position != current
                    {
                        edges.push((*position, destination));
                    }
                    destination
                }
            };
        }
        for (index, id) in columns.iter().copied().enumerate().skip(current + 1) {
            if parents.iter().any(|(_, position, _)| *position == Some(index)) {
                continue;
            }
            let destination = next.len();
            next.push(id);
            edges.push((index, destination));
        }
        for (_, _, destination) in &parents {
            edges.push((current, *destination));
        }
        row.lane = transition(columns.len(), next.len(), current, &edges);
        std::mem::swap(&mut columns, &mut next);
    }
}

fn transition(before: usize, after: usize, current: usize, edges: &[(usize, usize)]) -> String {
    const UP: u8 = 1;
    const DOWN: u8 = 2;
    const LEFT: u8 = 4;
    const RIGHT: u8 = 8;
    const VERTICAL: u8 = UP | DOWN;
    const HORIZONTAL: u8 = LEFT | RIGHT;
    const CROSS: u8 = VERTICAL | HORIZONTAL;
    const VERTICAL_RIGHT: u8 = VERTICAL | RIGHT;
    const VERTICAL_LEFT: u8 = VERTICAL | LEFT;
    const DOWN_HORIZONTAL: u8 = DOWN | HORIZONTAL;
    const UP_HORIZONTAL: u8 = UP | HORIZONTAL;
    const DOWN_RIGHT: u8 = DOWN | RIGHT;
    const DOWN_LEFT: u8 = DOWN | LEFT;
    const UP_RIGHT: u8 = UP | RIGHT;
    const UP_LEFT: u8 = UP | LEFT;

    let width = before.max(after).max(current + 1) * 2 - 1;
    let mut cells = vec![0u8; width];
    for &(from, to) in edges {
        let from = from * 2;
        let to = to * 2;
        cells[from] |= UP;
        cells[to] |= DOWN;
        if from < to {
            cells[from] |= RIGHT;
            cells[to] |= LEFT;
            for cell in &mut cells[from + 1..to] {
                *cell |= LEFT | RIGHT;
            }
        } else if to < from {
            cells[from] |= LEFT;
            cells[to] |= RIGHT;
            for cell in &mut cells[to + 1..from] {
                *cell |= LEFT | RIGHT;
            }
        }
    }

    let mut out = String::with_capacity(width + 1);
    for (index, cell) in cells.into_iter().enumerate() {
        out.push(if index == current * 2 {
            '●'
        } else {
            match cell {
                0 => ' ',
                CROSS => '┼',
                VERTICAL_RIGHT => '├',
                VERTICAL_LEFT => '┤',
                DOWN_HORIZONTAL => '┬',
                UP_HORIZONTAL => '┴',
                DOWN_RIGHT => '┌',
                DOWN_LEFT => '┐',
                UP_RIGHT => '└',
                UP_LEFT => '┘',
                HORIZONTAL => '─',
                _ => '│',
            }
        });
    }
    out.push(' ');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(n: u8) -> CommitRow {
        let mut bytes = [0; 20];
        bytes[19] = n;
        CommitRow {
            id: ObjectId::Sha1(bytes),
            parent_ids: ParentIds::new(),
            lane: String::new(),
            committer_time: gix::date::Time::default(),
            author_name: "author".into(),
            subject: format!("commit {n}").into(),
        }
    }

    fn row_with_parents(n: u8, parents: &[u8]) -> CommitRow {
        let mut commit = row(n);
        commit.parent_ids = parents.iter().map(|n| row(*n).id).collect();
        commit
    }

    #[test]
    fn completion_orders_and_draws_merge_lanes() {
        let mut app = App::new(10);
        for row in [
            row_with_parents(4, &[3, 2]),
            row_with_parents(3, &[1]),
            row(1),
            row_with_parents(2, &[1]),
        ] {
            app.extend_commits(vec![row]);
        }

        app.update(Action::Complete);

        assert_eq!(
            app.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [row(4).id, row(3).id, row(2).id, row(1).id]
        );
        assert_eq!(
            app.rows.iter().map(|row| row.lane.as_str()).collect::<Vec<_>>(),
            ["●─┐ ", "● │ ", "├─● ", "● "]
        );
    }

    #[test]
    fn lane_reuses_a_parent_that_is_already_to_the_right() {
        let mut app = App::new(10);
        for row in [row_with_parents(4, &[2, 3]), row_with_parents(2, &[3]), row(3)] {
            app.extend_commits(vec![row]);
        }

        app.update(Action::Complete);

        assert_eq!(
            app.rows.iter().map(|row| row.lane.as_str()).collect::<Vec<_>>(),
            ["●─┐ ", "●─┘ ", "● "]
        );
    }

    #[test]
    fn completion_keeps_independent_lines_of_history_together() {
        let mut app = App::new(10);
        app.extend_commits(vec![
            row_with_parents(5, &[3]),
            row_with_parents(4, &[2]),
            row_with_parents(3, &[1]),
            row_with_parents(2, &[1]),
            row(1),
        ]);

        app.update(Action::Complete);

        assert_eq!(
            app.rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            [row(5).id, row(3).id, row(4).id, row(2).id, row(1).id],
            "topological order finishes one line before showing another"
        );
    }

    #[test]
    fn selection_follows_the_oldest_commit_until_the_user_moves() {
        let mut app = App::new(2);
        app.extend_commits(vec![row(1), row(2), row(3)]);

        app.update(Action::Last);
        assert_eq!(app.selected, Some(2), "Last selects the oldest loaded commit");
        assert_eq!(app.offset, 1, "the selection remains visible");

        app.extend_commits(vec![row(4)]);
        assert_eq!(app.selected, Some(3), "new commits extend the followed tail");
        assert_eq!(app.offset, 2, "the viewport follows the tail");

        app.update(Action::MoveUp);
        app.extend_commits(vec![row(5)]);
        assert_eq!(app.selected, Some(2), "manual navigation stops following the tail");
    }

    #[test]
    fn navigation_is_clamped_and_uses_the_viewport_for_pages() {
        let mut app = App::new(2);
        app.extend_commits((1..=5).map(row).collect());

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
    fn half_pages_use_half_the_viewport() {
        let mut app = App::new(4);
        app.extend_commits((1..=5).map(row).collect());

        app.update(Action::HalfPageDown);
        assert_eq!(app.selected, Some(2));
        app.update(Action::HalfPageUp);
        assert_eq!(app.selected, Some(0));
    }

    #[test]
    fn toggles_metadata_columns() {
        let mut app = App::new(1);

        app.update(Action::ToggleDate);
        app.update(Action::ToggleName);
        app.update(Action::PinMetadata);

        assert!(!app.show_committer_date);
        assert!(!app.show_author_name);
        assert_eq!(app.pin_metadata, Some(true));
        app.update(Action::UnpinMetadata);
        assert_eq!(app.pin_metadata, Some(false));
    }

    #[test]
    fn cancellation_preserves_rows_and_ignores_late_worker_events() {
        let mut app = App::new(10);
        app.extend_commits(vec![row(1)]);

        assert_eq!(app.update(Action::Cancel), vec![Effect::Cancel]);
        assert_eq!(app.state, State::Cancelling);
        app.extend_commits(vec![row(2)]);
        assert_eq!(app.rows.len(), 1, "commits arriving after cancellation are ignored");

        app.update(Action::Cancelled);
        assert_eq!(app.state, State::Cancelled);
        assert_eq!(app.rows.len(), 1, "cancellation keeps already displayed commits");
    }

    #[test]
    fn completion_and_copy_effects_use_the_current_selection() {
        let mut app = App::new(10);
        assert!(
            app.update(Action::Copy).is_empty(),
            "there is nothing to copy without a selection"
        );
        app.extend_commits(vec![row(7)]);

        assert_eq!(app.update(Action::Copy), vec![Effect::Copy(row(7).id)]);
        app.update(Action::Complete);
        assert_eq!(app.state, State::Complete);
        assert_eq!(app.rows.len(), 1, "the loaded row count is the completed total");
        assert_eq!(app.update(Action::Quit), vec![Effect::Quit]);
    }
}
