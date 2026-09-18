use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::app::{App, Pick, Picker, Tab};

// same palette as fiberglass
const ACCENT: Color = Color::Cyan;
const GOOD: Color = Color::Rgb(63, 185, 80);
const BAD: Color = Color::Rgb(248, 81, 73);
const DIM: Color = Color::Rgb(120, 130, 145);
const GOLD: Color = Color::Rgb(210, 168, 60);
const PANEL: Color = Color::Rgb(70, 78, 92);
const SELECT_BG: Color = Color::Rgb(30, 60, 90);

// -- layout --

pub fn render(f: &mut Frame, app: &App) {
    let [head, tabs, body, foot] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());
    header(f, app, head);
    tab_bar(f, app, tabs);
    match app.tab {
        Tab::Build if app.monitor.is_some() => {
            let [top, bottom] =
                Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(body);
            build(f, app, top);
            serial(f, app, bottom);
        }
        Tab::Build => build(f, app, body),
        Tab::Monitor => serial(f, app, body),
        Tab::Project => project(f, app, body),
    }
    footer(f, app, foot);
    if let Some(p) = &app.picker {
        picker(f, app, p);
    }
}

fn panel(title: impl Into<Line<'static>>) -> Block<'static> {
    Block::bordered().border_style(Style::new().fg(PANEL)).title(title)
}

fn header(f: &mut Frame, app: &App, area: Rect) {
    let dim = |t: &'static str| t.fg(DIM);
    let line = match app.target() {
        None => Line::from(vec![dim(" no boards yet — plug one in and add it from Project (3)")]),
        Some(t) => {
            let port = match (app.port_of(app.active), &t.port) {
                (Some(d), _) => Span::styled(format!("● {}", d.port.address), Style::new().fg(GOOD)),
                (None, Some(p)) => Span::styled(format!("○ {p} (unplugged)"), Style::new().fg(BAD)),
                (None, None) => Span::styled("○ not connected", Style::new().fg(DIM)),
            };
            let sketch = match &t.sketch {
                Some(s) => Span::raw(s.clone()),
                None => Span::styled("none — press s", Style::new().fg(DIM)),
            };
            Line::from(vec![
                " ".into(),
                Span::styled(t.name.clone(), Style::new().fg(GOLD).bold()),
                format!("  {}", t.fqbn).fg(DIM),
                "    ".into(),
                port,
                dim("    sketch "),
                sketch,
            ])
        }
    };
    let name = app.root.file_name().unwrap_or_default().to_string_lossy();
    let mut block = panel(Line::from(vec![" fluxus ".fg(ACCENT).bold(), format!("{name} ").fg(DIM)]))
        .title(Line::from(format!(" arduino-cli {} ", app.cli_version).fg(DIM)).right_aligned());
    if let Some(e) = &app.project_err {
        block = block.title_bottom(Line::from(format!(" {e} ").fg(BAD)));
    }
    f.render_widget(Paragraph::new(line).block(block), area);
}

fn tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let titles = Tab::ALL.iter().enumerate().map(|(i, t)| format!(" {} {} ", i + 1, t.title()));
    let sel = Tab::ALL.iter().position(|t| *t == app.tab);
    let tabs = Tabs::new(titles)
        .select(sel)
        .style(Style::new().fg(DIM))
        .highlight_style(Style::new().fg(Color::Black).bg(ACCENT).bold())
        .divider("");
    f.render_widget(tabs, area);
}

fn footer(f: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = if app.picker.is_some() {
        &[("type", "filter"), ("↑↓", "move"), ("enter", "choose"), ("esc", "cancel")]
    } else if app.input.is_some() {
        &[("type", "message"), ("enter", "send"), ("esc", "done")]
    } else if app.job.is_some() {
        &[("j/k", "scroll"), ("esc", "cancel"), ("tab", "switch"), ("q", "quit")]
    } else {
        match app.tab {
            Tab::Build => &[
                ("c", "compile"),
                ("u", "upload"),
                ("F", "flash all"),
                ("m", "monitor"),
                ("s", "sketch"),
                ("j/k", "scroll"),
                ("q", "quit"),
            ],
            Tab::Monitor => &[
                ("m", "open/close"),
                ("i", "send"),
                ("+/-", "baud"),
                ("x", "clear"),
                ("j/k", "scroll"),
                ("u", "upload"),
                ("q", "quit"),
            ],
            Tab::Project if app.on_port_row() => &[
                ("enter", "add to project"),
                ("j/k", "move"),
                ("a", "add by hand"),
                ("F", "flash all"),
                ("q", "quit"),
            ],
            Tab::Project => &[
                ("a", "add"),
                ("enter", "make active"),
                ("s", "sketch"),
                ("p", "port"),
                ("b", "board"),
                ("d", "remove"),
                ("F", "flash all"),
                ("q", "quit"),
            ],
        }
    };
    let spans: Vec<Span> = keys
        .iter()
        .flat_map(|(k, v)| [format!(" {k} ").fg(ACCENT).bold(), format!("{v}  ").fg(DIM)])
        .collect();
    f.render_widget(Line::from(spans), area);
}

