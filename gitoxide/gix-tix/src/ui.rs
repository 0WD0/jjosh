use gix::bstr::ByteSlice;
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{HighlightSpacing, List, ListState, Paragraph},
};

use crate::{
    app::{App, State},
    history::Decorations,
};

pub(crate) fn draw(frame: &mut Frame<'_>, app: &mut App, decorations: &Decorations) {
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    app.viewport_rows = body.height as usize;
    app.ensure_visible();
    let start = app.offset.min(app.rows.len());
    let end = start.saturating_add(app.viewport_rows).min(app.rows.len());
    let show_committer_date = app.show_committer_date;
    let show_author_name = app.show_author_name;
    let rows = app.rows[start..end].iter().map(|row| {
        let id = row.id.to_hex().to_string();
        let labels = decorations.get(&row.id).map(|labels| {
            labels
                .iter()
                .map(|label| label.to_str_lossy())
                .collect::<Vec<_>>()
                .join(", ")
        });
        let mut spans = vec![
            Span::raw(&row.lane),
            Span::styled(id[..7].to_owned(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(labels.map_or_else(|| " ".into(), |labels| format!(" ({labels}) "))),
        ];
        if show_committer_date {
            spans.push(Span::raw(format!(
                "{} ",
                row.committer_time.format_or_unix(gix::date::time::format::SHORT)
            )));
        }
        if show_author_name {
            spans.push(Span::raw(format!("{} ", row.author_name.to_str_lossy())));
        }
        spans.push(Span::raw(row.subject.to_str_lossy()));
        Line::from(spans)
    });
    let list = List::new(rows)
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let selected = app
        .selected
        .and_then(|selected| selected.checked_sub(start))
        .filter(|selected| *selected < end - start);
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, body, &mut state);

    let status = match app.state {
        State::Loading => "loading",
        State::Cancelling => "cancelling",
        State::Complete => "complete",
        State::Cancelled => "cancelled",
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{} commits · {status} · ↑↓/jk move · d date · n name · y copy · Esc cancel · q quit",
            app.rows.len()
        )),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    use super::*;
    use crate::app::{Action, CommitRow};

    #[test]
    fn renders_rows_decorations_selection_and_footer() -> Result<(), Box<dyn std::error::Error>> {
        let id = gix::ObjectId::Sha1([1; 20]);
        let mut app = App::new(2);
        app.extend_commits(vec![CommitRow {
            id,
            parent_ids: Default::default(),
            lane: String::new(),
            committer_time: gix::date::Time::default(),
            author_name: "author".into(),
            subject: "subject".into(),
        }]);
        app.update(Action::Complete);
        let decorations = Decorations::from([(id, vec!["HEAD".into()])]);
        let mut terminal = Terminal::new(TestBackend::new(90, 2))?;

        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;

        let mut expected = Buffer::with_lines([
            format!("{:<90}", "> ● 0101010 (HEAD) 1970-01-01 author subject"),
            format!(
                "{:<90}",
                "1 commits · complete · ↑↓/jk move · d date · n name · y copy · Esc cancel · q quit"
            ),
        ]);
        for x in 0..90 {
            expected[(x, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        for x in 4..11 {
            expected[(x, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD));
        }
        terminal.backend().assert_buffer(&expected);

        app.update(Action::ToggleDate);
        app.update(Action::ToggleName);
        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;
        let row = (0..90).fold(String::new(), |mut out, x| {
            out.push_str(terminal.backend().buffer()[(x, 0)].symbol());
            out
        });
        assert!(!row.contains("1970-01-01"), "d hides the committer date");
        assert!(!row.contains("author"), "n hides the author name");
        assert!(row.contains("subject"), "the commit subject remains visible");
        Ok(())
    }

    #[test]
    fn renders_only_the_visible_rows() -> Result<(), Box<dyn std::error::Error>> {
        let mut app = App::new(2);
        app.extend_commits(
            (1..=3)
                .map(|n| CommitRow {
                    id: gix::ObjectId::Sha1([n; 20]),
                    parent_ids: Default::default(),
                    lane: String::new(),
                    committer_time: gix::date::Time::default(),
                    author_name: "author".into(),
                    subject: format!("subject {n}").into(),
                })
                .collect(),
        );
        app.update(Action::Complete);
        app.update(Action::Last);
        let mut terminal = Terminal::new(TestBackend::new(24, 3))?;

        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(5, 0)].symbol(), "2", "the viewport starts at the second row");
        assert_eq!(buffer[(5, 1)].symbol(), "3", "the selected third row remains visible");
        assert!(
            buffer[(0, 1)].modifier.contains(Modifier::REVERSED),
            "the slice-local selection highlights the global selection"
        );
        assert_eq!(app.selected, Some(2), "drawing preserves the global selection");
        assert_eq!(app.offset, 1, "drawing preserves the global offset");
        Ok(())
    }
}
