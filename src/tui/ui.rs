use super::app::{
    clean, App, DetailClick, EditTarget, Focus, LayoutRegions, ModalClick, Overlay, View,
};
use super::catalog::FieldKind;
use super::operations::Action;
use super::theme;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Scrollbar,
        ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
    },
    Frame,
};

fn accent() -> Style {
    theme::accent()
}

fn render_scrollbar(frame: &mut Frame<'_>, area: Rect, total: usize, position: usize) {
    if total == 0 {
        return;
    }
    let mut state = ScrollbarState::new(total).position(position.min(total - 1));
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None),
        area,
        &mut state,
    );
}

/// Style review effect lines: warnings and destructive notes stand out.
fn effect_lines(effects: &[String]) -> Vec<Line<'static>> {
    effects
        .iter()
        .flat_map(|effect| {
            let style = if effect.starts_with("Destructive") {
                theme::error()
            } else if effect.starts_with("External")
                || effect.starts_with("Network")
                || effect.starts_with("Shell")
                || effect.starts_with("Self-update")
            {
                theme::warning()
            } else if effect.starts_with("Tier:") || effect.starts_with("Command:") {
                theme::muted()
            } else {
                Style::default()
            };
            effect
                .lines()
                .map(|line| Line::styled(clean(line), style))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn block(title: String, focused: bool) -> Block<'static> {
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_set(theme::border_set(theme::is_ascii()))
        .title(title);
    if focused {
        block = block.border_style(accent());
    }
    block
}

fn modal_block(title: String) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_set(theme::border_set(theme::is_ascii()))
        .border_style(accent())
        .title(title)
        .title_top(Line::from(Span::styled(" [Esc] ", theme::muted())).right_aligned())
}

fn render_cursor_spans(prefix: &str, text: &str, cursor_chars: usize) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let cur = cursor_chars.min(chars.len());
    let before: String = chars[..cur].iter().collect();
    let at: String = if cur < chars.len() {
        chars[cur].to_string()
    } else {
        " ".to_string()
    };
    let after: String = if cur < chars.len() {
        chars[cur + 1..].iter().collect()
    } else {
        String::new()
    };
    let mut spans = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::raw(prefix.to_string()));
    }
    if !before.is_empty() {
        spans.push(Span::raw(before));
    }
    spans.push(Span::styled(at, theme::cursor()));
    if !after.is_empty() {
        spans.push(Span::raw(after));
    }
    spans
}