// -- panes --

fn log_style(line: &str) -> Style {
    let lower = line.to_ascii_lowercase();
    let fg = if line.starts_with('✗') || line.starts_with("Error") || lower.contains("error:") {
        BAD
    } else if lower.contains("warning:") {
        GOLD
    } else if line.starts_with('✓') || line.starts_with("Sketch uses") || line.starts_with("Global variables") {
        GOOD
    } else if line.starts_with("$ ") {
        ACCENT
    } else {
        return Style::new();
    };
    Style::new().fg(fg)
}

fn scrollback(f: &mut Frame, area: Rect, block: Block, lines: Vec<Line>, scroll: usize) {
    let inner = block.inner(area);
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    let bottom = para.line_count(inner.width).saturating_sub(inner.height as usize);
    let top = bottom.saturating_sub(scroll);
    let block = if top < bottom {
        block.title_bottom(Line::from(" scrolled — G to follow ".fg(DIM)).right_aligned())
    } else {
        block
    };
    f.render_widget(para.block(block).scroll((top.min(u16::MAX as usize) as u16, 0)), area);
}

fn build(f: &mut Frame, app: &App, area: Rect) {
    let title = match &app.job {
        Some(j) => Line::from(format!(" build — {}… ", j.name).fg(GOLD)),
        None => Line::from(" build "),
    };
    let block = panel(title);
    if app.log.is_empty() {
        let hint = Span::styled("press c to compile, u to upload", Style::new().fg(DIM));
        f.render_widget(Paragraph::new(hint).block(block), area);
        return;
    }
    let lines = app.log.iter().map(|l| Line::styled(l.as_str(), log_style(l))).collect();
    scrollback(f, area, block, lines, app.log_scroll);
}

fn serial(f: &mut Frame, app: &App, area: Rect) {
    let title = match &app.monitor {
        Some(m) => Line::from(vec![" monitor ".into(), format!("{} @ {} ", m.port, app.baud).fg(GOOD)]),
        None => Line::from(vec![" monitor ".into(), format!("closed @ {} ", app.baud).fg(DIM)]),
    };
    let block = panel(title);
    let [out, input] = if app.input.is_some() {
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area)
    } else {
        [area, Rect::default()]
    };

    if app.serial.iter().all(|l| l.is_empty()) {
        let hint = Span::styled("press m to open the serial monitor", Style::new().fg(DIM));
        f.render_widget(Paragraph::new(hint).block(block), out);
    } else {
        let lines = app
            .serial
            .iter()
            .map(|l| if l.starts_with('─') { Line::styled(l.as_str(), Style::new().fg(DIM)) } else { Line::raw(l.as_str()) })
            .collect();
        scrollback(f, out, block, lines, app.serial_scroll);
    }

    if let Some(buf) = &app.input {
        let block = Block::bordered().border_style(Style::new().fg(ACCENT)).title(" send ");
        let line = Line::from(vec![buf.clone().into(), "▏".fg(ACCENT)]);
        f.render_widget(Paragraph::new(line).block(block), input);
    }
}

