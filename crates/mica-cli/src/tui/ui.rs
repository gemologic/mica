use crate::tui::app::{
    App, EnvEditMode, EnvValueMode, FilterKind, Focus, Overlay, PackageEntry, PinField,
    PresetEntry, Toast, ToastLevel,
};
use mica_core::state::NIX_EXPR_PREFIX;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap,
};
use ratatui::Frame;

pub fn render(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, app, chunks[0]);
    render_body(frame, app, chunks[1]);
    render_status_bar(frame, app, chunks[2]);

    if let Some(overlay) = &app.overlay {
        render_overlay(frame, app, overlay);
    }

    if let Some(toast) = &app.toast {
        render_toast(frame, toast);
    }
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match app.mode {
        crate::tui::app::AppMode::Project => "project",
        crate::tui::app::AppMode::Global => "global",
    };
    let rev = if app.index_info.rev.is_empty() {
        "unknown".to_string()
    } else {
        short_rev(&app.index_info.rev)
    };
    let index_name = index_display_name(&app.index_info.url);
    let line_one_left = if let (crate::tui::app::AppMode::Project, Some(dir)) =
        (app.mode, app.project_dir.as_ref())
    {
        format!("{} | {}", mode, dir)
    } else {
        mode.to_string()
    };
    let line_one = header_line_with_right_span(&line_one_left, Span::raw("?: help"), area.width);
    let dirty = if app.dirty { "unsaved" } else { "saved" };
    let dirty_style = if app.dirty {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Green)
    };
    let line_two_left = format!("{} @ {}", index_name, rev);
    let line_two = header_line_with_right_span(
        &line_two_left,
        Span::styled(dirty.to_string(), dirty_style),
        area.width,
    );
    let text = Text::from(vec![line_one, line_two]);

    let header = Paragraph::new(text)
        .block(Block::default().title("mica").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(header, area);
}

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mode = match app.mode {
        crate::tui::app::AppMode::Project => "project",
        crate::tui::app::AppMode::Global => "global",
    };
    let focus = match app.focus {
        Focus::Packages => "packages",
        Focus::Presets => "templates",
        Focus::Changes => "changes",
    };
    let rev = if app.index_info.rev.is_empty() {
        "unknown".to_string()
    } else {
        short_rev(&app.index_info.rev)
    };
    let count = app
        .index_info
        .count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let generated = app
        .index_info
        .generated_at
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let installed = app.effective_package_count();
    let status = format!(
        "mode: {} | focus: {} | index {} | {} pkgs | installed {} | pulled {}",
        mode, focus, rev, count, installed, generated
    );

    let bar = Paragraph::new(status)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .alignment(Alignment::Left);
    frame.render_widget(bar, area);
}

fn render_body(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.presets_collapsed {
        let right = if app.changes_collapsed {
            Constraint::Length(10)
        } else {
            Constraint::Percentage(30)
        };
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), right])
            .split(area);

        render_package_column(frame, app, columns[0]);

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(0)])
            .split(columns[1]);

        render_templates_banner(frame, app, right[0]);
        render_changes_column(frame, app, right[1]);
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(body_constraints(app))
            .split(area);

        render_package_column(frame, app, columns[0]);

        render_presets_column(frame, app, columns[1]);
        render_changes_column(frame, app, columns[2]);
    }
}

fn render_package_column(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut constraints = vec![Constraint::Length(3), Constraint::Min(0)];
    if app.show_details {
        constraints.push(Constraint::Length(7));
    }
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    render_package_search(frame, app, layout[0]);
    render_package_table(frame, app, layout[1]);
    if app.show_details {
        render_package_details(frame, app, layout[2]);
    }
}

fn render_package_search(frame: &mut Frame, app: &App, area: Rect) {
    let mut filters = Vec::new();
    if !app.filters.license.is_empty() {
        filters.push(format!("license={}", app.filters.license));
    }
    if !app.filters.platform.is_empty() {
        filters.push(format!("platform={}", app.filters.platform));
    }
    let filter_summary = if filters.is_empty() {
        String::new()
    } else {
        format!(" [{}]", filters.join(" "))
    };

    let title_left = format!("[P]ackages search{}", filter_summary);
    let title_right = format!(
        "S:{} B:{} I:{} V:{}",
        app.search_mode_label(),
        if app.filters.show_broken { "on" } else { "off" },
        if app.filters.show_insecure {
            "on"
        } else {
            "off"
        },
        if app.filters.show_installed_only {
            "inst"
        } else {
            "all"
        }
    );
    let title = header_line_with_right(&title_left, &title_right, area.width);
    let border_style = focus_border_style(app, Focus::Packages);
    let search = Paragraph::new(app.query.as_str()).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    frame.render_widget(search, area);
}

