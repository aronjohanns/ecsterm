use crate::app::{App, Focus, Screen};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},

    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

pub fn draw(f: &mut Frame, app: &App) {
    match app.screen {
        Screen::Profiles => draw_profiles(f, app),
        Screen::Explorer => draw_explorer(f, app),
    }
}

fn draw_profiles(f: &mut Frame, app: &App) {
    let area = centered(60, 60, f.area());
    let items: Vec<ListItem> = if app.profiles.is_empty() {
        vec![ListItem::new("No profiles found in ~/.aws/config or ~/.aws/credentials")]
    } else {
        app.profiles
            .iter()
            .map(|p| {
                let mut spans = vec![Span::raw(p.name.clone())];
                if let Some(r) = &p.region {
                    spans.push(Span::styled(format!("  {r}"), Style::default().fg(Color::DarkGray)));
                }
                ListItem::new(Line::from(spans))
            })
            .collect()
    };
    let mut state = ListState::default().with_selected(Some(app.profile_selected));
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Select AWS profile  (Enter select, q quit) "))
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::White).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_widget(Clear, area);
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_explorer(f: &mut Frame, app: &App) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(f.area());
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(outer[0]);

    draw_tree(f, app, cols[0]);
    draw_commands(f, app, cols[1]);

    f.render_widget(Paragraph::new(app.status.as_str()).style(Style::default().fg(Color::DarkGray)), outer[1]);

    if let Some(input) = &app.command_input {
        let area = centered(90, 30, f.area());
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(format!("{input}▏"))
                .wrap(Wrap { trim: false })
                .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)).title(" Edit command  (Enter run, Esc cancel) ")),
            area,
        );
    }
    if let Some(input) = &app.region_input {
        let area = centered(50, 3, f.area());
        let area = Rect { height: 3, ..area };
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(format!("{input}▏")).block(Block::default().borders(Borders::ALL).title(" Region (Enter confirm, Esc cancel) ")),
            area,
        );
    }
}

