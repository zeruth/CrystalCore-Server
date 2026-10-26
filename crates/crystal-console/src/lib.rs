//! The server's console: a full-screen terminal UI over a running [`Server`]. A status line on
//! top, the log in the middle (colored by level, scrollable), and a prompt at the bottom for the
//! administration [`commands`] (`help` lists them), with history and Tab completion.
//!
//! Only the binary uses it; anything not on a terminal logs plainly instead.

pub mod commands;
pub mod logs;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crystal_common::Role;
use crystal_runtime::Server;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc;
use tracing::Level;

use crate::commands::Outcome;
use crate::logs::{Kind, Logging};

/// Ctrl+C twice within this stops the server.
const QUIT_WINDOW: Duration = Duration::from_secs(2);
const PROMPT: &str = "❯ ";

struct App {
    input: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Where Up/Down is in `history`; `None` is the line being typed.
    browsing: Option<usize>,
    /// Rows scrolled up from the newest line; 0 follows the log.
    scroll: usize,
    seen: u64,
    quit_armed: Option<Instant>,
    started: Instant,
    /// Shown under the prompt until the next key (Tab's choices, for one).
    hint: Option<String>,
}

/// Runs the console until it is told to quit, then stops the server.
pub async fn run(server: Server, logging: Logging) -> anyhow::Result<()> {
    let sink = logging.sink.clone();
    let mut terminal = ratatui::init();
    let (keys_tx, mut keys) = mpsc::unbounded_channel();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let stop = stop.clone();
        std::thread::Builder::new().name("console-input".into()).spawn(move || read_keys(keys_tx, stop))?
    };
    let mut app = App {
        input: vec![],
        cursor: 0,
        history: vec![],
        browsing: None,
        scroll: 0,
        seen: 0,
        quit_armed: None,
        started: Instant::now(),
        hint: None,
    };
    sink.push(Kind::Output, "Console ready. Type help for commands.");
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    let mut dirty = true;
    let result = loop {
        let version = sink.version();
        if version != app.seen {
            if app.scroll > 0 {
                // keep what is being read where it is
                app.scroll += (version - app.seen) as usize;
            }
            app.seen = version;
            dirty = true;
        }
        if dirty || last_draw.elapsed() >= Duration::from_secs(1) {
            if let Err(e) = terminal.draw(|f| draw(f, &app, &server, &logging)) {
                break Err(e.into());
            }
            last_draw = Instant::now();
            dirty = false;
        }
        tokio::select! {
            key = keys.recv() => {
                let Some(key) = key else { break Ok(()) };
                dirty = true;
                match on_key(&mut app, key, &terminal) {
                    Action::None => {}
                    Action::Quit => break Ok(()),
                    Action::Run(line) => {
                        sink.push(Kind::Input, line.clone());
                        match commands::run(&line, &server, &logging, app.started).await {
                            Ok((out, outcome)) => {
                                for l in out {
                                    sink.push(Kind::Output, l);
                                }
                                match outcome {
                                    Outcome::Continue => {}
                                    Outcome::Clear => sink.clear(),
                                    Outcome::Quit => break Ok(()),
                                }
                            }
                            Err(e) => sink.push(Kind::Error, format!("{e:#}")),
                        }
                        app.scroll = 0;
                    }
                }
            }
            _ = tick.tick() => {}
        }
    };
    stop.store(true, Ordering::Relaxed);
    drop(terminal);
    ratatui::restore();
    let _ = reader.join();
    sink.set_passthrough(true);
    tracing::info!("shutting down");
    server.shutdown().await;
    result
}

/// Terminal events on a thread of their own (crossterm's reads block).
fn read_keys(tx: mpsc::UnboundedSender<KeyEvent>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                // Windows reports releases too
                Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                    if tx.send(k).is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(_) => return,
            },
            Ok(false) => {}
            Err(_) => return,
        }
    }
}

enum Action {
    None,
    Run(String),
    Quit,
}