fn push_chip(
    spans: &mut Vec<Span<'static>>,
    cursor_x: &mut u16,
    y: u16,
    max_x: u16,
    label: String,
    style: Style,
) -> Option<Rect> {
    let span = Span::styled(label, style);
    let w = span.width() as u16;
    if cursor_x.saturating_add(w) > max_x {
        spans.push(span);
        *cursor_x = cursor_x.saturating_add(w);
        return None;
    }
    let rect = Rect::new(*cursor_x, y, w, 1);
    spans.push(span);
    *cursor_x = cursor_x.saturating_add(w);
    Some(rect)
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    app.regions = LayoutRegions::default();
    let area = frame.area();
    app.small = area.width < 60 || area.height < 16;
    app.wide = area.width >= 80;
    if app.small {
        frame.render_widget(
            Paragraph::new(format!(
                "numan - resize to at least 60 x 16\n{}\n[q] Quit when idle",
                clean(&app.status)
            ))
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(area);

    // Line 1 of header: `numan` + [1]..[5] top tabs + Nu badge + spinner + [Ctrl+P]
    let header_area = sections[0];
    let mut tab_spans = Vec::new();
    let mut x = header_area.x;
    let max_x = header_area.x.saturating_add(header_area.width);

    let brand = "numan ";
    let brand_w = brand.len() as u16;
    tab_spans.push(Span::styled(brand, accent()));
    x = x.saturating_add(brand_w);

    for (idx, view) in View::ALL.iter().enumerate() {
        let label = if header_area.width >= 108 {
            let count_suffix = app
                .count_for_view(*view)
                .map(|c| format!(" ({c})"))
                .unwrap_or_default();
            format!(" [{}]{}{} ", idx + 1, view.title(), count_suffix)
        } else if header_area.width >= 80 {
            let count_suffix = match view {
                View::Installed | View::Discover | View::Updates => app
                    .count_for_view(*view)
                    .map(|c| format!("({c})"))
                    .unwrap_or_default(),
                View::Commands | View::System => String::new(),
            };
            format!("[{}]{}{}", idx + 1, view.title(), count_suffix)
        } else {
            let short = match view {
                View::Installed => "Inst",
                View::Discover => "Disc",
                View::Updates => "Upd",
                View::Commands => "Cmds",
                View::System => "Sys",
            };
            format!("[{}]{short}", idx + 1)
        };
        let style = theme::tab_style(app.view == *view);
        if let Some(rect) = push_chip(&mut tab_spans, &mut x, header_area.y, max_x, label, style) {
            app.regions.tabs[idx] = rect;
        }
        if idx + 1 < View::ALL.len() {
            tab_spans.push(Span::raw(" "));
            x = x.saturating_add(1);
        }
    }

    let nu_label = app
        .data
        .as_ref()
        .and_then(|d| d.nu.as_ref())
        .map(|nu| {
            format!(
                "Nu {}{}",
                nu.nu_version,
                if app.data.as_ref().is_some_and(|d| !d.nu_valid) {
                    "!"
                } else {
                    ""
                }
            )
        })
        .unwrap_or_else(|| "Nu missing".into());
    let nu_style = if app.data.as_ref().is_some_and(|d| d.nu_valid) {
        theme::muted()
    } else {
        theme::warning()
    };
    if x.saturating_add(nu_label.len() as u16 + 2) <= max_x {
        tab_spans.push(Span::raw("  "));
        x = x.saturating_add(2);
        tab_spans.push(Span::styled(nu_label.clone(), nu_style));
        x = x.saturating_add(nu_label.len() as u16);
    }
    if app.busy {
        let spin = theme::SPINNER_FRAMES[app.spinner_tick % theme::SPINNER_FRAMES.len()];
        let work_label = if x.saturating_add(10) <= max_x {
            format!("  {spin} working")
        } else {
            format!(" {spin}")
        };
        tab_spans.push(Span::styled(work_label, theme::warning()));
    } else if x.saturating_add(18) <= max_x {
        tab_spans.push(Span::styled("  [Ctrl+P] Palette", theme::muted()));
    }

    // Line 2 of header: Filter chip + Facet chips + Medium-width pane toggle
    let sub_y = header_area.y + 1;
    let mut sub_spans = Vec::new();
    let mut sx = header_area.x;
    let filter_label = if app.filter.is_empty() {
        if header_area.width >= 75 {
            " / Filter (/) ".to_string()
        } else {
            "/ Filter".to_string()
        }
    } else {
        format!(" / {} ", clean(&app.filter))
    };
    app.regions.filter_chip = push_chip(
        &mut sub_spans,
        &mut sx,
        sub_y,
        max_x,
        filter_label,
        if app.filter.is_empty() {
            theme::muted()
        } else {
            accent()
        },
    );
    if !app.filter.is_empty() {
        sub_spans.push(Span::raw(" "));
        sx = sx.saturating_add(1);
        app.regions.clear_filter_chip = push_chip(
            &mut sub_spans,
            &mut sx,
            sub_y,
            max_x,
            "[Esc Clear]".into(),
            theme::warning(),
        );
    }

    if !app.wide {
        sub_spans.push(Span::raw("  "));
        sx = sx.saturating_add(2);
        let toggle_label = if header_area.width >= 72 {
            if app.focus == Focus::List {
                "[Tab] Show Details"
            } else {
                "[Tab] Back to List"
            }
        } else if app.focus == Focus::List {
            "[Tab] Details"
        } else {
            "[Tab] List"
        };
        app.regions.pane_toggle_chip = push_chip(
            &mut sub_spans,
            &mut sx,
            sub_y,
            max_x,
            toggle_label.into(),
            accent(),
        );
    }

    match app.view {
        View::Discover => {
            sub_spans.push(Span::raw("  "));
            sx = sx.saturating_add(2);
            let type_label = if header_area.width >= 72 {
                format!("[t] Type: {}", app.type_filter.label())
            } else {
                format!("[t] {}", app.type_filter.label())
            };
            app.regions.type_chip = push_chip(
                &mut sub_spans,
                &mut sx,
                sub_y,
                max_x,
                type_label,
                if app.type_filter == super::app::TypeFilter::All {
                    theme::muted()
                } else {
                    accent()
                },
            );
            sub_spans.push(Span::raw("  "));
            sx = sx.saturating_add(2);
            let compat_label = if header_area.width >= 72 {
                if app.show_all {
                    "[c] All compat"
                } else {
                    "[c] Compatible"
                }
            } else if app.show_all {
                "[c] All"
            } else {
                "[c] Compat"
            };
            app.regions.compat_chip = push_chip(
                &mut sub_spans,
                &mut sx,
                sub_y,
                max_x,
                compat_label.into(),
                if app.show_all {
                    theme::warning()
                } else {
                    theme::muted()
                },
            );
        }
        View::Updates => {
            let count = app.rows().len();
            sub_spans.push(Span::raw("  "));
            sx = sx.saturating_add(2);
            let batch_label = if header_area.width >= 72 {
                format!("[U] Review Update All ({count})")
            } else {
                format!("[U] Update All ({count})")
            };
            app.regions.batch_update_chip = push_chip(
                &mut sub_spans,
                &mut sx,
                sub_y,
                max_x,
                batch_label,
                if count > 0 { accent() } else { theme::muted() },
            );
        }
        View::System => {
            sub_spans.push(Span::styled(
                format!("  Root: {}", app.root.display()),
                theme::muted(),
            ));
        }
        _ => {}
    }

    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(tab_spans),
            Line::from(sub_spans),
        ])),
        header_area,
    );

    // Main panes: 58/42 split when wide (>= 80 cols), single full-width pane when 60..=79 cols.
    let content = sections[1];
    let panes = if app.wide {
        Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).split(content)
    } else {
        Layout::horizontal([Constraint::Percentage(100), Constraint::Length(0)]).split(content)
    };

    let rows = app.rows();
    app.selected = app.selected.min(rows.len().saturating_sub(1));

    if app.view == View::Commands {
        let specs = app.command_specs();
        app.command_selected = app.command_selected.min(specs.len().saturating_sub(1));
        if app.wide || app.focus != Focus::Details {
            let list_area = panes[0];
            app.regions.list_pane = Some(list_area);
            let list_block = block(
                format!("Commands ({}) | / {}", specs.len(), clean(&app.filter)),
                app.focus == Focus::List,
            );
            let inner = list_block.inner(list_area);
            let table_rows = specs.iter().map(|spec| {
                Row::new([
                    Cell::from(clean(&spec.display)),
                    Cell::from(Line::from(theme::pill_span(
                        &clean(spec.tier),
                        theme::tier_tone(spec.tier),
                    ))),
                    Cell::from(clean(spec.phase)),
                ])
            });
            let selected = (!specs.is_empty()).then_some(app.command_selected);
            let mut state = TableState::default()
                .with_selected(selected)
                .with_offset(app.list_offset);
            let table = Table::new(
                table_rows,
                [
                    Constraint::Percentage(38),
                    Constraint::Percentage(32),
                    Constraint::Percentage(30),
                ],
            )
            .header(Row::new(["Command", "Tier", "Phase"]).style(theme::header()))
            .block(list_block)
            .row_highlight_style(accent())
            .highlight_symbol("> ");
            frame.render_stateful_widget(table, list_area, &mut state);
            app.list_offset = state.offset();
            app.regions.list_offset = state.offset();
            if inner.height > 1 {
                app.regions.list_rows_area = Some(Rect::new(
                    inner.x,
                    inner.y + 1,
                    inner.width,
                    inner.height - 1,
                ));
            }
            render_scrollbar(frame, list_area, specs.len(), app.command_selected);
            if specs.is_empty() {
                frame.render_widget(
                    Paragraph::new("\n\nNo commands match the filter.").wrap(Wrap { trim: false }),
                    inner,
                );
            }
        }
        if app.wide || app.focus == Focus::Details {
            let target = if app.wide { panes[1] } else { panes[0] };
            app.regions.details_pane = Some(target);
            let details = specs
                .get(app.command_selected)
                .map(|spec| {
                    let fields = if spec.fields.iter().all(|f| f.hidden) {
                        "  (none)".into()
                    } else {
                        spec.fields
                            .iter()
                            .filter(|field| !field.hidden)
                            .map(|field| {
                                format!(
                                    "  {}{} ({:?})",
                                    field.id,
                                    if field.required { " *" } else { "" },
                                    field.kind
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    format!(
                        "numan {}\n\n{}\n\nTier: {}\nPhase: {}\n\nFields (* = required):\n{}\n\nEnter: open guided form (Left/Right pickers; Enter edits text). Read-only commands run immediately; mutating commands open confirmation review.",
                        spec.display, spec.summary, spec.tier, spec.phase, fields
                    )
                })
                .unwrap_or_else(|| "No command selected.".into());
            frame.render_widget(
                Paragraph::new(clean(&details))
                    .block(block("Command details".into(), app.focus == Focus::Details))
                    .wrap(Wrap { trim: false })
                    .scroll((app.detail_scroll, 0)),
                target,
            );
        }
    } else {
        if app.wide || app.focus != Focus::Details {
            let list_area = panes[0];
            app.regions.list_pane = Some(list_area);
            let title_text = match app.view {
                View::Installed => format!("Installed ({})", rows.len()),
                View::Discover => format!("Discover ({})", rows.len()),
                View::Updates => format!("Updates ({})", rows.len()),
                View::System => "System".into(),
                View::Commands => unreachable!(),
            };
            let list_block = block(
                if app.filter.is_empty() {
                    title_text
                } else {
                    format!("{title_text} | / {}", clean(&app.filter))
                },
                app.focus == Focus::List,
            );
            let inner = list_block.inner(list_area);
            let (headers, widths): ([&str; 4], [Constraint; 4]) = if app.view == View::System {
                (
                    ["Section", "Info", "Area", "Status"],
                    [
                        Constraint::Percentage(34),
                        Constraint::Percentage(22),
                        Constraint::Percentage(14),
                        Constraint::Percentage(30),
                    ],
                )
            } else {
                (
                    ["Package", "Version", "Type", "State"],
                    [
                        Constraint::Percentage(42),
                        Constraint::Percentage(22),
                        Constraint::Percentage(13),
                        Constraint::Percentage(23),
                    ],
                )
            };
            let table_rows = rows.iter().map(|row| {
                Row::new([
                    Cell::from(clean(&row.id)),
                    Cell::from(clean(&row.version)),
                    Cell::from(clean(&row.kind)),
                    Cell::from(Line::from(theme::pill_span(
                        &clean(&row.state),
                        theme::status_tone(&row.state),
                    ))),
                ])
            });
            let selected = (!rows.is_empty()).then_some(app.selected);
            let mut state = TableState::default()
                .with_selected(selected)
                .with_offset(app.list_offset);
            let table = Table::new(table_rows, widths)
                .header(Row::new(headers).style(theme::header()))
                .block(list_block)
                .row_highlight_style(accent())
                .highlight_symbol("> ");
            frame.render_stateful_widget(table, list_area, &mut state);
            app.list_offset = state.offset();
            app.regions.list_offset = state.offset();
            if inner.height > 1 {
                app.regions.list_rows_area = Some(Rect::new(
                    inner.x,
                    inner.y + 1,
                    inner.width,
                    inner.height - 1,
                ));
            }
            render_scrollbar(frame, list_area, rows.len(), app.selected);
            if rows.is_empty() {
                let message = if app.busy {
                    "Loading..."
                } else if app.view == View::Discover {
                    "No cached results. Press [Shift+R] to sync registries or [c]/[t] to widen filters."
                } else if app.view == View::Updates {
                    "No newer compatible versions in cached metadata."
                } else if app.view == View::System {
                    "No system rows match filter."
                } else {
                    "No installed packages. Press [2] Discover or [Ctrl+P] Palette."
                };
                frame.render_widget(
                    Paragraph::new(format!("\n\n{message}")).wrap(Wrap { trim: false }),
                    inner,
                );
            }
        }
        if app.wide || app.focus == Focus::Details {
            let target = if app.wide { panes[1] } else { panes[0] };
            app.regions.details_pane = Some(target);
            let panel_title = if app.view == View::System {
                "System details"
            } else {
                "Package details"
            };
            let detail_block = block(panel_title.into(), app.focus == Focus::Details);
            let inner = detail_block.inner(target);
            frame.render_widget(detail_block, target);

            let selected_row = rows.get(app.selected);
            let action_specs: Vec<(String, DetailClick, Style)> = if let Some(row) = selected_row {
                build_detail_buttons(app, row)
            } else {
                Vec::new()
            };

            let chunks = if !action_specs.is_empty() && inner.height > 2 {
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner)
            } else {
                Layout::vertical([Constraint::Min(1), Constraint::Length(0)]).split(inner)
            };

            let detail_text = selected_row
                .map(|row| {
                    Text::from(vec![
                        Line::from(vec![
                            Span::styled(clean(&row.id), theme::header()),
                            Span::raw("  "),
                            Span::raw(clean(&row.version)),
                            Span::raw("  "),
                            theme::pill_span(&clean(&row.state), theme::status_tone(&row.state)),
                        ]),
                        Line::raw(""),
                    ])
                })
                .map(|mut t| {
                    if let Some(row) = selected_row {
                        for line in clean(&row.details).lines() {
                            t.lines.push(Line::raw(line.to_string()));
                        }
                    }
                    t
                })
                .unwrap_or_else(|| {
                    Text::from(
                        "Nothing selected.\n\n1-5: switch views\nTab: toggle List / Details\nCtrl+P: universal fuzzy palette",
                    )
                });

            frame.render_widget(
                Paragraph::new(detail_text)
                    .wrap(Wrap { trim: false })
                    .scroll((app.detail_scroll, 0)),
                chunks[0],
            );

            if !action_specs.is_empty() && chunks[1].height == 1 {
                let mut btn_spans = Vec::new();
                let mut bx = chunks[1].x;
                let max_bx = chunks[1].x.saturating_add(chunks[1].width);
                for (idx, (label, click, style)) in action_specs.into_iter().enumerate() {
                    if idx > 0 {
                        btn_spans.push(Span::raw(" "));
                        bx = bx.saturating_add(1);
                    }
                    if let Some(rect) =
                        push_chip(&mut btn_spans, &mut bx, chunks[1].y, max_bx, label, style)
                    {
                        app.regions.detail_buttons.push((rect, click));
                    }
                }
                frame.render_widget(Paragraph::new(Line::from(btn_spans)), chunks[1]);
            }
        }
    }

    // Footer (3 lines): Focus + view keybinds + status or download progress bar
    let footer_area = sections[2];
    let focus_label = match app.focus {
        Focus::List => "List",
        Focus::Details => "Details",
    };
    let view_hints = match app.view {
        View::Installed => "[a] Activate  [d] Deactivate  [x] Remove  [Enter] Actions",
        View::Discover => "[i] Install  [v] Version  [t] Type  [c] Compat",
        View::Updates => "[u] Update  [U] Update All  [d] Deactivate  [x] Remove",
        View::Commands => "[Enter] Open guided form  [/] Filter commands",
        View::System => "[s] Local doctor scan  [Shift+R] Review sync  [r] Reload",
    };
    let line1 = Line::from(vec![Span::styled(
        format!(
            "Focus: {focus_label}{} | 1-5: tabs | Tab: pane | /: filter | Ctrl+P: palette | ?: help | q: quit",
            if app.busy {
                " (busy)"
            } else {
                ""
            }
        ),
        accent(),
    )]);
    let line2 = Line::styled(
        format!("{view_hints}  [l] Transcript  [n] Context"),
        theme::muted(),
    );

    if let Some((received, total)) = app.download_progress {
        let footer_chunks =
            Layout::vertical([Constraint::Length(2), Constraint::Length(1)]).split(footer_area);
        frame.render_widget(
            Paragraph::new(Text::from(vec![line1, line2])),
            footer_chunks[0],
        );
        match total {
            Some(total_bytes) if total_bytes > 0 => {
                let ratio = (received as f64 / total_bytes as f64).clamp(0.0, 1.0);
                let pct = (ratio * 100.0).round() as u16;
                frame.render_widget(
                    Gauge::default()
                        .gauge_style(accent())
                        .ratio(ratio)
                        .label(format!("Downloading {received}/{total_bytes} B ({pct}%)")),
                    footer_chunks[1],
                );
            }
            _ => {
                frame.render_widget(
                    Paragraph::new(Line::styled(
                        format!("Downloading: {received} bytes (size unknown)"),
                        theme::warning(),
                    )),
                    footer_chunks[1],
                );
            }
        }
    } else {
        frame.render_widget(
            Paragraph::new(Text::from(vec![
                line1,
                line2,
                Line::styled(clean(&app.status), theme::status(&app.status)),
            ])),
            footer_area,
        );
    }

    if app.overlay.is_some() {
        render_overlay(frame, app, area);
    }
}

fn build_detail_buttons(app: &App, row: &super::app::Row) -> Vec<(String, DetailClick, Style)> {
    let mut buttons = Vec::new();
    if app.view == View::System {
        match row.id.as_str() {
            "Doctor" => buttons.push(("[s] Scan".into(), DetailClick::Scan, accent())),
            "Registries" => buttons.push(("[Shift+R] Sync".into(), DetailClick::Sync, accent())),
            "Snapshots" => {
                if let Some(latest) = app.data.as_ref().and_then(|d| d.snapshots.first()) {
                    buttons.push((
                        "Rollback latest".into(),
                        DetailClick::RollbackSnapshot(latest.id.clone()),
                        theme::error(),
                    ));
                }
            }
            _ => {}
        }
        return buttons;
    }
    let actions = app.available_actions(row);
    match app.view {
        View::Discover => {
            if actions.contains(&Action::Install) {
                buttons.push((
                    "[i] Install".into(),
                    DetailClick::Prepare(Action::Install),
                    accent(),
                ));
            }
            buttons.push((
                "[v] Version".into(),
                DetailClick::CycleVersion,
                Style::default(),
            ));
        }
        View::Installed | View::Updates => {
            if app.view == View::Updates && actions.contains(&Action::Update) {
                buttons.push((
                    "[u] Update".into(),
                    DetailClick::Prepare(Action::Update),
                    accent(),
                ));
            }
            if app.view == View::Updates {
                buttons.push(("[U] All".into(), DetailClick::BatchUpdate, Style::default()));
            }
            if actions.contains(&Action::Activate) {
                buttons.push((
                    "[a] Activate".into(),
                    DetailClick::Prepare(Action::Activate),
                    theme::ok(),
                ));
            }
            if actions.contains(&Action::Deactivate) {
                buttons.push((
                    "[d] Deactivate".into(),
                    DetailClick::Prepare(Action::Deactivate),
                    theme::warning(),
                ));
            }
            let can_remove = actions.contains(&Action::Remove);
            buttons.push((
                "[x] Remove".into(),
                DetailClick::Prepare(Action::Remove),
                if can_remove {
                    theme::error()
                } else {
                    theme::muted()
                },
            ));
        }
        View::Commands | View::System => {}
    }
    buttons
}

fn render_overlay(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.saturating_sub(4);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + 2,
        width,
        height,
    );
    app.regions.modal_rect = Some(popup);
    if popup.width >= 10 {
        app.regions.modal_close_rect = Some(Rect::new(
            popup.x + popup.width.saturating_sub(8),
            popup.y,
            7,
            1,
        ));
    }
    frame.render_widget(Clear, popup);

    let Some(overlay) = &app.overlay else {
        return;
    };

    match overlay {
        Overlay::Actions { selected } => {
            let title = app
                .selected_row()
                .map(|row| row.id)
                .unwrap_or_else(|| "No package selected".into());
            let border = modal_block(format!(
                "Actions: {} | Up/Down select | Enter review",
                clean(&title)
            ));
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let items = app
                .package_actions()
                .into_iter()
                .map(|(label, _, reason)| {
                    if reason.is_empty() {
                        ListItem::new(Line::from(vec![
                            Span::styled(label, theme::header()),
                            Span::styled("  — opens confirmation review", theme::ok()),
                        ]))
                    } else {
                        ListItem::new(Line::from(vec![
                            Span::styled(format!("{label} [unavailable]"), theme::muted()),
                            Span::styled(format!("  — {reason}"), theme::muted()),
                        ]))
                    }
                })
                .collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(
                List::new(items)
                    .highlight_style(accent())
                    .highlight_symbol("> "),
                inner,
                &mut state,
            );
            app.regions.modal_list_area = Some((inner, state.offset()));
        }
        Overlay::Sync => {
            let border = modal_block("Review registry refresh".into());
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
            frame.render_widget(
                Paragraph::new(
                    "This explicitly contacts configured enabled registries and updates cached indexes under the existing mutation lock. Ed25519 signature verification remains enforced.\n\nNetwork failure may retain verified cached data; inspect the transcript for fallback warnings.",
                )
                .wrap(Wrap { trim: false }),
                parts[0],
            );
            render_action_pair(
                frame,
                &mut app.regions,
                parts[1],
                "[ Cancel (Esc) ]",
                ModalClick::Cancel,
                "[ Refresh now (Enter) ]",
                ModalClick::Confirm,
                true,
                theme::warning(),
            );
        }
        Overlay::Filter(query) => {
            let border = modal_block("Focus: Filter input | Enter apply".into());
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let mut spans = vec![Span::styled("/ ", accent())];
            spans.push(Span::raw(clean(query)));
            spans.push(Span::styled(" ", theme::cursor()));
            frame.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::from(spans),
                    Line::raw(""),
                    Line::styled(
                        "Type to filter rows in the active tab. Enter applies; Esc cancels.",
                        theme::muted(),
                    ),
                ]))
                .wrap(Wrap { trim: false }),
                inner,
            );
        }
        Overlay::Launcher { query, selected } => {
            let border = modal_block(
                "Ctrl+P Palette (Fuzzy: Recent, Packages, 42 Commands & Presets)".into(),
            );
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);
            let items_data = app.launcher_items(query);
            let meta = if query.trim().is_empty() {
                "Recent runs, selected-package actions & presets".into()
            } else {
                format!("{} fuzzy match(es)", items_data.len())
            };
            let mut q_spans = vec![Span::styled("> ", accent())];
            q_spans.push(Span::raw(clean(query)));
            q_spans.push(Span::styled(" ", theme::cursor()));
            q_spans.push(Span::styled(format!("   ({meta})"), theme::muted()));
            frame.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::from(q_spans),
                    Line::styled(
                        "Up/Down select • Enter jumps, fills form in-place, or runs ReadOnly",
                        theme::muted(),
                    ),
                ])),
                parts[0],
            );
            if items_data.is_empty() {
                frame.render_widget(
                    Paragraph::new("No matching packages, commands, snapshots, or presets.")
                        .style(theme::muted()),
                    parts[1],
                );
            } else {
                let sel_clamped = (*selected).min(items_data.len().saturating_sub(1));
                let list_items: Vec<ListItem> = items_data
                    .iter()
                    .enumerate()
                    .map(|(idx, item)| {
                        let is_sel = idx == sel_clamped;
                        let title_style = if is_sel {
                            theme::header().add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        };
                        ListItem::new(Line::from(vec![
                            Span::styled(clean(&item.label), title_style),
                            Span::raw(" "),
                            theme::pill_span(&clean(&item.tag), item.tone),
                            Span::styled(format!("  {}", clean(&item.sub)), theme::muted()),
                        ]))
                    })
                    .collect();
                let mut state = ListState::default().with_selected(Some(sel_clamped));
                frame.render_stateful_widget(
                    List::new(list_items)
                        .highlight_style(accent())
                        .highlight_symbol("> "),
                    parts[1],
                    &mut state,
                );
                app.regions.modal_list_area = Some((parts[1], state.offset()));
                render_scrollbar(frame, parts[1], items_data.len(), sel_clamped);
            }
        }
        Overlay::Review {
            review,
            confirm,
            scroll,
        } => {
            let border = modal_block(format!(
                "Focus: Review form | {:?} {}",
                review.action,
                clean(&review.id)
            ));
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
            frame.render_widget(
                Paragraph::new(Text::from(effect_lines(&review.effects)))
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                parts[0],
            );
            render_confirm_footer(frame, &mut app.regions, parts[1], *confirm);
        }
        Overlay::CommandForm(form) => {
            let border = modal_block(format!(
                "Command form: numan {} | Left/Right picker | Enter edit/toggle",
                clean(&form.display)
            ));
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([
                Constraint::Min(1),
                Constraint::Length(5),
                Constraint::Length(3),
            ])
            .split(inner);
            let mut edited_row = None;
            let mut lines: Vec<Line<'static>> = form
                .fields
                .iter()
                .enumerate()
                .flat_map(|(index, field)| {
                    let marker = if index == form.selected { ">" } else { " " };
                    let marker_style = if index == form.selected {
                        accent()
                    } else {
                        Style::default()
                    };
                    let editing = form.editing == Some(EditTarget::Field(index));
                    let picker_opts = app.picker_options(form, index);
                    let required = if field.spec.required { " *" } else { "" };
                    let mut spans = vec![
                        Span::styled(format!("{marker} "), marker_style),
                        Span::styled(format!("{}{required}: ", field.spec.id), theme::header()),
                    ];

                    if editing {
                        let cursor_end = Line::from(spans.clone()).width()
                            + Span::raw(
                                form.edit_buffer
                                    .chars()
                                    .take(form.edit_cursor)
                                    .collect::<String>(),
                            )
                            .width()
                            + form
                                .edit_buffer
                                .chars()
                                .nth(form.edit_cursor)
                                .map(|c| Span::raw(c.to_string()).width())
                                .unwrap_or(1)
                                .max(1);
                        spans.extend(render_cursor_spans("", &form.edit_buffer, form.edit_cursor));
                        edited_row = Some((
                            index,
                            cursor_end,
                            form.edit_buffer.clone(),
                            form.edit_cursor,
                        ));
                    } else if field.spec.kind == FieldKind::Bool {
                        spans.push(Span::raw(if field.enabled { "[x]" } else { "[ ]" }));
                    } else if !picker_opts.is_empty() {
                        let shown = if field.value.is_empty() {
                            "(unset)"
                        } else {
                            &field.value
                        };
                        let idx_hint = picker_opts
                            .iter()
                            .position(|o| o == &field.value)
                            .map(|pos| format!(" ({}/{})", pos + 1, picker_opts.len()))
                            .unwrap_or_else(|| format!(" (0/{})", picker_opts.len()));
                        spans.push(Span::raw(format!("< {} >{idx_hint}", clean(shown))));
                    } else {
                        spans.push(Span::raw(if field.value.is_empty() {
                            "(empty)".to_string()
                        } else {
                            clean(&field.value)
                        }));
                    }

                    let picker_tip = if !picker_opts.is_empty() && !editing {
                        " [Left/Right: pick, Enter: edit]"
                    } else {
                        ""
                    };
                    vec![
                        Line::from(spans),
                        Line::styled(
                            format!("    {}{picker_tip}", clean(&field.spec.label)),
                            theme::muted(),
                        ),
                    ]
                })
                .collect();
            let preview_marker = if form.selected == form.fields.len() {
                ">"
            } else {
                " "
            };
            lines.push(Line::styled(
                format!("{preview_marker} <preview row>"),
                if form.selected == form.fields.len() {
                    accent()
                } else {
                    Style::default()
                },
            ));
            {
                let execute_marker = if form.selected == form.fields.len() + 1 {
                    ">"
                } else {
                    " "
                };
                let label = match super::catalog::execution_mode(&form.display, &form.fields) {
                    super::catalog::ExecutionMode::ReadOnly => {
                        "<execute read-only>  (Enter runs it; output opens as a result)"
                    }
                    super::catalog::ExecutionMode::Reviewed => {
                        "<review and execute>  (Enter opens a confirmation review)"
                    }
                    super::catalog::ExecutionMode::Blocked(_) => "<execution unavailable in TUI>",
                };
                lines.push(Line::styled(
                    format!("{execute_marker} {label}"),
                    if form.selected == form.fields.len() + 1 {
                        theme::error()
                    } else {
                        theme::muted()
                    },
                ));
            }
            let content_lines = form.fields.len() as u32 * 2 + 2;
            let max_scroll = content_lines.saturating_sub(parts[0].height as u32);
            let field_scroll = (form.selected as u32 * 2)
                .saturating_sub(u32::from(parts[0].height.saturating_sub(1)))
                .min(max_scroll);
            let scroll_u16 = field_scroll.min(u32::from(u16::MAX)) as u16;
            frame.render_widget(
                Paragraph::new(Text::from(lines)).scroll((scroll_u16, 0)),
                parts[0],
            );

            if let Some((index, cursor_end, edit_buffer, edit_cursor)) = edited_row {
                let row = index as u32 * 2;
                let width = usize::from(parts[0].width);
                if cursor_end > width
                    && width > 0
                    && row >= field_scroll
                    && row - field_scroll < u32::from(parts[0].height)
                {
                    let chars: Vec<char> = edit_buffer.chars().collect();
                    let cur = edit_cursor.min(chars.len());
                    let at = if cur < chars.len() {
                        chars[cur].to_string()
                    } else {
                        " ".to_string()
                    };
                    let at_width = Span::raw(&at).width().max(1);
                    let after: String = if cur < chars.len() {
                        chars[cur + 1..].iter().collect()
                    } else {
                        String::new()
                    };
                    let mut tail_chars = Vec::new();
                    let mut used = 1 + at_width; // 1 for leading ellipsis
                    for &ch in chars[..cur].iter().rev() {
                        let cw = Span::raw(ch.to_string()).width();
                        if used + cw > width {
                            break;
                        }
                        used += cw;
                        tail_chars.push(ch);
                    }
                    tail_chars.reverse();
                    let before_tail: String = tail_chars.into_iter().collect();
                    let mut scrolled = vec![Span::styled("…", theme::muted())];
                    if !before_tail.is_empty() {
                        scrolled.push(Span::raw(before_tail));
                    }
                    scrolled.push(Span::styled(at, theme::cursor()));
                    if !after.is_empty() {
                        scrolled.push(Span::raw(after));
                    }
                    let row_rect = Rect::new(
                        parts[0].x,
                        parts[0].y + (row - field_scroll) as u16,
                        parts[0].width,
                        1,
                    );
                    frame.render_widget(Clear, row_rect);
                    frame.render_widget(Line::from(scrolled), row_rect);
                }
            }

            // Record visible clickable rects for field/preview/execute rows.
            for idx in 0..=form.fields.len() + 1 {
                let line_y = if idx <= form.fields.len() {
                    idx as u16 * 2
                } else {
                    form.fields.len() as u16 * 2 + 1
                };
                if line_y >= scroll_u16 {
                    let rel_y = line_y - scroll_u16;
                    if rel_y < parts[0].height {
                        app.regions.modal_buttons.push((
                            Rect::new(parts[0].x, parts[0].y + rel_y, parts[0].width, 1),
                            ModalClick::FieldRow(idx),
                        ));
                    }
                }
            }

            let preview_line = if form.editing == Some(EditTarget::Preview) {
                Line::from(render_cursor_spans("", &form.edit_buffer, form.edit_cursor))
            } else {
                Line::raw(clean(&app.command_preview(form)))
            };
            frame.render_widget(
                Paragraph::new(Text::from(vec![
                    Line::styled(
                        "Preview (Enter on preview row to edit; edits are re-parsed, never run as shell text):",
                        theme::muted(),
                    ),
                    preview_line,
                ]))
                .wrap(Wrap { trim: false }),
                parts[1],
            );
            let status = match (&form.error, &form.notice) {
                (Some(error), _) => format!("Error: {}", clean(error)),
                (None, Some(notice)) => clean(notice),
                (None, None) => String::new(),
            };
            let status_style = if form.error.is_some() {
                theme::error()
            } else {
                theme::muted()
            };
            frame.render_widget(
                Paragraph::new(Line::styled(status, status_style)).wrap(Wrap { trim: false }),
                parts[2],
            );
        }
        Overlay::GenericReview {
            argv: _,
            display,
            effects,
            confirm,
            scroll,
        } => {
            let border = modal_block(format!("Focus: Review form | numan {display}"));
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
            frame.render_widget(
                Paragraph::new(Text::from(effect_lines(effects)))
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0)),
                parts[0],
            );
            render_confirm_footer(frame, &mut app.regions, parts[1], *confirm);
        }
        Overlay::Text {
            title,
            content,
            scroll,
            follow_up,
        } => {
            let border = modal_block(format!("Focus: {} | arrows scroll", clean(title)));
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            if let Some(fu) = follow_up {
                let parts =
                    Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
                frame.render_widget(
                    Paragraph::new(content.as_str())
                        .wrap(Wrap { trim: false })
                        .scroll((*scroll, 0)),
                    parts[0],
                );
                let mut f_spans = Vec::new();
                let mut fx = parts[1].x;
                let max_fx = parts[1].x.saturating_add(parts[1].width);
                if let Some(rect) = push_chip(
                    &mut f_spans,
                    &mut fx,
                    parts[1].y,
                    max_fx,
                    "[ Close (Esc) ]".into(),
                    theme::muted(),
                ) {
                    app.regions.modal_buttons.push((rect, ModalClick::Close));
                }
                f_spans.push(Span::raw("   "));
                fx = fx.saturating_add(3);
                if let Some(rect) = push_chip(
                    &mut f_spans,
                    &mut fx,
                    parts[1].y,
                    max_fx,
                    format!("[ {} ]", clean(&fu.label)),
                    accent(),
                ) {
                    app.regions
                        .modal_buttons
                        .push((rect, ModalClick::FollowUp((*fu.command).clone())));
                }
                frame.render_widget(
                    Paragraph::new(Text::from(vec![
                        Line::from(f_spans),
                        Line::styled(
                            format!(
                                "Press '{}' to trigger follow-up review or Esc to close.",
                                fu.key
                            ),
                            theme::muted(),
                        ),
                    ])),
                    parts[1],
                );
            } else {
                frame.render_widget(
                    Paragraph::new(content.as_str())
                        .wrap(Wrap { trim: false })
                        .scroll((*scroll, 0)),
                    inner,
                );
            }
        }
        Overlay::Handoff { args, shell } => {
            let border = modal_block("Focus: CLI-only command".into());
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).split(inner);
            frame.render_widget(
                Paragraph::new(format!(
                    "This action runs outside the TUI. Nothing will be executed.\n\nShell: {shell:?} (Tab changes shell)\n\n{}\n\nThe command prints only after the terminal is restored. Diagnostics go to stderr.",
                    clean(&app.handoff(args, *shell))
                ))
                .wrap(Wrap { trim: false }),
                parts[0],
            );
            render_action_pair(
                frame,
                &mut app.regions,
                parts[1],
                "[ Change Shell (Tab) ]",
                ModalClick::CycleShell,
                "[ Print & Exit (Enter) ]",
                ModalClick::Confirm,
                true,
                accent(),
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_action_pair(
    frame: &mut Frame<'_>,
    regions: &mut LayoutRegions,
    area: Rect,
    left_label: &str,
    left_click: ModalClick,
    right_label: &str,
    right_click: ModalClick,
    right_active: bool,
    right_style: Style,
) {
    let mut spans = Vec::new();
    let mut x = area.x;
    let max_x = area.x.saturating_add(area.width);
    if let Some(rect) = push_chip(
        &mut spans,
        &mut x,
        area.y,
        max_x,
        left_label.to_string(),
        if right_active {
            theme::muted()
        } else {
            accent()
        },
    ) {
        regions.modal_buttons.push((rect, left_click));
    }
    spans.push(Span::raw("   "));
    x = x.saturating_add(3);
    if let Some(rect) = push_chip(
        &mut spans,
        &mut x,
        area.y,
        max_x,
        right_label.to_string(),
        right_style,
    ) {
        regions.modal_buttons.push((rect, right_click));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_confirm_footer(
    frame: &mut Frame<'_>,
    regions: &mut LayoutRegions,
    area: Rect,
    confirm: bool,
) {
    let cancel_label = if !confirm {
        ">[ Cancel ]"
    } else {
        "[ Cancel ]"
    };
    let confirm_label = if confirm {
        ">[ Confirm exact request ]"
    } else {
        "[ Confirm exact request ]"
    };
    let mut spans = Vec::new();
    let mut x = area.x;
    let max_x = area.x.saturating_add(area.width);
    if let Some(rect) = push_chip(
        &mut spans,
        &mut x,
        area.y,
        max_x,
        cancel_label.to_string(),
        if confirm { theme::muted() } else { accent() },
    ) {
        regions.modal_buttons.push((rect, ModalClick::Cancel));
    }
    spans.push(Span::raw("    "));
    x = x.saturating_add(4);
    if let Some(rect) = push_chip(
        &mut spans,
        &mut x,
        area.y,
        max_x,
        confirm_label.to_string(),
        if confirm {
            theme::error()
        } else {
            theme::muted()
        },
    ) {
        regions.modal_buttons.push((rect, ModalClick::Confirm));
    }
    frame.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(spans),
            Line::styled(
                "Tab selects; Enter activates; arrows scroll; Esc cancels.",
                theme::muted(),
            ),
        ])),
        area,
    );
}
