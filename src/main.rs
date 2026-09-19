mod app;
mod cli;
mod project;
mod settings;
mod sketch;
mod ui;

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures::StreamExt;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc;

use app::{App, AppEvent};

const PORT_POLL: Duration = Duration::from_secs(2);

#[tokio::main]
async fn main() -> Result<()> {
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h" | "--help")) {
        println!("usage: fluxus [DIR]\n\nDIR is a project folder or a single sketch (default: current directory).");
        return Ok(());
    }
    let path = Path::new(arg.as_deref().unwrap_or("."));
    let root = path.canonicalize().with_context(|| format!("{} not found", path.display()))?;
    let root = if root.is_file() { root.parent().context("no parent folder")?.to_path_buf() } else { root };
    let cli_version = cli::check_version().await?;

    let (tx, rx) = mpsc::unbounded_channel();
    let mut app = App::new(cli_version, root, tx.clone());

    // -- background --

    let ports_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            let r = cli::board_list().await.map_err(|e| format!("{e:#}"));
            if ports_tx.send(AppEvent::Ports(r)).is_err() {
                break;
            }
            tokio::time::sleep(PORT_POLL).await;
        }
    });
    if app.wants_defaults() {
        let (tx, dir) = (tx.clone(), app.root.clone());
        tokio::spawn(async move {
            if let Ok(d) = cli::attached(&dir).await {
                let _ = tx.send(AppEvent::Defaults(d));
            }
        });
    }
    app.fetch_boards();

    // -- ui --

    let mut terminal = ratatui::init();
    let res = run(&mut terminal, &mut app, rx).await;
    ratatui::restore();
    res
}

async fn run(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    mut rx: mpsc::UnboundedReceiver<AppEvent>,
) -> Result<()> {
    let mut input = EventStream::new();
    while !app.quit {
        terminal.draw(|f| ui::render(f, app))?;
        tokio::select! {
            Some(ev) = input.next() => {
                if let Event::Key(k) = ev? && k.kind == KeyEventKind::Press {
                    app.key(k);
                }
            }
            Some(ev) = rx.recv() => app.event(ev),
        }
    }
    Ok(())
}
