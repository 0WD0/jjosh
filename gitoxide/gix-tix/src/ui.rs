use gix::bstr::{BStr, ByteSlice};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
};

use crate::{
    app::{App, CommitRow, State},
    history::{DecorationKind, Decorations},
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
    let selected = app.selected;
    let metadata: Vec<_> = visible_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            metadata_line(
                row,
                app.title(row),
                decorations,
                show_committer_date,
                show_author_name,
                show_special_refs,
                selected == Some(start + index),
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
    let horizontal_offset = app.horizontal_offset.min(max_offset) as u16;

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
            color_graph(frame, row_area, &row.lane, horizontal_offset as usize, selected);
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
            color_graph(frame, row_area, &row.lane, horizontal_offset as usize, selected);
        }
    }
    app.set_horizontal_bounds(content.width as usize, max_offset);

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

fn metadata_line<'a>(
    row: &CommitRow,
    title: &'a BStr,
    decorations: &'a Decorations,
    show_committer_date: bool,
    show_author_name: bool,
    show_special_refs: bool,
    selected: bool,
) -> Line<'a> {
    let id = row.id.to_hex().to_string();
    let mut spans = vec![Span::styled(
        id[..7].to_owned(),
        color(Color::Magenta, selected).add_modifier(Modifier::BOLD),
    )];
    let mut labels = decorations
        .get(&row.id)
        .into_iter()
        .flatten()
        .filter(|decoration| show_special_refs || decoration.kind != DecorationKind::Special)
        .peekable();
    if labels.peek().is_some() {
        spans.push(Span::raw(" ("));
        for (index, decoration) in labels.enumerate() {
            if index != 0 {
                spans.push(Span::raw(", "));
            }
            spans.push(Span::styled(
                decoration.name.to_str_lossy(),
                decoration_style(decoration.kind, selected),
            ));
        }
        spans.push(Span::raw(") "));
    } else {
        spans.push(Span::raw(" "));
    }
    if show_committer_date {
        spans.push(Span::styled(
            format!("{} ", row.committer_time.format_or_unix(gix::date::time::format::SHORT)),
            color(Color::Blue, selected),
        ));
    }
    if show_author_name {
        spans.push(Span::styled(
            format!("{} ", row.author_name.to_str_lossy()),
            color(Color::Green, selected),
        ));
    }
    spans.push(Span::raw(title.to_str_lossy()));
    Line::from(spans)
}

fn decoration_style(kind: DecorationKind, selected: bool) -> Style {
    if selected {
        return Style::default();
    }
    match kind {
        DecorationKind::Head => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        DecorationKind::Local => Style::default().fg(Color::Cyan),
        DecorationKind::Remote => Style::default().fg(Color::Yellow),
        DecorationKind::Tag => Style::default().fg(Color::Magenta),
        DecorationKind::AnnotatedTag => Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        DecorationKind::Special => Style::default().fg(Color::Blue),
    }
}

fn color(color: Color, selected: bool) -> Style {
    if selected {
        Style::default()
    } else {
        Style::default().fg(color)
    }
}

fn color_graph(frame: &mut Frame<'_>, area: Rect, graph: &str, offset: usize, selected: bool) {
    if selected {
        return;
    }
    for (x, symbol) in graph.chars().skip(offset).take(area.width as usize).enumerate() {
        if symbol.is_whitespace() {
            continue;
        }
        let style = if symbol == '●' {
            Style::default().fg(Color::Blue)
        } else {
            graph_style(offset.saturating_add(x) / 2)
        };
        frame.buffer_mut()[(area.x + x as u16, area.y)].set_style(style);
    }
}

