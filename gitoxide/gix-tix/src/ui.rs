use gix::bstr::ByteSlice;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use crate::{
    app::{App, CommitRow, State},
    history::Decorations,
};

pub(crate) fn draw(frame: &mut Frame<'_>, app: &mut App, decorations: &Decorations) {
    let [body, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
    app.viewport_rows = body.height as usize;
    app.ensure_visible();
    let start = app.offset.min(app.rows.len());
    let end = start.saturating_add(app.viewport_rows).min(app.rows.len());
    let visible_rows = &app.rows[start..end];
    let content = Rect::new(
        body.x.saturating_add(2),
        body.y,
        body.width.saturating_sub(2),
        body.height,
    );
    let max_lane_width = visible_rows
        .iter()
        .map(|row| row.lane.trim_end().chars().count().saturating_add(1))
        .max()
        .unwrap_or_default();
    let pane_limit = ((body.width as usize) / 3)
        .saturating_sub(2)
        .max(1)
        .min(content.width as usize);
    let pane_width = max_lane_width.min(pane_limit);
    let graph_is_wide = max_lane_width > pane_limit;
    let pin_metadata = app.pin_metadata.unwrap_or(graph_is_wide);
    let show_committer_date = app.show_committer_date;
    let show_author_name = app.show_author_name;
    let show_special_refs = app.show_special_refs;
    let metadata: Vec<_> = visible_rows
        .iter()
        .map(|row| {
            metadata_line(
                row,
                decorations,
                show_committer_date,
                show_author_name,
                show_special_refs,
            )
        })
        .collect();
    let max_offset = if pin_metadata {
        max_lane_width.saturating_sub(pane_width)
    } else {
        visible_rows
            .iter()
            .zip(&metadata)
            .map(|(row, metadata)| row.lane.chars().count().saturating_add(metadata.width()))
            .max()
            .unwrap_or_default()
            .saturating_sub(content.width as usize)
    }
    .min(u16::MAX as usize);
    app.set_horizontal_bounds(content.width as usize, max_offset);
    let horizontal_offset = app.horizontal_offset as u16;

    let visible_rows = &app.rows[start..end];
    for (index, (row, metadata)) in visible_rows.iter().zip(metadata).enumerate() {
        let y = body.y.saturating_add(index as u16);
        let selected = app.selected == Some(start + index);
        let style = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(if selected { "> " } else { "  " }).style(style),
            Rect::new(body.x, y, body.width.min(2), 1),
        );

        let row_area = Rect::new(content.x, y, content.width, 1);
        if pin_metadata {
            frame.render_widget(
                Paragraph::new(row.lane.as_str())
                    .style(style)
                    .scroll((0, horizontal_offset)),
                row_area,
            );
            let pane = Rect::new(
                content.x.saturating_add(pane_width as u16),
                y,
                content.width.saturating_sub(pane_width as u16),
                1,
            );
            frame.render_widget(Clear, pane);
            frame.render_widget(Paragraph::new(metadata).style(style), pane);
        } else {
            let mut spans = Vec::with_capacity(metadata.spans.len() + 1);
            spans.push(Span::raw(row.lane.as_str()));
            spans.extend(metadata.spans);
            frame.render_widget(
                Paragraph::new(Line::from(spans))
                    .style(style)
                    .scroll((0, horizontal_offset)),
                row_area,
            );
        }
    }

    let status = match app.state {
        State::Loading => "loading",
        State::Cancelling => "cancelling",
        State::Complete => "complete",
        State::Cancelled => "cancelled",
    };
    frame.render_widget(
        Paragraph::new(format!(
            "{} commits · {status} · ↑↓/jk move · h/l pan · [ pane · ] natural · d date · n name · r refs · y copy · Esc cancel · q quit",
            app.rows.len()
        )),
        footer,
    );
}