fn body_constraints(app: &App) -> [Constraint; 3] {
    let collapsed_width = Constraint::Length(10);
    match (app.presets_collapsed, app.changes_collapsed) {
        (true, true) => [Constraint::Min(0), collapsed_width, collapsed_width],
        (true, false) => [
            Constraint::Percentage(70),
            collapsed_width,
            Constraint::Percentage(30),
        ],
        (false, true) => [
            Constraint::Percentage(70),
            Constraint::Percentage(30),
            collapsed_width,
        ],
        (false, false) => [
            Constraint::Percentage(60),
            Constraint::Percentage(25),
            Constraint::Percentage(15),
        ],
    }
}

fn render_presets_column(frame: &mut Frame, app: &mut App, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(7),
        ])
        .split(area);
    render_preset_search(frame, app, layout[0]);
    render_preset_list(frame, app, layout[1]);
    render_preset_details(frame, app, layout[2]);
}

fn render_changes_column(frame: &mut Frame, app: &App, area: Rect) {
    if app.changes_collapsed {
        render_changes_collapsed(frame, app, area);
    } else {
        render_changes_panel(frame, app, area);
    }
}

fn render_package_table(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = focus_border_style(app, Focus::Packages);

    let displayed = app.packages.len();
    let limit_label = match app.index_info.displayed_count {
        Some(total) if total > displayed => format!("{}+", displayed),
        _ => displayed.to_string(),
    };

    let rows: Vec<Row> = app
        .packages
        .iter()
        .map(|pkg| package_row(app, pkg))
        .collect();

    let package_min = if app.columns.show_description { 24 } else { 40 };
    let mut headers: Vec<Cell> = Vec::new();
    let mut constraints: Vec<Constraint> = Vec::new();
    headers.push(Cell::from("State"));
    headers.push(Cell::from("Package"));
    constraints.push(Constraint::Length(4));
    constraints.push(Constraint::Min(package_min));

    if app.columns.show_version {
        headers.push(Cell::from("Version"));
        constraints.push(Constraint::Length(10));
    }
    if app.columns.show_description {
        headers.push(Cell::from("Description"));
        constraints.push(Constraint::Min(30));
    }
    if app.columns.show_license {
        headers.push(Cell::from("License"));
        constraints.push(Constraint::Length(14));
    }
    if app.columns.show_platforms {
        headers.push(Cell::from("Platforms"));
        constraints.push(Constraint::Length(18));
    }
    if app.columns.show_main_program {
        headers.push(Cell::from("Main"));
        constraints.push(Constraint::Length(14));
    }

    let header = Row::new(headers).style(Style::default().add_modifier(Modifier::BOLD));

    let table = Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .title(format!("[P]ackages ({})", limit_label))
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(table, area, &mut app.packages_state);
}