fn graph_style(column: usize) -> Style {
    const COLORS: [Color; 7] = [
        Color::Magenta,
        Color::Yellow,
        Color::Cyan,
        Color::Green,
        Color::Reset,
        Color::White,
        Color::Red,
    ];
    let index = column % 14;
    let style = Style::default().fg(COLORS[index % COLORS.len()]);
    if index >= COLORS.len() {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    use super::*;
    use crate::{
        app::{Action, Commit},
        history::{Decoration, DecorationKind},
    };

    #[test]
    fn renders_rows_decorations_selection_and_footer() -> Result<(), Box<dyn std::error::Error>> {
        let id = gix::ObjectId::Sha1([1; 20]);
        let mut app = App::new(2);
        app.extend_commits(vec![Commit {
            id,
            parent_ids: Default::default(),
            lane: String::new(),
            committer_time: gix::date::Time::default(),
            author_name: b"author".as_bstr(),
            title: "subject".into(),
        }]);
        app.update(Action::Complete);
        let decorations = Decorations::from([(
            id,
            vec![
                Decoration {
                    name: "HEAD".into(),
                    kind: DecorationKind::Head,
                },
                Decoration {
                    name: "refs/patches/main/patch".into(),
                    kind: DecorationKind::Special,
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
                .map(|n| Commit {
                    id: gix::ObjectId::Sha1([n; 20]),
                    parent_ids: Default::default(),
                    lane: String::new(),
                    committer_time: gix::date::Time::default(),
                    author_name: b"author".as_bstr(),
                    title: format!("subject {n}").into(),
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
    fn uses_the_tig_palette_without_coloring_the_selection() -> Result<(), Box<dyn std::error::Error>> {
        let id = gix::ObjectId::Sha1([1; 20]);
        let commit = Commit {
            id,
            parent_ids: Default::default(),
            lane: "● │ │ │ │ │ │ │ ".into(),
            committer_time: gix::date::Time::default(),
            author_name: b"author".as_bstr(),
            title: "subject".into(),
        };
        let decorations = Decorations::from([(
            id,
            vec![
                Decoration {
                    name: "HEAD".into(),
                    kind: DecorationKind::Head,
                },
                Decoration {
                    name: "main".into(),
                    kind: DecorationKind::Local,
                },
                Decoration {
                    name: "origin/main".into(),
                    kind: DecorationKind::Remote,
                },
                Decoration {
                    name: "tag: v1".into(),
                    kind: DecorationKind::AnnotatedTag,
                },
                Decoration {
                    name: "refs/stash".into(),
                    kind: DecorationKind::Special,
                },
            ],
        )]);
        let mut app = App::new(1);
        app.extend_commits(vec![commit]);
        let row = &app.rows[0];
        let line = metadata_line(row, app.title(row), &decorations, true, true, true, false);
        let style = |text| {
            line.spans
                .iter()
                .find(|span| span.content == text)
                .expect("the styled field is present")
                .style
        };
        assert_eq!(
            style("0101010"),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        );
        assert_eq!(style("1970-01-01 "), Style::default().fg(Color::Blue));
        assert_eq!(style("author "), Style::default().fg(Color::Green));
        assert_eq!(
            style("HEAD"),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        );
        assert_eq!(style("main"), Style::default().fg(Color::Cyan));
        assert_eq!(style("origin/main"), Style::default().fg(Color::Yellow));
        assert_eq!(
            style("tag: v1"),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        );
        assert_eq!(style("refs/stash"), Style::default().fg(Color::Blue));

        app.selected = None;
        let mut terminal = Terminal::new(TestBackend::new(80, 2))?;
        terminal.draw(|frame| draw(frame, &mut app, &decorations))?;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 0)].fg, Color::Blue, "commit dots use graph-commit");
        assert_eq!(buffer[(4, 0)].fg, Color::Yellow, "lanes cycle through tig's palette");
        assert_eq!(
            buffer[(16, 0)].fg,
            Color::Magenta,
            "the palette repeats after seven lanes"
        );
        assert!(
            buffer[(16, 0)].modifier.contains(Modifier::BOLD),
            "the second palette cycle is bold"
        );
        Ok(())
    }

    #[test]
    fn overlays_metadata_on_wide_graphs_and_allows_natural_flow() -> Result<(), Box<dyn std::error::Error>> {
        let mut app = App::new(1);
        app.extend_commits(vec![Commit {
            id: gix::ObjectId::Sha1([1; 20]),
            parent_ids: Default::default(),
            lane: String::new(),
            committer_time: gix::date::Time::default(),
            author_name: b"author".as_bstr(),
            title: "subject".into(),
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
