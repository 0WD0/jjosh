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
    let rows = app.rows.iter().map(|row| {
        let id = row.id.to_hex().to_string();
        let labels = decorations.get(&row.id).map(|labels| {
            labels.iter().map(|label| label.to_str_lossy()).collect::<Vec<_>>().join(", ")
        });
        Line::from(vec![
            Span::styled(id[..7].to_owned(), Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(labels.map_or_else(|| " ".into(), |labels| format!(" ({labels}) "))),
            Span::raw(row.subject.to_str_lossy()),
        ])
    });
    let list = List::new(rows)
        .highlight_symbol("> ")
        .highlight_spacing(HighlightSpacing::Always)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_offset(app.offset).with_selected(app.selected);
    frame.render_stateful_widget(list, body, &mut state);
    app.offset = state.offset();

    let status = match app.state {
        State::Loading => "loading",
        State::Cancelling => "cancelling",
        State::Complete => "complete",
        State::Cancelled => "cancelled",
    };
    frame.render_widget(
        Paragraph::new(format!("{} commits · {status} · ↑↓/jk move · y copy · Esc cancel · q quit", app.rows.len())),
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
        app.update(Action::Commit(CommitRow { id, subject: "subject".into() }));
        app.update(Action::Complete);
        let decorations = Decorations::from([(id, vec!["HEAD".into()])]);
        let mut terminal = Terminal::new(TestBackend::new(54, 2))?;

        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;

        let mut expected = Buffer::with_lines([
            "> 0101010 (HEAD) subject                             ",
            "1 commits · complete · ↑↓/jk move · y copy · Esc cance",
        ]);
        for x in 0..54 {
            expected[(x, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        for x in 2..9 {
            expected[(x, 0)].set_style(
                Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            );
        }
        terminal.backend().assert_buffer(&expected);
        Ok(())
    }
}
