use super::app::{clean, App, Focus, Overlay, View};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
    },
    Frame,
};

fn accent() -> Style {
    if std::env::var_os("NO_COLOR").is_some() {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }
}

fn block(title: String, focused: bool) -> Block<'static> {
    let mut block = Block::default().borders(Borders::ALL).title(title);
    if std::env::var_os("NUMAN_TUI_ASCII").as_deref() == Some(std::ffi::OsStr::new("1")) {
        block = block.border_set(ratatui::symbols::border::Set {
            top_left: "+",
            top_right: "+",
            bottom_left: "+",
            bottom_right: "+",
            vertical_left: "|",
            vertical_right: "|",
            horizontal_top: "-",
            horizontal_bottom: "-",
        });
    }
    if focused {
        block = block.border_style(accent());
    }
    block
}

fn title(app: &App) -> &'static str {
    match app.view {
        View::Installed => "Installed",
        View::Discover => "Discover (cached)",
        View::Updates => "Updates (cached)",
    }
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    app.small = area.width < 60 || area.height < 16;
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
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(area);
    let context = app
        .data
        .as_ref()
        .and_then(|d| d.nu.as_ref())
        .map(|nu| format!("Nu {} | {}", nu.nu_version, nu.nu_executable))
        .unwrap_or_else(|| "Nu context unavailable | Ctrl+P > Initialize root (CLI only)".into());
    let heading = format!(
        "numan / {}    [Ctrl+P] Launcher\n{}\nRoot: {}",
        title(app),
        clean(&context),
        app.root.display()
    );
    frame.render_widget(Paragraph::new(heading), sections[0]);

    let content = sections[1];
    let show_nav = content.width >= 120;
    let panes = Layout::horizontal(if show_nav {
        vec![Constraint::Length(16), Constraint::Min(1)]
    } else {
        vec![Constraint::Length(0), Constraint::Min(1)]
    })
    .split(content);
    if show_nav {
        let items = ["Packages", "  Installed", "  Discover", "Updates"]
            .into_iter()
            .map(ListItem::new)
            .collect::<Vec<_>>();
        let mut state = ListState::default().with_selected(Some(match app.view {
            View::Installed => 1,
            View::Discover => 2,
            View::Updates => 3,
        }));
        frame.render_stateful_widget(
            List::new(items)
                .block(block("Navigate".into(), app.focus == Focus::Navigation))
                .highlight_style(accent())
                .highlight_symbol("> "),
            panes[0],
            &mut state,
        );
    }
    let wide = panes[1].width >= 80;
    let regions = if wide {
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).split(panes[1])
    } else {
        Layout::horizontal([Constraint::Percentage(100), Constraint::Length(0)]).split(panes[1])
    };
    let rows = app.rows();
    app.selected = app.selected.min(rows.len().saturating_sub(1));
    if wide || app.focus != Focus::Details {
        let table_rows = rows.iter().map(|row| {
            Row::new([
                clean(&row.id),
                clean(&row.version),
                clean(&row.kind),
                clean(&row.state),
            ])
        });
        let selected = (!rows.is_empty()).then_some(app.selected);
        let mut state = TableState::default().with_selected(selected);
        let table = Table::new(
            table_rows,
            [
                Constraint::Percentage(43),
                Constraint::Percentage(23),
                Constraint::Percentage(13),
                Constraint::Percentage(21),
            ],
        )
        .header(
            Row::new(["Package", "Version", "Type", "State"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .block(block(
            format!("{} | / {}", title(app), clean(&app.filter)),
            app.focus == Focus::List,
        ))
        .row_highlight_style(accent())
        .highlight_symbol("> ");
        frame.render_stateful_widget(table, regions[0], &mut state);
        if rows.is_empty() {
            let inner = block(String::new(), false).inner(regions[0]);
            let message = if app.busy {
                "Loading..."
            } else if app.view == View::Discover {
                "No cached results. Ctrl+P > Sync registries (CLI only)."
            } else if app.view == View::Updates {
                "No newer compatible versions in cached metadata."
            } else {
                "No installed packages. Use Discover from navigation or launcher."
            };
            frame.render_widget(
                Paragraph::new(format!("\n\n{message}")).wrap(Wrap { trim: false }),
                inner,
            );
        }
    }
    if wide || app.focus == Focus::Details {
        let target = if wide { regions[1] } else { regions[0] };
        let details = rows
            .get(app.selected)
            .map(|row| {
                format!(
                    "{}\n{} / {}\n\n{}\n\n{}",
                    row.id,
                    row.version,
                    row.state,
                    row.details,
                    app.available_actions(row)
                        .iter()
                        .map(|action| format!("Review {action:?}"))
                        .collect::<Vec<_>>()
                        .join(" | ")
                )
            })
            .unwrap_or_else(|| {
                "Select a package to inspect it.\n\nTab: focus navigation\nCtrl+P: search actions"
                    .into()
            });
        frame.render_widget(
            Paragraph::new(clean(&details))
                .block(block("Package details".into(), app.focus == Focus::Details))
                .wrap(Wrap { trim: false })
                .scroll((app.detail_scroll, 0)),
            target,
        );
    }
    let actions = app
        .selected_row()
        .map(|row| {
            if app.focus == Focus::Navigation {
                Vec::new()
            } else {
                app.available_actions(&row)
            }
        })
        .unwrap_or_default();
    let hints = actions
        .iter()
        .map(|action| match action {
            super::operations::Action::Install => "[i] Review install",
            super::operations::Action::Activate => "[a] Review activate",
            super::operations::Action::Update => "[u] Review update",
            super::operations::Action::Remove => "[x] Review remove",
            _ => "",
        })
        .collect::<Vec<_>>()
        .join("  ");
    let focus = match app.focus {
        Focus::Navigation => "Navigation",
        Focus::List => "Package list",
        Focus::Details => "Package details",
    };
    let footer = format!(
        "Focus: {focus}{} | Left/Right: pane | Enter: actions | ?: help | q: quit\n{}\n{}",
        if app.busy {
            " (busy; displayed data may be stale)"
        } else {
            ""
        },
        format_args!(
            "{}  {}  [Shift+R] Refresh registries",
            if app.focus == Focus::Navigation {
                "Up/Down: destination"
            } else if app.focus == Focus::List {
                "[/] Filter  [r] Reload locally"
            } else {
                "Up/Down: scroll  [r] Reload locally"
            },
            hints
        ),
        clean(&app.status)
    );
    frame.render_widget(Paragraph::new(footer), sections[2]);
    if let Some(overlay) = &app.overlay {
        render_overlay(frame, app, overlay, area);
    }
}

fn render_overlay(frame: &mut Frame<'_>, app: &App, overlay: &Overlay, area: Rect) {
    let width = area.width.saturating_sub(4).min(100);
    let height = area.height.saturating_sub(4);
    let popup = Rect::new(area.x + (area.width - width) / 2, area.y + 2, width, height);
    frame.render_widget(Clear, popup);
    let (heading, body) = match overlay {
        Overlay::Actions { selected } => {
            let title = app.selected_row().map(|row| row.id).unwrap_or_else(|| "No package selected".into());
            let border = block(format!("Actions: {} | Up/Down select | Enter review | Esc back", clean(&title)), true);
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let items = app.package_actions().iter().map(|(label, _, reason)| ListItem::new(if reason.is_empty() { format!("{label}\n  Available: opens a separate confirmation review.\n") } else { format!("{label} [unavailable]\n  {reason}\n") })).collect::<Vec<_>>();
            let mut state = ListState::default().with_selected(Some(*selected));
            frame.render_stateful_widget(List::new(items).highlight_style(accent()).highlight_symbol("> "), inner, &mut state);
            return;
        }
        Overlay::Sync => ("Review registry refresh | Esc cancel".into(), "This explicitly contacts configured enabled registries and updates cached indexes under the existing mutation lock. Signature verification remains enforced.

Network failure may retain verified cached data; inspect the transcript for fallback warnings.

[Enter] Refresh now    [Esc] Cancel".into()),
        Overlay::Filter(query) => ("Focus: Filter input | Enter apply | Esc cancel".to_string(), format!("/ {}_\n\nAll letters are text in this field.", clean(query))),
        Overlay::Launcher { query, selected } => {
            let border = block("Focus: Launcher input | Esc close".into(), true);
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).split(inner);
            frame.render_widget(Paragraph::new(format!("> {}_", clean(query))), parts[0]);
            let items = app.launcher_items(query).into_iter().map(|item| ListItem::new(clean(&item.label))).collect::<Vec<_>>();
            let mut state = ListState::default().with_selected((!items.is_empty()).then_some(*selected));
            frame.render_stateful_widget(List::new(items).highlight_style(accent()).highlight_symbol("> "), parts[1], &mut state);
            return;
        }
        Overlay::Review { review, confirm, scroll } => {
            let border = block(format!("Focus: Review form | {:?} {}", review.action, clean(&review.id)), true);
            let inner = border.inner(popup);
            frame.render_widget(border, popup);
            let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(inner);
            frame.render_widget(Paragraph::new(clean(&review.effects.join("\n\n"))).wrap(Wrap { trim: false }).scroll((*scroll, 0)), parts[0]);
            let controls = if *confirm { "[ Cancel ]    >[ Confirm exact request ]" } else { ">[ Cancel ]    [ Confirm exact request ]" };
            frame.render_widget(Paragraph::new(format!("{controls}\nTab selects; Enter activates; arrows scroll; Esc cancels.")), parts[1]);
            return;
        }
        Overlay::Text { title, content, scroll } => {
            frame.render_widget(Paragraph::new(content.as_str()).block(block(format!("Focus: {title} | arrows scroll | Esc close"), true)).wrap(Wrap { trim: false }).scroll((*scroll, 0)), popup);
            return;
        }
        Overlay::Handoff { args, shell } => ("Focus: CLI-only command | Esc back".into(), format!("This action runs outside the TUI. Nothing will be executed.\n\nShell: {shell:?} (Tab changes shell)\n\n{}\n\n[Enter] Print command to stdout and exit\n\nThe command prints only after the terminal is restored. Diagnostics go to stderr.", clean(&app.handoff(args, *shell)))),
    };
    frame.render_widget(
        Paragraph::new(body)
            .block(block(heading, true))
            .wrap(Wrap { trim: false }),
        popup,
    );
}
