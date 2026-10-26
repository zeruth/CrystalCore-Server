//! The server's console: a full-screen terminal UI over a running [`Server`]. A status line on
//! top, the log in the middle (colored by level, scrollable), and a prompt at the bottom for the
//! administration [`commands`] (`help` lists them), with history and Tab completion. The mouse
//! wheel, Shift+arrows and PgUp/PgDn scroll the log; F2 steps the log level.
//!
//! Only the binary uses it; anything not on a terminal logs plainly instead.

pub mod commands;
mod highlight;
pub mod logs;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crystal_common::Role;
use crystal_runtime::Server;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
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
/// Rows a mouse wheel notch scrolls.
const WHEEL_ROWS: usize = 3;

/// The console's colors: blues, with the levels' own.
mod theme {
    use ratatui::style::Color;

    /// The console's background, and the header's and the prompt's.
    pub const BG: Color = Color::Rgb(10, 22, 44);
    pub const PANEL: Color = Color::Rgb(18, 38, 72);
    pub const ACCENT: Color = Color::Rgb(88, 166, 255);
    pub const ACCENT_DEEP: Color = Color::Rgb(31, 111, 235);
    pub const RULE: Color = Color::Rgb(40, 72, 120);
    pub const DIM: Color = Color::Rgb(110, 118, 129);
    pub const TEXT: Color = Color::Rgb(201, 209, 217);
    pub const OUTPUT: Color = Color::Rgb(165, 214, 255);
    pub const ERROR: Color = Color::Rgb(248, 81, 73);
    pub const ERROR_TEXT: Color = Color::Rgb(255, 161, 152);
    pub const WARN: Color = Color::Rgb(210, 153, 34);
    pub const WARN_TEXT: Color = Color::Rgb(242, 204, 96);
    pub const DEBUG: Color = Color::Rgb(57, 197, 207);
    pub const TRACE: Color = Color::Rgb(188, 140, 255);
    pub const ONLINE: Color = Color::Rgb(63, 185, 80);
    pub const GM: Color = Color::Rgb(255, 215, 64);
}

/// The levels F2 steps through.
const LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];