fn on_key(app: &mut App, key: KeyEvent, terminal: &DefaultTerminal) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    app.hint = None;
    if !(ctrl && key.code == KeyCode::Char('c')) {
        app.quit_armed = None;
    }
    let page = terminal.size().map_or(10, |s| s.height.saturating_sub(7).max(1) as usize);
    match key.code {
        KeyCode::Char('c') if ctrl => {
            if !app.input.is_empty() {
                app.input.clear();
                app.cursor = 0;
            } else if app.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
                return Action::Quit;
            } else {
                app.quit_armed = Some(Instant::now());
            }
        }
        KeyCode::Char('u') if ctrl => {
            app.input.drain(..app.cursor);
            app.cursor = 0;
        }
        KeyCode::Char('l') if ctrl => app.scroll = 0,
        KeyCode::Char(c) => {
            app.input.insert(app.cursor, c);
            app.cursor += 1;
            app.browsing = None;
        }
        KeyCode::Backspace if app.cursor > 0 => {
            app.cursor -= 1;
            app.input.remove(app.cursor);
        }
        KeyCode::Delete if app.cursor < app.input.len() => {
            app.input.remove(app.cursor);
        }
        KeyCode::Left => app.cursor = app.cursor.saturating_sub(1),
        KeyCode::Right => app.cursor = (app.cursor + 1).min(app.input.len()),
        KeyCode::Home if ctrl => app.scroll = usize::MAX / 2,
        KeyCode::End if ctrl => app.scroll = 0,
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.len(),
        KeyCode::PageUp => app.scroll = app.scroll.saturating_add(page),
        KeyCode::PageDown => app.scroll = app.scroll.saturating_sub(page),
        KeyCode::Esc => {
            if app.input.is_empty() {
                app.scroll = 0;
            }
            app.input.clear();
            app.cursor = 0;
            app.browsing = None;
        }
        KeyCode::Up if !app.history.is_empty() => {
            let i = match app.browsing {
                None => app.history.len() - 1,
                Some(i) => i.saturating_sub(1),
            };
            app.browsing = Some(i);
            app.input = app.history[i].chars().collect();
            app.cursor = app.input.len();
        }
        KeyCode::Down => {
            if let Some(i) = app.browsing {
                if i + 1 < app.history.len() {
                    app.browsing = Some(i + 1);
                    app.input = app.history[i + 1].chars().collect();
                } else {
                    app.browsing = None;
                    app.input.clear();
                }
                app.cursor = app.input.len();
            }
        }
        KeyCode::Tab => {
            let text: String = app.input.iter().collect();
            if !text.contains(char::is_whitespace) {
                let choices = commands::complete(text.trim_start_matches('/'));
                match choices.as_slice() {
                    [] => {}
                    [one] => {
                        app.input = format!("{one} ").chars().collect();
                        app.cursor = app.input.len();
                    }
                    many => app.hint = Some(many.join("  ")),
                }
            }
        }
        KeyCode::Enter => {
            let line: String = app.input.iter().collect();
            app.input.clear();
            app.cursor = 0;
            app.browsing = None;
            let line = line.trim().to_owned();
            if line.is_empty() {
                return Action::None;
            }
            if app.history.last() != Some(&line) {
                app.history.push(line.clone());
            }
            return Action::Run(line);
        }
        _ => {}
    }
    Action::None
}