fn render_package_details(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if let Some(pkg) = app.current_package() {
        let title = pkg.name.clone();
        let version = pkg.version.clone().unwrap_or_else(|| "unknown".to_string());
        lines.push(Line::from(Span::styled(
            format!("{} ({})", title, version),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if let Some(description) = pkg.description.as_ref() {
            if !description.trim().is_empty() {
                lines.push(Line::from(description.clone()));
            }
        }
        lines.push(Line::from(format!("attr: {}", pkg.attr_path)));
        lines.push(Line::from(format!(
            "main: {}",
            pkg.main_program.as_deref().unwrap_or("-")
        )));
        lines.push(Line::from(format!(
            "license: {}",
            pkg.license.as_deref().unwrap_or("-")
        )));
        lines.push(Line::from(format!(
            "platforms: {}",
            pkg.platforms.as_deref().unwrap_or("-")
        )));
    } else {
        lines.push(Line::from("No package selected"));
    }

    let details = Paragraph::new(Text::from(lines))
        .block(Block::default().title("Details").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(details, area);
}

fn render_preset_search(frame: &mut Frame, app: &App, area: Rect) {
    let title = "[T]emplates search";
    let border_style = focus_border_style(app, Focus::Presets);
    let search = Paragraph::new(app.preset_query.as_str()).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    frame.render_widget(search, area);
}

fn render_preset_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let border_style = focus_border_style(app, Focus::Presets);
    let items: Vec<ListItem> = app
        .preset_filtered
        .iter()
        .filter_map(|idx| app.presets.get(*idx))
        .map(|preset| preset_item(app, preset))
        .collect();

    let mut state = app.presets_state.clone();
    let list = List::new(items)
        .block(
            Block::default()
                .title(format!("[T]emplates ({})", app.active_presets.len()))
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut state);
    app.presets_state = state;
}

fn render_preset_details(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if let Some(preset) = app.current_preset() {
        lines.push(Line::from(Span::styled(
            preset.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if !preset.description.trim().is_empty() {
            lines.push(Line::from(preset.description.clone()));
        }
        if !preset.packages_required.is_empty() {
            lines.push(Line::from("Required packages:"));
            let max = 4usize;
            for pkg in preset.packages_required.iter().take(max) {
                lines.push(Line::from(format!("- {}", pkg)));
            }
            if preset.packages_required.len() > max {
                let remaining = preset.packages_required.len() - max;
                lines.push(Line::from(format!("... +{} more", remaining)));
            }
        }
        if !preset.packages_optional.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from("Optional packages:"));
            let max = 4usize;
            for pkg in preset.packages_optional.iter().take(max) {
                lines.push(Line::from(format!("- {}", pkg)));
            }
            if preset.packages_optional.len() > max {
                let remaining = preset.packages_optional.len() - max;
                lines.push(Line::from(format!("... +{} more", remaining)));
            }
        }
    } else {
        lines.push(Line::from("No template selected"));
    }

    let details = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Template details")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(details, area);
}

fn render_changes_panel(frame: &mut Frame, app: &App, area: Rect) {
    let lines = build_changes_lines(app, 3);
    let border_style = focus_border_style(app, Focus::Changes);
    let changes = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("[C]hanges")
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(changes, area);
}

fn render_changes_collapsed(frame: &mut Frame, app: &App, area: Rect) {
    let title = "[C]hanges";
    let border_style = focus_border_style(app, Focus::Changes);
    let content = Paragraph::new(Text::from(Line::from("-")))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    frame.render_widget(content, area);
}

fn render_templates_banner(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title("[T]emplates")
        .borders(Borders::TOP)
        .border_style(focus_border_style(app, Focus::Presets));
    frame.render_widget(block, area);
}

fn render_overlay(frame: &mut Frame, app: &App, overlay: &Overlay) {
    match overlay {
        Overlay::Help => render_help_overlay(frame),
        Overlay::PackageInfo(state) => render_package_info_overlay(frame, state),
        Overlay::VersionPicker(state) => render_version_picker_overlay(frame, state),
        Overlay::PinEditor(state) => render_pin_editor_overlay(frame, state),
        Overlay::Columns(state) => render_columns_overlay(frame, app, state),
        Overlay::Filter(state) => render_filter_overlay(frame, state),
        Overlay::Env(state) => render_env_overlay(frame, state),
        Overlay::Shell(state) => render_shell_overlay(frame, state),
        Overlay::Diff(state) => render_diff_overlay(frame, app, state),
    }
}

fn render_help_overlay(frame: &mut Frame) {
    let area = centered_rect(70, 70, frame.area());
    frame.render_widget(Clear, area);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let note = Paragraph::new(Text::from(Line::from(
        "mica is a TUI for managing Nix dev environments. Browse packages, apply templates, edit env/shell, and sync default.nix.",
    )))
    .block(Block::default().title("Help").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(note, layout[0]);

    let header_style = Style::default().add_modifier(Modifier::BOLD);
    let key_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let rows = vec![
        Row::new(vec!["Navigation", ""]).style(header_style),
        Row::new(vec![
            Span::styled("Tab", key_style),
            Span::raw("switch focus"),
        ]),
        Row::new(vec![
            Span::styled("Arrows", key_style),
            Span::raw("move selection"),
        ]),
        Row::new(vec![
            Span::styled("Enter/Space", key_style),
            Span::raw("toggle"),
        ]),
        Row::new(vec![
            Span::styled("Type", key_style),
            Span::raw("search (focused panel)"),
        ]),
        Row::new(vec![
            Span::styled("Query", key_style),
            Span::raw("shortcuts: 'exact, bin:, name:, desc:, all:"),
        ]),
        Row::new(vec![
            Span::styled("Example", key_style),
            Span::raw("'bin:rg = exact main program, name:ripgrep = name-only"),
        ]),
        Row::new(vec![
            Span::styled("Ctrl+U", key_style),
            Span::raw("clear search"),
        ]),
        Row::new(vec![Span::styled("S", key_style), Span::raw("search mode")]),
        Row::new(vec![
            Span::styled("Esc/?", key_style),
            Span::raw("close overlay"),
        ]),
        Row::new(vec!["", ""]),
        Row::new(vec!["Actions", ""]).style(header_style),
        Row::new(vec![Span::styled("Ctrl+S", key_style), Span::raw("save")]),
        Row::new(vec![Span::styled("Ctrl+Q", key_style), Span::raw("quit")]),
        Row::new(vec![
            Span::styled("Ctrl+P", key_style),
            Span::raw("package info"),
        ]),
        Row::new(vec![
            Span::styled("Ctrl+V", key_style),
            Span::raw("version picker"),
        ]),
        Row::new(vec![
            Span::styled("Ctrl+N", key_style),
            Span::raw("add pin"),
        ]),
        Row::new(vec![
            Span::styled("D", key_style),
            Span::raw("diff preview"),
        ]),
        Row::new(vec![
            Span::styled("T", key_style),
            Span::raw("toggle diff view (diff)"),
        ]),
        Row::new(vec![Span::styled("U", key_style), Span::raw("update pin")]),
        Row::new(vec![Span::styled("M", key_style), Span::raw("columns")]),
        Row::new(vec![
            Span::styled("R", key_style),
            Span::raw("rebuild index"),
        ]),
        Row::new(vec![
            Span::styled("Y", key_style),
            Span::raw("reload from nix"),
        ]),
        Row::new(vec!["", ""]),
        Row::new(vec!["Filters", ""]).style(header_style),
        Row::new(vec![
            Span::styled("B", key_style),
            Span::raw("broken filter"),
        ]),
        Row::new(vec![
            Span::styled("I", key_style),
            Span::raw("insecure filter"),
        ]),
        Row::new(vec![
            Span::styled("V", key_style),
            Span::raw("installed only"),
        ]),
        Row::new(vec![
            Span::styled("L", key_style),
            Span::raw("license filter"),
        ]),
        Row::new(vec![
            Span::styled("O", key_style),
            Span::raw("platform filter"),
        ]),
        Row::new(vec!["", ""]),
        Row::new(vec!["Panels", ""]).style(header_style),
        Row::new(vec![
            Span::styled("T", key_style),
            Span::raw("toggle templates"),
        ]),
        Row::new(vec![
            Span::styled("C", key_style),
            Span::raw("toggle changes"),
        ]),
        Row::new(vec![
            Span::styled("K", key_style),
            Span::raw("toggle details"),
        ]),
        Row::new(vec![Span::styled("E", key_style), Span::raw("edit env")]),
        Row::new(vec![
            Span::styled("Tab", key_style),
            Span::raw("in env edit: toggle string/expr mode"),
        ]),
        Row::new(vec![
            Span::styled("H", key_style),
            Span::raw("edit shell hook"),
        ]),
    ];

    let table = Table::new(rows, [Constraint::Length(16), Constraint::Min(0)])
        .block(Block::default().borders(Borders::ALL))
        .column_spacing(2);
    frame.render_widget(table, layout[1]);
}

fn render_filter_overlay(frame: &mut Frame, state: &crate::tui::app::FilterEditorState) {
    let area = centered_rect(60, 20, frame.area());
    frame.render_widget(Clear, area);

    let title = match state.kind {
        FilterKind::License => "Filter: License",
        FilterKind::Platform => "Filter: Platform",
    };

    let input_line = render_input_with_cursor(&state.input, state.cursor);
    let mut lines = vec![Line::from("Type to filter, Enter to apply, Esc to cancel")];
    lines.push(Line::from(""));
    lines.push(input_line);

    let filter = Paragraph::new(Text::from(lines))
        .block(Block::default().title(title).borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(filter, area);
}

fn render_columns_overlay(
    frame: &mut Frame,
    app: &App,
    state: &crate::tui::app::ColumnsEditorState,
) {
    let area = centered_rect(50, 50, frame.area());
    frame.render_widget(Clear, area);

    let items: Vec<ListItem> = crate::tui::app::COLUMN_OPTIONS
        .iter()
        .map(|option| {
            let enabled = match option.kind {
                crate::tui::app::ColumnKind::Version => app.columns.show_version,
                crate::tui::app::ColumnKind::Description => app.columns.show_description,
                crate::tui::app::ColumnKind::License => app.columns.show_license,
                crate::tui::app::ColumnKind::Platforms => app.columns.show_platforms,
                crate::tui::app::ColumnKind::MainProgram => app.columns.show_main_program,
            };
            let marker = if enabled { "[x]" } else { "[ ]" };
            ListItem::new(Line::from(format!("{} {}", marker, option.label)))
        })
        .collect();

    let mut list_state = ListState::default();
    if !items.is_empty() {
        list_state.select(Some(state.cursor));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .title("Columns (Enter/Space toggle, Esc close)")
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut list_state);
}

fn render_package_info_overlay(frame: &mut Frame, state: &crate::tui::app::PackageInfoState) {
    let area = centered_rect(80, 80, frame.area());
    frame.render_widget(Clear, area);

    let lines: Vec<Line> = state
        .lines
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();
    let paragraph = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .title("Package info (Esc to close, Up/Down to scroll)")
                .borders(Borders::ALL),
        )
        .scroll((state.scroll as u16, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(paragraph, area);
}

fn render_version_picker_overlay(frame: &mut Frame, state: &crate::tui::app::VersionPickerState) {
    let area = centered_rect(80, 80, frame.area());
    frame.render_widget(Clear, area);

    let mut list_state = TableState::default();
    if !state.entries.is_empty() {
        list_state.select(Some(state.cursor));
    }

    let rows: Vec<Row> = state
        .entries
        .iter()
        .map(|entry| {
            let short_commit = entry.commit.chars().take(8).collect::<String>();
            Row::new(vec![
                Cell::from(entry.source.clone()),
                Cell::from(entry.version.clone()),
                Cell::from(entry.commit_date.clone()),
                Cell::from(short_commit),
            ])
        })
        .collect();

    let header = Row::new(vec!["Source", "Version", "Date", "Commit"])
        .style(Style::default().add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(35),
            Constraint::Length(12),
            Constraint::Length(20),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(format!(
                "Versions for {} (Enter to pin, Esc to close)",
                state.package
            ))
            .borders(Borders::ALL),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(table, area, &mut list_state);
}

fn render_pin_editor_overlay(frame: &mut Frame, state: &crate::tui::app::PinEditorState) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);

    let mut lines = Vec::new();
    lines.push(Line::from(
        "Add pin, Tab/Shift+Tab or Up/Down to move, Ctrl+L toggle latest, Enter add, Esc cancel",
    ));
    lines.push(Line::from(""));

    lines.push(render_pin_field_line(
        "Name",
        PinField::Name,
        state,
        "Required",
    ));
    lines.push(render_pin_field_line(
        "URL",
        PinField::Url,
        state,
        "Required",
    ));
    lines.push(render_pin_field_line(
        "Branch",
        PinField::Branch,
        state,
        "Default main",
    ));
    lines.push(render_pin_field_line(
        "Revision",
        PinField::Rev,
        state,
        if state.use_latest {
            "Ignored when latest on"
        } else {
            "Required if latest off"
        },
    ));
    lines.push(render_pin_field_line(
        "Sha256",
        PinField::Sha256,
        state,
        "Optional, auto when rev set",
    ));
    lines.push(render_pin_field_line(
        "Tarball name",
        PinField::TarballName,
        state,
        "Optional",
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Latest:", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(if state.use_latest { " on" } else { " off" }),
    ]));

    if let Some(error) = &state.error {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    let editor = Paragraph::new(Text::from(lines))
        .block(Block::default().title("Add pin").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(editor, area);
}

fn render_env_overlay(frame: &mut Frame, state: &crate::tui::app::EnvEditorState) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);

    let input_height = if state.error.is_some() { 4 } else { 3 };
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(input_height)])
        .split(area);

    let items: Vec<ListItem> = state
        .entries
        .iter()
        .map(|entry| {
            let value = env_value_for_display(&entry.value);
            let mode_suffix = if env_value_is_nix_expression(&entry.value) {
                " [expr]"
            } else {
                ""
            };
            ListItem::new(Line::from(format!(
                "{}={}{}",
                entry.key, value, mode_suffix
            )))
        })
        .collect();

    let mut list_state = ListState::default();
    if !state.entries.is_empty() {
        list_state.select(Some(state.cursor));
    }

    let list = List::new(items)
        .block(Block::default().title("Environment").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, layout[0], &mut list_state);

    let (input_title, input_line) = match &state.mode {
        EnvEditMode::List => (
            "a add | Enter edit | d delete | Esc close".to_string(),
            Line::from(""),
        ),
        EnvEditMode::Edit { value_mode, .. } => {
            let mode = match value_mode {
                EnvValueMode::String => "string",
                EnvValueMode::NixExpression => "nix expr",
            };
            (
                format!(
                    "Editing (KEY=VALUE), mode: {} (Tab toggles), Enter save, Esc cancel",
                    mode
                ),
                render_input_with_cursor(&state.input, state.input_cursor),
            )
        }
    };

    let mut input_lines = Vec::new();
    if let Some(error) = &state.error {
        input_lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }
    input_lines.push(input_line);

    let input = Paragraph::new(Text::from(input_lines))
        .block(Block::default().title(input_title).borders(Borders::ALL));
    frame.render_widget(input, layout[1]);
}

fn env_value_is_nix_expression(value: &str) -> bool {
    value.starts_with(NIX_EXPR_PREFIX)
}

fn env_value_for_display(value: &str) -> String {
    value
        .strip_prefix(NIX_EXPR_PREFIX)
        .unwrap_or(value)
        .to_string()
}

fn render_shell_overlay(frame: &mut Frame, state: &crate::tui::app::ShellEditorState) {
    let area = centered_rect(80, 70, frame.area());
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    for (row, line) in state.lines.iter().enumerate() {
        if row == state.cursor_row {
            lines.push(render_line_with_cursor(line, state.cursor_col));
        } else {
            lines.push(Line::from(line.clone()));
        }
    }

    if lines.is_empty() {
        lines.push(render_line_with_cursor("", 0));
    }

    let text = Text::from(lines);
    let shell = Paragraph::new(text)
        .block(
            Block::default()
                .title("Shell hook (Esc to close, Ctrl+C cancel)")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(shell, area);
}

fn render_diff_overlay(frame: &mut Frame, _app: &App, state: &crate::tui::app::DiffViewerState) {
    let area = centered_rect(90, 80, frame.area());
    frame.render_widget(Clear, area);

    let current_lines = if state.show_full {
        &state.full_lines
    } else {
        &state.change_lines
    };

    let mut lines = Vec::new();
    for line in current_lines {
        let style = if line.starts_with('+') {
            Style::default().fg(Color::Green)
        } else if line.starts_with('-') {
            Style::default().fg(Color::Red)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(line.clone(), style)));
    }

    let title = if state.show_full {
        "Diff (full, T to toggle, Esc to close)"
    } else {
        "Diff (changes only, T to toggle, Esc to close)"
    };

    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().title(title).borders(Borders::ALL))
        .scroll((state.scroll as u16, 0))
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

fn render_toast(frame: &mut Frame, toast: &Toast) {
    let area = frame.area();
    if area.width < 10 || area.height < 3 {
        return;
    }

    let message = toast.message.clone();
    let max_width = area.width.saturating_sub(2) as usize;
    let width = (message.chars().count() + 4).min(max_width).max(10) as u16;
    let height = 3u16;
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width),
        area.y + area.height.saturating_sub(height),
        width,
        height,
    );

    let (border_style, text_style) = match toast.level {
        ToastLevel::Info => (
            Style::default().fg(Color::Cyan),
            Style::default().fg(Color::White),
        ),
        ToastLevel::Error => (
            Style::default().fg(Color::Red),
            Style::default().fg(Color::Red),
        ),
    };

    let paragraph = Paragraph::new(message)
        .block(
            Block::default()
                .title("status")
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .style(text_style)
        .wrap(Wrap { trim: true });

    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

fn focus_border_style(app: &App, focus: Focus) -> Style {
    if app.focus == focus {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    }
}

fn package_row(app: &App, pkg: &PackageEntry) -> Row<'static> {
    let base_attr = app.base_attr_for(&pkg.attr_path);
    let is_removed = app.removed.contains(&base_attr);
    let is_added = app.added.contains(&base_attr);
    let is_preset = app.preset_packages.contains(&base_attr);
    let is_pinned = app.pinned.contains_key(&base_attr);

    let marker = if is_removed {
        "[-]"
    } else if is_pinned {
        "[p]"
    } else if is_added {
        "[+]"
    } else if is_preset {
        "[x]"
    } else {
        "[ ]"
    };

    let alert = if pkg.broken {
        "!"
    } else if pkg.insecure {
        "~"
    } else {
        " "
    };

    let marker_style = if is_removed {
        Style::default().fg(Color::Red)
    } else if is_pinned {
        Style::default().fg(Color::Magenta)
    } else if is_added {
        Style::default().fg(Color::Green)
    } else if is_preset {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let mut row_style = Style::default();
    if pkg.broken {
        row_style = row_style.fg(Color::Red);
    } else if pkg.insecure {
        row_style = row_style.fg(Color::Yellow);
    }

    let version = pkg.version.as_deref().unwrap_or("-");
    let description = pkg.description.as_deref().unwrap_or("");
    let license = pkg.license.as_deref().unwrap_or("-");
    let platforms = pkg.platforms.as_deref().unwrap_or("-");
    let main_program = pkg.main_program.as_deref().unwrap_or("-");

    let mut cells = Vec::new();
    cells.push(Cell::from(Span::styled(
        format!("{}{}", marker, alert),
        marker_style,
    )));
    cells.push(Cell::from(pkg.name.clone()));

    if app.columns.show_version {
        cells.push(Cell::from(truncate_text(version, 12)));
    }
    if app.columns.show_description {
        cells.push(Cell::from(truncate_text(description, 80)));
    }
    if app.columns.show_license {
        cells.push(Cell::from(truncate_text(license, 20)));
    }
    if app.columns.show_platforms {
        cells.push(Cell::from(truncate_text(platforms, 24)));
    }
    if app.columns.show_main_program {
        cells.push(Cell::from(truncate_text(main_program, 20)));
    }

    Row::new(cells).style(row_style)
}

fn preset_item(app: &App, preset: &PresetEntry) -> ListItem<'static> {
    let active = app.active_presets.contains(&preset.name);
    let marker = if active { "[x]" } else { "[ ]" };
    let mut spans = vec![Span::raw(format!("{} ", marker))];
    if active {
        spans.push(Span::styled(
            preset.name.clone(),
            Style::default().fg(Color::Green),
        ));
    } else {
        spans.push(Span::raw(preset.name.clone()));
    }
    if !preset.description.trim().is_empty() {
        spans.push(Span::styled(
            format!(" - {}", truncate_text(&preset.description, 32)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn build_changes_lines(app: &App, max_items: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    let added: Vec<_> = app.added.difference(&app.base_added).cloned().collect();
    let removed: Vec<_> = app.removed.difference(&app.base_removed).cloned().collect();
    let presets_on: Vec<_> = app
        .active_presets
        .difference(&app.base_presets)
        .cloned()
        .collect();
    let presets_off: Vec<_> = app
        .base_presets
        .difference(&app.active_presets)
        .cloned()
        .collect();

    let mut pinned_added = Vec::new();
    let mut pinned_removed = Vec::new();
    let mut pinned_changed = Vec::new();
    for (name, pinned) in &app.pinned {
        match app.base_pinned.get(name) {
            None => pinned_added.push(format!("{} ({})", name, pinned.version)),
            Some(existing) if existing != pinned => {
                pinned_changed.push(format!("{} ({})", name, pinned.version))
            }
            _ => {}
        }
    }
    for name in app.base_pinned.keys() {
        if !app.pinned.contains_key(name) {
            pinned_removed.push(name.clone());
        }
    }

    let mut env_added = Vec::new();
    let mut env_removed = Vec::new();
    let mut env_changed = Vec::new();
    for (key, value) in &app.env {
        let display = env_value_for_display(value);
        let suffix = if env_value_is_nix_expression(value) {
            " [expr]"
        } else {
            ""
        };
        match app.base_env.get(key) {
            None => env_added.push(format!("{}={}{}", key, display, suffix)),
            Some(existing) if existing != value => {
                env_changed.push(format!("{}={}{}", key, display, suffix))
            }
            _ => {}
        }
    }
    for key in app.base_env.keys() {
        if !app.env.contains_key(key) {
            env_removed.push(key.clone());
        }
    }

    lines.push(Line::from(Span::styled(
        "Packages",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    push_change_lines(&mut lines, "+", &added, max_items, Color::Green);
    push_change_lines(&mut lines, "-", &removed, max_items, Color::Red);

    lines.push(Line::from(Span::styled(
        "Templates",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    push_change_lines(&mut lines, "+", &presets_on, max_items, Color::Green);
    push_change_lines(&mut lines, "-", &presets_off, max_items, Color::Red);

    lines.push(Line::from(Span::styled(
        "Pinned",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    push_change_lines(&mut lines, "+", &pinned_added, max_items, Color::Green);
    push_change_lines(&mut lines, "-", &pinned_removed, max_items, Color::Red);
    push_change_lines(&mut lines, "~", &pinned_changed, max_items, Color::Yellow);

    lines.push(Line::from(Span::styled(
        "Env",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    push_change_lines(&mut lines, "+", &env_added, max_items, Color::Green);
    push_change_lines(&mut lines, "-", &env_removed, max_items, Color::Red);
    push_change_lines(&mut lines, "~", &env_changed, max_items, Color::Yellow);

    let shell_changed = app.shell_hook != app.base_shell_hook;
    lines.push(Line::from(Span::styled(
        "Shell hook",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(if shell_changed {
        Span::styled("modified", Style::default().fg(Color::Yellow))
    } else {
        Span::raw("unchanged")
    }));

    lines
}

fn push_change_lines(
    lines: &mut Vec<Line>,
    prefix: &str,
    items: &[String],
    max_items: usize,
    color: Color,
) {
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("{} none", prefix),
            Style::default().fg(Color::DarkGray),
        )));
        return;
    }

    for item in items.iter().take(max_items) {
        lines.push(Line::from(vec![
            Span::styled(format!("{} ", prefix), Style::default().fg(color)),
            Span::raw(item.clone()),
        ]));
    }

    if items.len() > max_items {
        let remaining = items.len() - max_items;
        lines.push(Line::from(Span::styled(
            format!("... +{} more", remaining),
            Style::default().fg(Color::DarkGray),
        )));
    }
}

fn render_input_with_cursor(input: &str, cursor: usize) -> Line<'static> {
    let cursor = cursor.min(input.len());
    let (left, right) = input.split_at(cursor);
    let mut spans = vec![Span::raw(left.to_string())];
    let mut chars = right.chars();
    if let Some(next) = chars.next() {
        spans.push(Span::styled(
            next.to_string(),
            Style::default().add_modifier(Modifier::REVERSED),
        ));
        let rest: String = chars.collect();
        if !rest.is_empty() {
            spans.push(Span::raw(rest));
        }
    } else {
        spans.push(Span::styled(
            " ",
            Style::default().add_modifier(Modifier::REVERSED),
        ));
    }
    Line::from(spans)
}

fn render_line_with_cursor(input: &str, cursor: usize) -> Line<'static> {
    render_input_with_cursor(input, cursor)
}

fn header_line_with_right(left: &str, right: &str, width: u16) -> Line<'static> {
    let width = width.saturating_sub(2) as usize;
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if width == 0 || left_len + right_len + 1 > width {
        return Line::from(left.to_string());
    }
    let padding = width - left_len - right_len;
    let mut buffer = String::with_capacity(width);
    buffer.push_str(left);
    buffer.push_str(&" ".repeat(padding));
    buffer.push_str(right);
    Line::from(buffer)
}

fn header_line_with_right_span(left: &str, right: Span<'static>, width: u16) -> Line<'static> {
    let width = width.saturating_sub(2) as usize;
    let left_len = left.chars().count();
    let right_len = right.content.chars().count();
    if width == 0 || left_len + right_len + 1 > width {
        return Line::from(left.to_string());
    }
    let padding = width - left_len - right_len;
    Line::from(vec![
        Span::raw(left.to_string()),
        Span::raw(" ".repeat(padding)),
        right,
    ])
}

fn render_pin_field_line(
    label: &str,
    field: PinField,
    state: &crate::tui::app::PinEditorState,
    hint: &str,
) -> Line<'static> {
    let (value, cursor) = match field {
        PinField::Name => (&state.name, state.name_cursor),
        PinField::Url => (&state.url, state.url_cursor),
        PinField::Branch => (&state.branch, state.branch_cursor),
        PinField::Rev => (&state.rev, state.rev_cursor),
        PinField::Sha256 => (&state.sha256, state.sha256_cursor),
        PinField::TarballName => (&state.tarball_name, state.tarball_name_cursor),
    };

    let active = state.active == field;
    let input_line = if active {
        render_input_with_cursor(value, cursor)
    } else if value.is_empty() {
        Line::from(Span::styled(
            "<empty>",
            Style::default().fg(Color::DarkGray),
        ))
    } else {
        Line::from(value.to_string())
    };

    let mut spans = Vec::new();
    let label_style = if active {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    spans.push(Span::styled(if active { "> " } else { "  " }, label_style));
    spans.push(Span::styled(format!("{}: ", label), label_style));
    spans.extend(input_line.spans);
    if !hint.is_empty() {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("({})", hint),
            Style::default().fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1]);

    horizontal[1]
}

fn short_rev(value: &str) -> String {
    value.chars().take(8).collect()
}

fn index_display_name(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return "jpetrucciani/nix".to_string();
    }
    if let Some(pos) = trimmed.find("github.com/") {
        let mut tail = &trimmed[pos + "github.com/".len()..];
        if let Some(idx) = tail.find('#') {
            tail = &tail[..idx];
        }
        if let Some(idx) = tail.find('?') {
            tail = &tail[..idx];
        }
        for marker in ["/archive/", "/tarball/", "/commit/", "/tree/", "/releases/"] {
            if let Some(idx) = tail.find(marker) {
                tail = &tail[..idx];
                break;
            }
        }
        let tail = tail.trim_end_matches(".git");
        let parts: Vec<&str> = tail.split('/').filter(|part| !part.is_empty()).collect();
        if parts.len() >= 2 {
            return format!("{}/{}", parts[0], parts[1]);
        }
    }
    trimmed.to_string()
}

fn truncate_text(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let trimmed = text.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let mut out = trimmed
        .chars()
        .take(max.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}