fn project(f: &mut Frame, app: &App, area: Rect) {
    let rows = (app.boards.len().max(1) + 2) as u16;
    let [top, bottom] = Layout::vertical([Constraint::Length(rows), Constraint::Min(0)]).areas(area);
    let links = app.links();

    let block = panel(format!(" boards ({}) ", app.boards.len()));
    if app.boards.is_empty() {
        let hint = Span::styled("pick a detected port below, or press a to add a board by hand", Style::new().fg(DIM));
        f.render_widget(Paragraph::new(hint).block(block), top);
    } else {
        let ports: Vec<(String, Color)> = (0..app.boards.len())
            .map(|i| match (links[i], &app.boards[i].port) {
                (Some(j), _) => (format!("● {}", app.ports[j].port.address), GOOD),
                (None, Some(p)) => (format!("○ {p}"), BAD),
                (None, None) => ("○ not connected".into(), DIM),
            })
            .collect();
        let width = |f: &dyn Fn(usize) -> usize| (0..app.boards.len()).map(f).max().unwrap_or(0) + 2;
        let (wn, wf) = (width(&|i| app.boards[i].name.chars().count()), width(&|i| app.boards[i].fqbn.len()));
        let wp = width(&|i| ports[i].0.chars().count());
        let items: Vec<ListItem> = app
            .boards
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let port = Span::styled(format!("{:<wp$}", ports[i].0), Style::new().fg(ports[i].1));
                let sketch = match &t.sketch {
                    Some(s) => Span::raw(s.clone()),
                    None => Span::styled("no sketch", Style::new().fg(DIM)),
                };
                ListItem::new(Line::from(vec![
                    (if i == app.active { " ▸ " } else { "   " }).fg(ACCENT),
                    Span::styled(format!("{:<wn$}", t.name), Style::new().fg(GOLD)),
                    Span::styled(format!("{:<wf$}", t.fqbn), Style::new().fg(DIM)),
                    port,
                    sketch,
                ]))
            })
            .collect();
        let sel = (app.sel < app.boards.len()).then_some(app.sel);
        let mut state = ListState::default().with_selected(sel);
        f.render_stateful_widget(
            List::new(items).block(block).highlight_style(Style::new().bg(SELECT_BG)),
            top,
            &mut state,
        );
    }

    let free = app.free_ports();
    let title = if app.scanned {
        Line::from(vec![format!(" new ports ({}) ", free.len()).into(), "enter adds to project ".fg(DIM)])
    } else {
        Line::from(" new ports — scanning… ")
    };
    let mut block = panel(title);
    if let Some(e) = &app.ports_err {
        block = block.title_bottom(Line::from(format!(" {e} ").fg(BAD)));
    }
    let items: Vec<ListItem> = free
        .iter()
        .map(|&j| {
            let d = &app.ports[j];
            let boards = match d.matching_boards.iter().find(|b| !b.fqbn.is_empty()) {
                Some(b) => Line::from(vec![Span::styled(b.name.clone(), Style::new().fg(GOOD)), format!("  {}", b.fqbn).fg(DIM)]),
                None => Line::from("unknown board — you pick the type".fg(DIM)),
            };
            let mut spans = vec![
                Span::raw(format!("   {:<34}", d.port.address)),
                Span::styled(format!("{:<20}", d.port.protocol_label), Style::new().fg(DIM)),
            ];
            spans.extend(boards.spans);
            ListItem::new(Line::from(spans))
        })
        .collect();
    let sel = app.sel.checked_sub(app.boards.len());
    let mut state = ListState::default().with_selected(sel);
    f.render_stateful_widget(
        List::new(items).block(block).highlight_style(Style::new().bg(SELECT_BG)),
        bottom,
        &mut state,
    );
}

// -- overlays --

fn picker(f: &mut Frame, app: &App, p: &Picker) {
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage(15),
        Constraint::Percentage(70),
        Constraint::Percentage(15),
    ])
    .areas(f.area());
    let [_, area, _] = Layout::vertical([
        Constraint::Percentage(15),
        Constraint::Percentage(70),
        Constraint::Percentage(15),
    ])
    .areas(mid);
    f.render_widget(Clear, area);

    let who = app.boards.get(p.target).map_or(String::new(), |t| format!(" for {}", t.name));
    let what = match p.kind {
        Pick::AddBoard => " add board".to_string(),
        Pick::Board => format!(" board{who}"),
        Pick::Sketch => format!(" sketch{who}"),
        Pick::Port => format!(" port{who}"),
    };
    let block = Block::bordered()
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(vec![what.fg(DIM), ": ".fg(DIM), p.query.clone().fg(Color::White), "▏".fg(ACCENT)]));

    let empty = match p.kind {
        Pick::AddBoard | Pick::Board => match &app.all_boards {
            None => Some(Span::styled("loading boards…", Style::new().fg(DIM))),
            Some(Err(e)) => Some(Span::styled(e.clone(), Style::new().fg(BAD))),
            Some(Ok(all)) if all.is_empty() => Some(Span::styled(
                "no cores installed — try: arduino-cli core install arduino:avr",
                Style::new().fg(DIM),
            )),
            Some(Ok(_)) => None,
        },
        Pick::Sketch if app.sketches.is_empty() => {
            Some(Span::styled(format!("no .ino files under {}", app.root.display()), Style::new().fg(DIM)))
        }
        Pick::Port if app.ports.is_empty() => Some(Span::styled("no ports detected", Style::new().fg(DIM))),
        _ => None,
    };
    if let Some(msg) = empty {
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }).block(block), area);
        return;
    }

    let items: Vec<ListItem> = app
        .picks()
        .into_iter()
        .map(|(label, detail)| ListItem::new(Line::from(vec![format!(" {label:<40}").into(), detail.fg(DIM)])))
        .collect();
    let mut state = ListState::default().with_selected(Some(p.sel));
    f.render_stateful_widget(
        List::new(items).block(block).highlight_style(Style::new().bg(SELECT_BG)),
        area,
        &mut state,
    );
}
