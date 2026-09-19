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
        Tab::Build if app.panes.iter().any(|p| p.link.is_some()) => {
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
        None => Line::from(format!(" {}", app.no_boards()).fg(DIM)),
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
                ("←→", "pane"),
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
    let [grid, input] = if app.input.is_some() {
        Layout::vertical([Constraint::Min(0), Constraint::Length(3)]).areas(area)
    } else {
        [area, Rect::default()]
    };

    let shown = app.shown_panes();
    if shown.is_empty() {
        let hint = Span::styled("add a board in Project (3), then press m", Style::new().fg(DIM));
        f.render_widget(Paragraph::new(hint).block(panel(" monitor ")), grid);
    }
    let cols = if shown.len() <= 3 { shown.len() } else { (shown.len() as f64).sqrt().ceil() as usize };
    let rows: Vec<&[usize]> = shown.chunks(cols.max(1)).collect();
    let row_areas = Layout::vertical(vec![Constraint::Fill(1); rows.len()]).split(grid);
    for (row, &row_area) in rows.iter().zip(row_areas.iter()) {
        let cells = Layout::horizontal(vec![Constraint::Fill(1); row.len()]).split(row_area);
        for (&i, &cell) in row.iter().zip(cells.iter()) {
            pane(f, app, i, cell, shown.len() > 1);
        }
    }

    if let Some(buf) = &app.input {
        let who = app.target().map_or(String::new(), |t| format!(" to {}", t.name));
        let block = Block::bordered().border_style(Style::new().fg(ACCENT)).title(format!(" send{who} "));
        let line = Line::from(vec![buf.clone().into(), "▏".fg(ACCENT)]);
        f.render_widget(Paragraph::new(line).block(block), input);
    }
}

fn pane(f: &mut Frame, app: &App, i: usize, area: Rect, many: bool) {
    let p = &app.panes[i];
    let name = app.boards.get(i).map_or("monitor", |t| t.name.as_str());
    let status = match &p.link {
        Some(l) => format!("{} @ {} ", l.port, p.baud).fg(GOOD),
        None => format!("closed @ {} ", p.baud).fg(DIM),
    };
    let mut block = panel(Line::from(vec![format!(" {name} ").into(), status]));
    if many && i == app.active {
        block = block.border_style(Style::new().fg(ACCENT));
    }
    if p.lines.iter().all(|l| l.is_empty()) {
        let hint = Span::styled("press m to open the serial monitor", Style::new().fg(DIM));
        f.render_widget(Paragraph::new(hint).block(block), area);
        return;
    }
    let lines = p
        .lines
        .iter()
        .map(|l| match l.chars().next() {
            Some('─') => Line::styled(l.as_str(), Style::new().fg(DIM)),
            Some('→') => Line::styled(l.as_str(), Style::new().fg(ACCENT)),
            _ => Line::raw(l.as_str()),
        })
        .collect();
    scrollback(f, area, block, lines, p.scroll);
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

    let picks = app.picks();
    let items: Vec<ListItem> = if p.kind == Pick::Sketch {
        let root = app.root.file_name().map_or("sketch".into(), |n| n.to_string_lossy().into_owned());
        let labels: Vec<&str> = picks.iter().map(|(l, _)| l.as_str()).collect();
        tree(&labels, &root)
            .into_iter()
            .zip(&picks)
            .enumerate()
            .map(|(i, ((dirs, depth, file), (_, detail)))| {
                let mut lines: Vec<Line> =
                    dirs.into_iter().map(|(d, name)| format!(" {}{name}/", "  ".repeat(d)).fg(DIM).into()).collect();
                let mut leaf = Line::from(vec![
                    " ".repeat(1 + 2 * depth).into(),
                    Span::styled(file, Style::new().fg(GOOD).bold()),
                    format!("  {detail}").fg(DIM),
                ]);
                if i == p.sel {
                    leaf = leaf.bg(SELECT_BG);
                }
                lines.push(leaf);
                ListItem::new(lines)
            })
            .collect()
    } else {
        picks
            .into_iter()
            .map(|(label, detail)| ListItem::new(Line::from(vec![format!(" {label:<40}").into(), detail.fg(DIM)])))
            .collect()
    };
    let highlight = if p.kind == Pick::Sketch { Style::new() } else { Style::new().bg(SELECT_BG) };
    let mut state = ListState::default().with_selected(Some(p.sel));
    f.render_stateful_widget(
        List::new(items).block(block).highlight_style(highlight),
        area,
        &mut state,
    );
}

// dirs not shared with the row above, then the .ino itself
type TreeRow = (Vec<(usize, String)>, usize, String);

fn tree(labels: &[&str], root: &str) -> Vec<TreeRow> {
    let mut prev: Vec<&str> = vec![];
    labels
        .iter()
        .map(|&l| {
            let mut parts: Vec<&str> = if l == "." { vec![] } else { l.split('/').collect() };
            let file = match parts.last() {
                Some(f) if f.ends_with(".ino") => parts.pop().unwrap().to_string(),
                Some(f) => format!("{f}.ino"),
                None => format!("{root}.ino"),
            };
            let shared = prev.iter().zip(&parts).take_while(|(a, b)| a == b).count();
            let dirs = parts.iter().enumerate().skip(shared).map(|(d, n)| (d, n.to_string())).collect();
            let depth = parts.len();
            prev = parts;
            (dirs, depth, file)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_shares_parent_dirs() {
        let rows = tree(&["giga-r1/i2c_scan", "giga-r1/main", "scratchpad/motor_test.ino", "."], "blackout");
        let d = |v: &[(usize, &str)]| v.iter().map(|&(i, s)| (i, s.to_string())).collect::<Vec<_>>();
        assert_eq!(rows[0], (d(&[(0, "giga-r1"), (1, "i2c_scan")]), 2, "i2c_scan.ino".into()));
        assert_eq!(rows[1], (d(&[(1, "main")]), 2, "main.ino".into()));
        assert_eq!(rows[2], (d(&[(0, "scratchpad")]), 1, "motor_test.ino".into()));
        assert_eq!(rows[3], (vec![], 0, "blackout.ino".into()));
    }

    #[tokio::test]
    async fn monitor_grid_fits_any_board_count() {
        use crate::app::AppEvent;
        use crate::cli::{Detected, Port};
        use crossterm::event::{KeyCode, KeyEvent};
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
        for n in 1..=5 {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            let mut app = App::new(String::new(), std::env::temp_dir().join("fluxus-none"), tx);
            let ports = (0..n).map(|j| Detected { port: Port { address: format!("/dev/p{j}"), ..Default::default() }, matching_boards: vec![] });
            app.event(AppEvent::Ports(Ok(ports.collect())));
            for j in 0..n {
                app.boards.push(crate::project::Target { name: format!("board{j}"), fqbn: "x:y:z".into(), port: Some(format!("/dev/p{j}")), sketch: None });
                app.panes.push(crate::app::Pane { lines: vec![], scroll: 0, baud: 9600, link: None });
            }
            app.key(KeyEvent::from(KeyCode::Char('m')));
            assert_eq!(app.shown_panes().len(), n);
            term.draw(|f| render(f, &app)).unwrap();
            let screen: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            assert!((0..n).all(|j| screen.contains(&format!("board{j}"))), "{n} panes");
        }
    }
}