fn draw(f: &mut Frame, app: &App, server: &Server, logging: &Logging) {
    let [header, rule, body, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    f.render_widget(Paragraph::new(status_line(app, server, logging)), header);
    f.render_widget(Paragraph::new("─".repeat(rule.width as usize)).fg(Color::DarkGray), rule);
    draw_log(f, app, logging, body);
    draw_input(f, app, input);
    f.render_widget(Paragraph::new(footer_line(app, footer.width)), footer);
}

fn status_line(app: &App, server: &Server, logging: &Logging) -> Line<'static> {
    let dim = Style::new().fg(Color::DarkGray);
    let mut spans = vec![
        Span::styled(" ✻ ", Style::new().fg(Color::Rgb(215, 119, 87)).bold()),
        Span::styled("CrystalCore-Server", Style::new().bold()),
        Span::styled(format!(" {}", env!("CARGO_PKG_VERSION")), dim),
    ];
    for (role, name) in [(Role::Lobby, "lobby"), (Role::Zone, "zone"), (Role::Chat, "chat")] {
        if let Some(addr) = server.local_addr(role) {
            spans.push(Span::styled("  │  ", dim));
            spans.push(Span::styled(format!("{name} "), dim));
            spans.push(Span::raw(addr.to_string()));
        }
    }
    if let Some(world) = server.world() {
        spans.push(Span::styled("  │  ", dim));
        spans.push(Span::styled(format!("{} online", world.online().len()), Style::new().fg(Color::Green)));
    }
    let secs = app.started.elapsed().as_secs();
    spans.push(Span::styled("  │  ", dim));
    spans.push(Span::styled(format!("up {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60), dim));
    spans.push(Span::styled("  │  ", dim));
    spans.push(Span::styled(format!("log {}", logging.filter()), dim));
    Line::from(spans)
}

fn level_style(level: Level) -> (&'static str, Style) {
    match level {
        Level::ERROR => ("ERROR", Style::new().fg(Color::Red).bold()),
        Level::WARN => ("WARN ", Style::new().fg(Color::Yellow).bold()),
        Level::INFO => ("INFO ", Style::new().fg(Color::Green)),
        Level::DEBUG => ("DEBUG", Style::new().fg(Color::Blue)),
        Level::TRACE => ("TRACE", Style::new().fg(Color::Magenta)),
    }
}

/// One log line as screen rows `width` wide: a styled prefix, then the text, wrapped under it.
fn rows(line: &logs::Line, width: usize) -> Vec<Line<'static>> {
    let dim = Style::new().fg(Color::DarkGray);
    let (prefix, text_style): (Vec<Span<'static>>, Style) = match line.kind {
        Kind::Log(level) => {
            let (name, style) = level_style(level);
            let text = if level <= Level::WARN { style.remove_modifier(Modifier::BOLD) } else { Style::new() };
            (vec![Span::styled(format!("{} ", line.time), dim), Span::styled(format!("{name} "), style)], text)
        }
        Kind::Input => (
            vec![Span::styled(PROMPT.to_string(), Style::new().fg(Color::Cyan).bold())],
            Style::new().fg(Color::Cyan).bold(),
        ),
        Kind::Output => (vec![Span::styled("  ⎿ ".to_string(), dim)], Style::new().fg(Color::Gray)),
        Kind::Error => (vec![Span::styled("  ⎿ ".to_string(), dim)], Style::new().fg(Color::Red)),
    };
    let indent: usize = prefix.iter().map(|s| s.content.chars().count()).sum();
    let room = width.saturating_sub(indent).max(10);
    let chars: Vec<char> = line.text.chars().collect();
    let mut out = Vec::new();
    let mut chunks = chars.chunks(room).peekable();
    let first: String = chunks.next().map(|c| c.iter().collect()).unwrap_or_default();
    let mut spans = prefix;
    spans.push(Span::styled(first, text_style));
    out.push(Line::from(spans));
    for chunk in chunks {
        out.push(Line::from(vec![
            Span::raw(" ".repeat(indent)),
            Span::styled(chunk.iter().collect::<String>(), text_style),
        ]));
    }
    out
}

fn draw_log(f: &mut Frame, app: &App, logging: &Logging, area: Rect) {
    let (height, width) = (area.height as usize, area.width as usize);
    let want = height + app.scroll;
    // the newest rows, enough to fill the view at this scroll, newest last
    let (mut visible, total) = logging.sink.with_lines(|lines| {
        let mut taken: Vec<Line<'static>> = Vec::new();
        for line in lines.iter().rev() {
            let mut r = rows(line, width);
            r.reverse();
            taken.extend(r);
            if taken.len() >= want {
                break;
            }
        }
        let total = taken.len();
        taken.reverse();
        (taken, total)
    });
    let scroll = app.scroll.min(total.saturating_sub(height));
    let end = visible.len() - scroll;
    let start = end.saturating_sub(height);
    let mut shown: Vec<Line<'static>> = visible.drain(start..end).collect();
    // newest at the bottom, like a terminal
    while shown.len() < height {
        shown.insert(0, Line::raw(""));
    }
    f.render_widget(Paragraph::new(shown), area);
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let border = if app.quit_armed.is_some() { Color::Yellow } else { Color::DarkGray };
    let block = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(border));
    let inner = block.inner(area);
    let room = (inner.width as usize).saturating_sub(PROMPT.chars().count() + 1).max(1);
    // keep the cursor in view
    let skip = app.cursor.saturating_sub(room);
    let text: String = app.input.iter().skip(skip).take(room).collect();
    let mut spans = vec![Span::styled(PROMPT, Style::new().fg(Color::Cyan).bold())];
    if app.input.is_empty() {
        spans.push(Span::styled("type a command (help lists them)", Style::new().fg(Color::DarkGray).italic()));
    } else {
        spans.push(Span::raw(text));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    let x = inner.x + (PROMPT.chars().count() + app.cursor - skip) as u16;
    f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
}

fn footer_line(app: &App, width: u16) -> Line<'static> {
    let dim = Style::new().fg(Color::DarkGray);
    if app.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
        return Line::from(Span::styled("  Press Ctrl+C again to stop the server", Style::new().fg(Color::Yellow)));
    }
    if let Some(hint) = &app.hint {
        return Line::from(Span::styled(format!("  {hint}"), Style::new().fg(Color::Cyan)));
    }
    let left = "  help · ↑↓ history · Tab complete · PgUp/PgDn scroll · Ctrl+C twice stops";
    let right =
        if app.scroll > 0 { format!("scrolled back {} rows · Esc to follow  ", app.scroll) } else { String::new() };
    let pad = (width as usize).saturating_sub(left.chars().count() + right.chars().count());
    Line::from(vec![
        Span::styled(left, dim),
        Span::raw(" ".repeat(pad)),
        Span::styled(right, Style::new().fg(Color::Yellow)),
    ])
}