fn metadata_line(
    row: &CommitRow,
    decorations: &Decorations,
    show_committer_date: bool,
    show_author_name: bool,
    show_special_refs: bool,
) -> Line<'static> {
    let id = row.id.to_hex().to_string();
    let labels = decorations.get(&row.id).and_then(|labels| {
        let labels = labels
            .iter()
            .filter(|decoration| show_special_refs || !decoration.special)
            .map(|decoration| decoration.name.to_str_lossy())
            .collect::<Vec<_>>()
            .join(", ");
        (!labels.is_empty()).then_some(labels)
    });
    let mut spans = vec![
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
    spans.push(Span::raw(row.subject.to_str_lossy().into_owned()));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    use super::*;
    use crate::{
        app::{Action, CommitRow},
        history::Decoration,
    };

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
        let decorations = Decorations::from([(
            id,
            vec![
                Decoration {
                    name: "HEAD".into(),
                    special: false,
                },
                Decoration {
                    name: "refs/patches/main/patch".into(),
                    special: true,
                },
            ],
        )]);
        let mut terminal = Terminal::new(TestBackend::new(130, 2))?;

        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;

        let mut expected = Buffer::with_lines([
            format!("{:<130}", "> ● 0101010 (HEAD) 1970-01-01 author subject"),
            format!(
                "{:<130}",
                "1 commits · complete · ↑↓/jk move · h/l pan · [ pane · ] natural · d date · n name · r refs · y copy · Esc cancel · q quit"
            ),
        ]);
        for x in 0..130 {
            expected[(x, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        for x in 4..11 {
            expected[(x, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD));
        }
        terminal.backend().assert_buffer(&expected);

        app.update(Action::ToggleDate);
        app.update(Action::ToggleName);
        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;
        let row = rendered_row(&terminal);
        assert!(!row.contains("1970-01-01"), "d hides the committer date");
        assert!(!row.contains("author"), "n hides the author name");
        assert!(!row.contains("refs/patches"), "special refs are hidden until requested");
        assert!(row.contains("subject"), "the commit subject remains visible");

        app.update(Action::ToggleSpecialRefs);
        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;
        assert!(rendered_row(&terminal).contains("refs/patches"), "r shows special refs");
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

    #[test]
    fn overlays_metadata_on_wide_graphs_and_allows_natural_flow() -> Result<(), Box<dyn std::error::Error>> {
        let mut app = App::new(1);
        app.extend_commits(vec![CommitRow {
            id: gix::ObjectId::Sha1([1; 20]),
            parent_ids: Default::default(),
            lane: String::new(),
            committer_time: gix::date::Time::default(),
            author_name: "author".into(),
            subject: "subject".into(),
        }]);
        app.update(Action::Complete);
        app.rows[0].lane = format!("{}{}", "A".repeat(40), "B".repeat(40));
        let mut terminal = Terminal::new(TestBackend::new(60, 2))?;

        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;
        let pane_column = rendered_row(&terminal)
            .find("0101010")
            .expect("wide graphs automatically pin metadata");
        assert!(
            pane_column < 60,
            "wide graphs automatically pin metadata within the viewport"
        );

        app.update(Action::ScrollRight);
        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;
        assert_eq!(terminal.backend().buffer()[(2, 0)].symbol(), "B");
        assert_eq!(
            rendered_row(&terminal).find("0101010"),
            Some(pane_column),
            "horizontal graph scrolling leaves pinned metadata fixed"
        );

        app.update(Action::ScrollLeft);
        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;
        app.update(Action::UnpinMetadata);
        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;
        assert!(
            !rendered_row(&terminal).contains("0101010"),
            "] restores natural post-graph placement"
        );

        app.update(Action::ScrollRight);
        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;
        assert!(
            rendered_row(&terminal).contains("0101010"),
            "l pages far enough right to reveal natural metadata"
        );

        app.rows[0].lane = "● ".into();
        app.update(Action::PinMetadata);
        terminal.draw(|frame| draw(frame, &mut app, &Decorations::new()))?;
        assert_eq!(
            terminal.backend().buffer()[(4, 0)].symbol(),
            "0",
            "the pane starts immediately after the widest visible lane"
        );
        Ok(())
    }

    fn rendered_row(terminal: &Terminal<TestBackend>) -> String {
        (0..terminal.backend().buffer().area.width).fold(String::new(), |mut out, x| {
            out.push_str(terminal.backend().buffer()[(x, 0)].symbol());
            out
        })
    }
}
