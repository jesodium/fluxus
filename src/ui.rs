use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::app::{App, Picker, Tab};

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
        Tab::Boards => ports(f, app, body),
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
    let port = match &app.port {
        Some(p) if app.port_connected() => Span::styled(p.address.clone(), Style::new().fg(GOOD)),
        Some(p) => Span::styled(format!("{} (unplugged)", p.address), Style::new().fg(BAD)),
        None => Span::styled("none", Style::new().fg(DIM)),
    };
    let board = match &app.board {
        Some(b) => Span::styled(format!("{}  {}", b.name, b.fqbn), Style::new().fg(GOLD)),
        None => Span::styled("none — press b", Style::new().fg(DIM)),
    };
    let sketch = match &app.sketch {
        Ok(p) => Span::raw(p.file_name().unwrap_or_default().to_string_lossy().into_owned()),
        Err(e) => Span::styled(e.clone(), Style::new().fg(BAD)),
    };
    let line = Line::from(vec![
        " sketch ".fg(DIM),
        sketch,
        "    port ".fg(DIM),
        port,
        "    board ".fg(DIM),
        board,
    ]);
    let block = panel(" fluxus ".fg(ACCENT).bold())
        .title(Line::from(format!(" arduino-cli {} ", app.cli_version).fg(DIM)).right_aligned());
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
                ("m", "monitor"),
                ("j/k", "scroll"),
                ("b", "board"),
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
            Tab::Boards => &[
                ("j/k", "move"),
                ("enter", "use port"),
                ("b", "board"),
                ("c", "compile"),
                ("u", "upload"),
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

fn ports(f: &mut Frame, app: &App, area: Rect) {
    let title = if app.scanned {
        format!(" ports ({}) ", app.ports.len())
    } else {
        " ports — scanning… ".into()
    };
    let block = panel(title);

    if app.ports.is_empty() {
        let msg = match &app.ports_err {
            Some(e) => Span::styled(e.clone(), Style::new().fg(BAD)),
            None if app.scanned => Span::styled("no ports found — plug in a board", Style::new().fg(DIM)),
            None => Span::raw(""),
        };
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }).block(block), area);
        return;
    }

    let items: Vec<ListItem> = app
        .ports
        .iter()
        .map(|d| {
            let chosen = app.port.as_ref().is_some_and(|p| p.address == d.port.address);
            let boards = if d.matching_boards.is_empty() {
                Span::styled("unknown board", Style::new().fg(DIM))
            } else {
                let names: Vec<_> = d.matching_boards.iter().map(|b| b.name.as_str()).collect();
                Span::styled(names.join(", "), Style::new().fg(GOOD))
            };
            ListItem::new(Line::from(vec![
                (if chosen { " ● " } else { "   " }).fg(ACCENT),
                Span::raw(format!("{:<34}", d.port.address)),
                Span::styled(format!("{:<20}", d.port.protocol_label), Style::new().fg(DIM)),
                boards,
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.sel));
    let block = match &app.ports_err {
        Some(e) => block.title_bottom(Line::from(format!(" {e} ").fg(BAD))),
        None => block,
    };
    f.render_stateful_widget(
        List::new(items).block(block).highlight_style(Style::new().bg(SELECT_BG)),
        area,
        &mut state,
    );
}

// -- overlays --

fn picker(f: &mut Frame, app: &App, p: &Picker) {
    let [_, mid, _] = Layout::horizontal([
        Constraint::Percentage(20),
        Constraint::Percentage(60),
        Constraint::Percentage(20),
    ])
    .areas(f.area());
    let [_, area, _] = Layout::vertical([
        Constraint::Percentage(15),
        Constraint::Percentage(70),
        Constraint::Percentage(15),
    ])
    .areas(mid);
    f.render_widget(Clear, area);

    let block = Block::bordered()
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(vec![" board: ".fg(DIM), p.query.clone().fg(Color::White), "▏".fg(ACCENT)]));

    let err = match &app.all_boards {
        None => Some(Span::styled("loading boards…", Style::new().fg(DIM))),
        Some(Err(e)) => Some(Span::styled(e.clone(), Style::new().fg(BAD))),
        Some(Ok(all)) if all.is_empty() => Some(Span::styled(
            "no cores installed — try: arduino-cli core install arduino:avr",
            Style::new().fg(DIM),
        )),
        Some(Ok(_)) => None,
    };
    if let Some(msg) = err {
        f.render_widget(Paragraph::new(msg).wrap(Wrap { trim: true }).block(block), area);
        return;
    }

    let items: Vec<ListItem> = app
        .filtered()
        .into_iter()
        .map(|b| ListItem::new(Line::from(vec![format!(" {:<36}", b.name).into(), b.fqbn.clone().fg(DIM)])))
        .collect();
    let mut state = ListState::default().with_selected(Some(p.sel));
    f.render_stateful_widget(
        List::new(items).block(block).highlight_style(Style::new().bg(SELECT_BG)),
        area,
        &mut state,
    );
}