/// What the input thread reads.
enum Input {
    Key(KeyEvent),
    /// Mouse wheel: rows up (positive) or down.
    Wheel(i32),
}

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
    // the wheel scrolls the log (Shift+drag still selects text in most terminals)
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
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
            let mut max_scroll = app.scroll;
            if let Err(e) = terminal.draw(|f| max_scroll = draw(f, &app, &server, &logging)) {
                break Err(e.into());
            }
            // scrolled past the oldest line: hold at it, so scrolling down answers at once
            app.scroll = app.scroll.min(max_scroll);
            last_draw = Instant::now();
            dirty = false;
        }
        tokio::select! {
            input = keys.recv() => {
                let Some(input) = input else { break Ok(()) };
                dirty = true;
                let key = match input {
                    Input::Wheel(rows) => {
                        app.scroll = app.scroll.saturating_add_signed(rows as isize * WHEEL_ROWS as isize);
                        continue;
                    }
                    Input::Key(key) => key,
                };
                match on_key(&mut app, key, &terminal) {
                    Action::None => {}
                    Action::Quit => break Ok(()),
                    Action::CycleLevel => {
                        let now = logging.filter();
                        let next = LEVELS.iter().position(|l| now.eq_ignore_ascii_case(l)).map_or(2, |i| (i + 1) % LEVELS.len());
                        match logging.set_filter(LEVELS[next]) {
                            Ok(()) => sink.push(Kind::Output, format!("log level: {}", LEVELS[next])),
                            Err(e) => sink.push(Kind::Error, format!("{e:#}")),
                        }
                    }
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
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    drop(terminal);
    ratatui::restore();
    let _ = reader.join();
    sink.set_passthrough(true);
    tracing::info!("shutting down");
    server.shutdown().await;
    result
}

/// Terminal events on a thread of their own (crossterm's reads block).
fn read_keys(tx: mpsc::UnboundedSender<Input>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match event::poll(Duration::from_millis(100)) {
            Ok(true) => match event::read() {
                // Windows reports releases too
                Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                    if tx.send(Input::Key(k)).is_err() {
                        return;
                    }
                }
                Ok(Event::Mouse(m)) => {
                    let rows = match m.kind {
                        MouseEventKind::ScrollUp => 1,
                        MouseEventKind::ScrollDown => -1,
                        _ => continue,
                    };
                    if tx.send(Input::Wheel(rows)).is_err() {
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
    CycleLevel,
    Quit,
}

fn on_key(app: &mut App, key: KeyEvent, terminal: &DefaultTerminal) -> Action {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
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
        KeyCode::F(2) => return Action::CycleLevel,
        KeyCode::Up if shift => app.scroll = app.scroll.saturating_add(1),
        KeyCode::Down if shift => app.scroll = app.scroll.saturating_sub(1),
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
                let choices = commands::complete(text.trim_start_matches(['/', '.']));
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

/// Draws the console; returns how far back the log can scroll (in rows).
fn draw(f: &mut Frame, app: &App, server: &Server, logging: &Logging) -> usize {
    let [header, rule, body, input, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
    f.render_widget(Block::new().style(Style::new().bg(theme::BG)), f.area());
    f.render_widget(Paragraph::new(status_line(app, server, logging)).style(Style::new().bg(theme::PANEL)), header);
    f.render_widget(Paragraph::new("━".repeat(rule.width as usize)).fg(theme::RULE), rule);
    let max_scroll = draw_log(f, app, logging, body);
    draw_input(f, app, input);
    f.render_widget(Paragraph::new(footer_line(app, footer.width)), footer);
    max_scroll
}

fn status_line(app: &App, server: &Server, logging: &Logging) -> Line<'static> {
    let dim = Style::new().fg(theme::DIM);
    let sep = || Span::styled("  │  ", Style::new().fg(theme::RULE));
    let mut spans = vec![
        Span::styled(" ✦ ", Style::new().fg(theme::ACCENT).bold()),
        Span::styled("CrystalCore", Style::new().fg(theme::ACCENT).bold()),
        Span::styled("-Server", Style::new().fg(theme::TEXT).bold()),
        Span::styled(format!(" {}", env!("CARGO_PKG_VERSION")), dim),
    ];
    for (role, name) in [(Role::Lobby, "lobby"), (Role::Zone, "zone"), (Role::Chat, "chat")] {
        if let Some(addr) = server.local_addr(role) {
            spans.push(sep());
            spans.push(Span::styled(format!("{name} "), dim));
            spans.push(Span::styled(addr.to_string(), Style::new().fg(theme::TEXT)));
        }
    }
    if let Some(world) = server.world() {
        spans.push(sep());
        spans.push(Span::styled(format!("● {} online", world.online().len()), Style::new().fg(theme::ONLINE)));
    }
    let secs = app.started.elapsed().as_secs();
    spans.push(sep());
    spans.push(Span::styled(format!("up {:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60), dim));
    spans.push(sep());
    spans.push(Span::styled("log ", dim));
    let filter = logging.filter();
    let level_color = LEVELS.iter().position(|l| filter.eq_ignore_ascii_case(l)).map_or(theme::ACCENT, |i| {
        level_colors([Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG, Level::TRACE][i]).0
    });
    spans.push(Span::styled(filter, Style::new().fg(level_color).bold()));
    Line::from(spans)
}

/// A level's badge color and the color its lines' text takes.
fn level_colors(level: Level) -> (Color, Color) {
    match level {
        Level::ERROR => (theme::ERROR, theme::ERROR_TEXT),
        Level::WARN => (theme::WARN, theme::WARN_TEXT),
        Level::INFO => (theme::ACCENT_DEEP, theme::TEXT),
        Level::DEBUG => (theme::DEBUG, theme::DIM),
        Level::TRACE => (theme::TRACE, theme::DIM),
    }
}

fn level_badge(level: Level) -> Span<'static> {
    let name = match level {
        Level::ERROR => " ERR ",
        Level::WARN => " WRN ",
        Level::INFO => " INF ",
        Level::DEBUG => " DBG ",
        Level::TRACE => " TRC ",
    };
    let (badge, _) = level_colors(level);
    Span::styled(name, Style::new().fg(Color::Black).bg(badge).bold())
}

/// One log line as screen rows `width` wide: a styled prefix, then the text, wrapped under it.
fn rows(line: &logs::Line, width: usize) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::DIM);
    let (prefix, pieces): (Vec<Span<'static>>, Vec<(String, Style)>) = match line.kind {
        Kind::Log(level) => {
            let (_, text) = level_colors(level);
            (
                vec![Span::styled(format!("{} ", line.time), dim), level_badge(level), Span::raw(" ")],
                highlight::segments(&line.text, Style::new().fg(text)),
            )
        }
        Kind::Input => {
            // the command name bold, its arguments plain
            let (name, args) = line.text.split_once(' ').unwrap_or((&line.text, ""));
            let mut p = vec![(name.to_owned(), Style::new().fg(theme::ACCENT).bold())];
            if !args.is_empty() {
                p.push((format!(" {args}"), Style::new().fg(theme::TEXT)));
            }
            (vec![Span::styled(PROMPT.to_string(), Style::new().fg(theme::ACCENT).bold())], p)
        }
        Kind::Output => {
            // GM changes stand out from the rest of the answers
            let style = if line.text.contains("GM level") || line.text.contains("GM mode") {
                Style::new().fg(theme::GM).bold()
            } else {
                Style::new().fg(theme::OUTPUT)
            };
            (vec![Span::styled("  ⎿ ".to_string(), dim)], highlight::segments(&line.text, style))
        }
        Kind::Error => {
            (vec![Span::styled("  ⎿ ".to_string(), dim)], vec![(line.text.clone(), Style::new().fg(theme::ERROR_TEXT))])
        }
    };
    let indent: usize = prefix.iter().map(|s| s.content.chars().count()).sum();
    let room = width.saturating_sub(indent).max(10);
    let mut out = Vec::new();
    for (i, row) in highlight::wrap(pieces, room).into_iter().enumerate() {
        let mut spans = if i == 0 { prefix.clone() } else { vec![Span::raw(" ".repeat(indent))] };
        spans.extend(row.into_iter().map(|(t, s)| Span::styled(t, s)));
        out.push(Line::from(spans));
    }
    out
}

/// Draws the log at the app's scroll; returns how far back it can scroll.
fn draw_log(f: &mut Frame, app: &App, logging: &Logging, area: Rect) -> usize {
    let (height, width) = (area.height as usize, area.width as usize);
    let want = height + app.scroll;
    // the newest rows, enough to fill the view at this scroll, newest last; `all` tells whether
    // they reach the oldest line
    let (mut visible, all) = logging.sink.with_lines(|lines| {
        let mut taken: Vec<Line<'static>> = Vec::new();
        let mut all = true;
        for (i, line) in lines.iter().rev().enumerate() {
            let mut r = rows(line, width);
            r.reverse();
            taken.extend(r);
            if taken.len() >= want && i + 1 < lines.len() {
                all = false;
                break;
            }
        }
        taken.reverse();
        (taken, all)
    });
    let total = visible.len();
    let max_scroll = if all { total.saturating_sub(height) } else { usize::MAX };
    let scroll = app.scroll.min(total.saturating_sub(height));
    let end = total - scroll;
    let start = end.saturating_sub(height);
    let mut shown: Vec<Line<'static>> = visible.drain(start..end).collect();
    // newest at the bottom, like a terminal
    while shown.len() < height {
        shown.insert(0, Line::raw(""));
    }
    f.render_widget(Paragraph::new(shown), area);
    max_scroll
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let border = if app.quit_armed.is_some() { theme::WARN } else { theme::ACCENT_DEEP };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .style(Style::new().bg(theme::PANEL));
    let inner = block.inner(area);
    let room = (inner.width as usize).saturating_sub(PROMPT.chars().count() + 1).max(1);
    // keep the cursor in view
    let skip = app.cursor.saturating_sub(room);
    let text: String = app.input.iter().skip(skip).take(room).collect();
    let mut spans = vec![Span::styled(PROMPT, Style::new().fg(theme::ACCENT).bold())];
    if app.input.is_empty() {
        spans.push(Span::styled("type a command (help lists them)", Style::new().fg(theme::DIM).italic()));
    } else {
        spans.push(Span::styled(text, Style::new().fg(theme::TEXT)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
    let x = inner.x + (PROMPT.chars().count() + app.cursor - skip) as u16;
    f.set_cursor_position((x.min(inner.right().saturating_sub(1)), inner.y));
}

fn footer_line(app: &App, width: u16) -> Line<'static> {
    let dim = Style::new().fg(theme::DIM);
    let key = Style::new().fg(theme::ACCENT);
    if app.quit_armed.is_some_and(|t| t.elapsed() < QUIT_WINDOW) {
        return Line::from(Span::styled("  Press Ctrl+C again to stop the server", Style::new().fg(theme::WARN_TEXT)));
    }
    if let Some(hint) = &app.hint {
        return Line::from(Span::styled(format!("  {hint}"), key));
    }
    let mut spans = vec![Span::raw("  ")];
    for (i, (k, what)) in [
        ("help", "commands"),
        ("↑↓", "history"),
        ("Tab", "complete"),
        ("wheel/PgUp/PgDn", "scroll"),
        ("F2", "log level"),
        ("Ctrl+C×2", "stop"),
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            spans.push(Span::styled(" · ", dim));
        }
        spans.push(Span::styled(k, key));
        spans.push(Span::styled(format!(" {what}"), dim));
    }
    let left: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let right = if app.scroll > 0 { format!("↑ {} rows back · Esc to follow  ", app.scroll) } else { String::new() };
    let pad = (width as usize).saturating_sub(left + right.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(right, Style::new().fg(theme::WARN_TEXT)));
    Line::from(spans)
}