fn draw_tree(f: &mut Frame, app: &App, area: Rect) {
    let title = match &app.ecs {
        Some(e) => format!(" Explorer  [{}] ", e.profile),
        None => " Explorer ".into(),
    };
    let items: Vec<ListItem> = app
        .rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            if let Some(msg) = &row.message {
                let style = if msg.starts_with("Error") {
                    Style::default().fg(Color::Red)
                } else if msg.starts_with('⟳') {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                return ListItem::new(Line::from(vec![Span::raw(format!("{indent}    ")), Span::styled(msg.clone(), style)]));
            }
            let node = app.tree.get(&row.path).expect("row path valid");
            let arrow = if node.is_leaf() {
                "  "
            } else if node.expanded {
                "▾ "
            } else {
                "▸ "
            };
            let mut spans = vec![
                Span::styled(format!("{indent}{arrow}"), Style::default().fg(Color::DarkGray)),
                Span::raw(node.icon()),
                Span::raw(node.label()),
            ];
            if let Some(d) = node.description() {
                let color = match node.exec_ready() {
                    Some(true) => Color::Green,
                    Some(false) => Color::Red,
                    None if d == "ACTIVE" => Color::Green,
                    None => Color::Yellow,
                };
                spans.push(Span::styled(format!("  {d}"), Style::default().fg(color)));
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let focused = app.focus == Focus::Tree;
    let mut state = ListState::default().with_selected(Some(app.selected));
    let list = List::new(items)
        .block(panel(title, focused))
        .highlight_style(Style::default().bg(if focused { Color::Blue } else { Color::DarkGray }).fg(Color::White));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_commands(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focus == Focus::Commands;
    let block = panel(" AWS CLI  (Enter run, e edit, y copy) ".into(), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.commands.is_empty() {
        f.render_widget(
            Paragraph::new("Select a node in the tree to see the matching AWS CLI commands.").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    if let Some((_, node)) = app.selected_node() {
        if let crate::tree::Kind::Service(s) = &node.kind {
            lines.push(Line::from(Span::styled(s.arn.clone(), Style::default().fg(Color::DarkGray))));
            lines.push(Line::from(Span::styled(format!("Task Definition: {}", s.task_definition), Style::default().fg(Color::DarkGray))));
            lines.push(Line::raw(""));
        }
        let service = match &node.kind {
            crate::tree::Kind::Container { service, .. } | crate::tree::Kind::Task { service, .. } => Some(service),
            _ => None,
        };
        if let Some(service) = service {
            let (txt, color) = if service.enable_execute_command {
                ("ECS Exec: enabled on service", Color::Green)
            } else {
                ("ECS Exec: disabled on service (enable it first)", Color::Yellow)
            };
            lines.push(Line::from(Span::styled(txt, Style::default().fg(color))));
            if let crate::tree::Kind::Task { task, .. } = &node.kind {
                let (txt, color) = match node.exec_ready() {
                    Some(true) => ("Task is RUNNING with ExecuteCommandAgent".to_string(), Color::Green),
                    _ if task.last_status != "RUNNING" && task.desired_status == "RUNNING" => {
                        ("Container instance starting, try again later.".to_string(), Color::Yellow)
                    }
                    _ => (format!("Task not exec-ready ({})", task.last_status), Color::Red),
                };
                lines.push(Line::from(Span::styled(txt, Style::default().fg(color))));
                lines.push(Line::from(Span::styled(task.arn.clone(), Style::default().fg(Color::DarkGray))));
            }
            lines.push(Line::raw(""));
        }
    }
    for (i, c) in app.commands.iter().enumerate() {
        let sel = focused && i == app.command_selected;
        let (marker, style) = if sel { ("▶ ", Style::default().fg(Color::Cyan).bold()) } else { ("  ", Style::default().bold()) };
        lines.push(Line::from(vec![
            Span::styled(marker, style),
            Span::styled(c.title.clone(), if sel { style.underlined() } else { style }),
        ]));
        lines.push(Line::from(Span::styled(
            format!("  {}", c.command),
            if sel { Style::default().fg(Color::Gray) } else { Style::default().fg(Color::DarkGray) },
        )));
        lines.push(Line::raw(""));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// A panel's frame. The focused panel is thick and bright and the other one
/// dimmed, so it is clear at a glance which panel the keys go to.
fn panel(title: String, focused: bool) -> Block<'static> {
    let (border_type, style) = if focused {
        (BorderType::Thick, Style::default().fg(Color::Cyan).bold())
    } else {
        (BorderType::Plain, Style::default().fg(Color::DarkGray))
    };
    Block::default().borders(Borders::ALL).border_type(border_type).border_style(style).title(title)
}

fn centered(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage((100 - pct_y) / 2), Constraint::Percentage(pct_y), Constraint::Percentage((100 - pct_y) / 2)])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage((100 - pct_x) / 2), Constraint::Percentage(pct_x), Constraint::Percentage((100 - pct_x) / 2)])
        .split(v[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    fn render(focus: Focus) -> Buffer {
        let mut app = App::new(Config::defaults(), None, None);
        app.screen = Screen::Explorer;
        app.focus = focus;
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn only_the_focused_panel_has_a_thick_bright_border() {
        // The tree panel takes 45% of the 40 columns, so the command panel starts at 18.
        let (tree, commands) = ((0, 0), (18, 0));

        let buf = render(Focus::Tree);
        assert_eq!((buf[tree].symbol(), buf[tree].fg), ("┏", Color::Cyan));
        assert_eq!((buf[commands].symbol(), buf[commands].fg), ("┌", Color::DarkGray));

        let buf = render(Focus::Commands);
        assert_eq!((buf[tree].symbol(), buf[tree].fg), ("┌", Color::DarkGray));
        assert_eq!((buf[commands].symbol(), buf[commands].fg), ("┏", Color::Cyan));
    }
}
