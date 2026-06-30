//! Full-screen session board, in the spirit of herdr's UI: workspaces grouped
//! in a sidebar, live agent statuses (loading / working / idle / suspended /
//! done / error) with a spinner for active ones, a detail+activity panel for the
//! selected session, and a footer with the path, branch, and keybindings.

use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use shepherd_agent::ClaudeStreamParser;
use shepherd_core::agent::AgentEvent;
use shepherd_core::sandbox::{SandboxProvider, SandboxStatus};
use shepherd_core::session::{Session, SessionStatus};
use shepherd_core::workspace::WorkspaceSpec;

use crate::store::Store;

// A small catppuccin-ish palette.
const ACCENT: Color = Color::Rgb(203, 166, 247); // mauve
const GREEN: Color = Color::Rgb(166, 227, 161);
const YELLOW: Color = Color::Rgb(249, 226, 175);
const RED: Color = Color::Rgb(243, 139, 168);
const BLUE: Color = Color::Rgb(137, 180, 250);
const TEAL: Color = Color::Rgb(148, 226, 213);
const SUBTEXT: Color = Color::Rgb(166, 173, 200);
const SELECT_BG: Color = Color::Rgb(49, 50, 68);

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

type Term = Terminal<CrosstermBackend<Stdout>>;

struct Row {
    session: Session,
    live: Option<SandboxStatus>,
}

struct App {
    rows: Vec<Row>,
    selected: usize,
    tick: u64,
    log_lines: Vec<Line<'static>>,
}

enum Action {
    None,
    Quit,
    Attach(String),
    Delete(String),
}

pub async fn run_tui(store: &Store, provider: &dyn SandboxProvider) -> Result<()> {
    let mut term = setup_terminal()?;
    let mut app = App { rows: Vec::new(), selected: 0, tick: 0, log_lines: Vec::new() };
    app.refresh(store, provider).await;
    app.load_log(provider).await;

    let mut events = EventStream::new();
    let mut spin = tokio::time::interval(Duration::from_millis(120));
    let mut refresh = tokio::time::interval(Duration::from_secs(2));
    refresh.tick().await; // consume the immediate first tick

    let result = loop {
        if let Err(e) = term.draw(|f| ui(f, &app)) {
            break Err(e.into());
        }
        tokio::select! {
            _ = spin.tick() => app.tick = app.tick.wrapping_add(1),
            _ = refresh.tick() => {
                app.refresh(store, provider).await;
                app.load_log(provider).await;
            }
            maybe = events.next() => {
                let Some(Ok(Event::Key(key))) = maybe else { continue };
                if key.kind != KeyEventKind::Press { continue }
                match app.on_key(key.code) {
                    Action::None => {}
                    Action::Quit => break Ok(()),
                    Action::Delete(id) => {
                        if let Ok(Some(s)) = store.get(&id.as_str().into()) {
                            if let Some(sb) = &s.sandbox_id { let _ = provider.destroy(sb).await; }
                            let _ = store.delete(&id.as_str().into());
                        }
                        app.refresh(store, provider).await;
                    }
                    Action::Attach(id) => {
                        // Drop out of the TUI, run the interactive attach, then return.
                        restore_terminal(&mut term)?;
                        let _ = crate::attach::attach(store, provider, &id).await;
                        term = setup_terminal()?;
                        app.refresh(store, provider).await;
                        app.load_log(provider).await;
                    }
                }
            }
        }
    };

    restore_terminal(&mut term)?;
    result
}

impl App {
    async fn refresh(&mut self, store: &Store, provider: &dyn SandboxProvider) {
        let mut sessions = store.list().unwrap_or_default();
        // Group visually by workspace (repo), like herdr's workspaces.
        sessions.sort_by(|a, b| {
            repo_label(&a.workspace).cmp(&repo_label(&b.workspace)).then(a.created_at.cmp(&b.created_at))
        });
        let mut rows = Vec::new();
        for s in sessions {
            let live = match &s.sandbox_id {
                Some(id) => provider.get(id).await.ok().flatten().map(|sb| sb.status),
                None => None,
            };
            rows.push(Row { session: s, live });
        }
        self.rows = rows;
        if self.selected >= self.rows.len() {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    async fn load_log(&mut self, provider: &dyn SandboxProvider) {
        self.log_lines.clear();
        let Some(row) = self.rows.get(self.selected) else { return };
        let Some(sandbox_id) = row.session.sandbox_id.clone() else { return };
        let mount = row.session.workspace.mount_path();
        let path = format!("{mount}/{}", crate::AGENT_LOG_REL);
        let Ok(bytes) = provider.get_file(&sandbox_id, &path).await else { return };
        let text = String::from_utf8_lossy(&bytes);
        let mut parser = ClaudeStreamParser::new();
        let mut events = parser.feed(&text);
        events.extend(parser.flush());
        for ev in events {
            match ev {
                AgentEvent::Text { text } => {
                    for line in text.lines() {
                        self.log_lines.push(Line::raw(line.to_string()));
                    }
                }
                AgentEvent::ToolUse { name, .. } => {
                    self.log_lines.push(Line::from(Span::styled(format!("  · {name}"), Style::default().fg(TEAL))));
                }
                AgentEvent::Error { message } => {
                    self.log_lines.push(Line::from(Span::styled(format!("  ! {message}"), Style::default().fg(RED))));
                }
                _ => {}
            }
        }
    }

    fn on_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                Action::None
            }
            KeyCode::Enter | KeyCode::Char('a') => match self.rows.get(self.selected) {
                Some(r) => Action::Attach(r.session.id.to_string()),
                None => Action::None,
            },
            KeyCode::Char('d') => match self.rows.get(self.selected) {
                Some(r) => Action::Delete(r.session.id.to_string()),
                None => Action::None,
            },
            _ => Action::None,
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0), Constraint::Length(1)])
        .split(f.area());
    render_title(f, chunks[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(chunks[1]);
    render_sidebar(f, body[0], app);
    render_detail(f, body[1], app);
    render_footer(f, chunks[2], app);
}

fn render_title(f: &mut Frame, area: Rect, app: &App) {
    let mut counts = [0usize; 6]; // working, loading, idle, suspended, done, error
    for r in &app.rows {
        match r.session.status {
            SessionStatus::Running => counts[0] += 1,
            SessionStatus::Seeding | SessionStatus::Pending => counts[1] += 1,
            SessionStatus::Idle => counts[2] += 1,
            SessionStatus::Suspended => counts[3] += 1,
            SessionStatus::Done => counts[4] += 1,
            SessionStatus::Error => counts[5] += 1,
        }
    }
    let mut spans = vec![
        Span::styled(" shepherd ", Style::default().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
    ];
    let summary = [
        (counts[0], "working", GREEN),
        (counts[1], "loading", YELLOW),
        (counts[2], "idle", TEAL),
        (counts[3], "suspended", BLUE),
        (counts[4], "done", GREEN),
        (counts[5], "error", RED),
    ];
    let parts: Vec<(usize, &str, Color)> = summary.into_iter().filter(|(n, _, _)| *n > 0).collect();
    for (i, (n, label, color)) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(SUBTEXT)));
        }
        spans.push(Span::styled(format!("{n} {label}"), Style::default().fg(*color)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_sidebar(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" workspaces ").border_style(Style::default().fg(SUBTEXT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.rows.is_empty() {
        let hint = Paragraph::new(vec![
            Line::raw(""),
            Line::from(Span::styled("  no sessions yet", Style::default().fg(SUBTEXT))),
            Line::raw(""),
            Line::from(Span::styled("  start one:", Style::default().fg(SUBTEXT))),
            Line::from(Span::styled("  shepherd run --agent --prompt ...", Style::default().fg(ACCENT))),
        ]);
        f.render_widget(hint, inner);
        return;
    }

    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = 0usize;
    let mut last_repo: Option<String> = None;
    for (i, row) in app.rows.iter().enumerate() {
        let repo = repo_label(&row.session.workspace);
        if last_repo.as_deref() != Some(repo.as_str()) {
            if last_repo.is_some() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::from(Span::styled(format!("▾ {repo}"), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))));
            last_repo = Some(repo);
        }
        let (icon, color, label) = status_display(row.session.status, app.tick);
        let selected = i == app.selected;
        if selected {
            selected_line = lines.len();
        }
        let row_style = if selected {
            Style::default().bg(SELECT_BG).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let spans = vec![
            Span::raw(if selected { " ❯ " } else { "   " }),
            Span::styled(format!("{icon} "), Style::default().fg(color)),
            Span::styled(format!("{label:<9} "), Style::default().fg(color)),
            Span::styled(row.session.title.clone(), Style::default().fg(Color::White)),
        ];
        lines.push(Line::from(spans).style(row_style));
    }

    // Scroll so the selected line stays visible.
    let height = inner.height as usize;
    let offset = selected_line.saturating_sub(height.saturating_sub(1));
    f.render_widget(Paragraph::new(lines).scroll((offset as u16, 0)), inner);
}

fn render_detail(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" detail ").border_style(Style::default().fg(SUBTEXT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let Some(row) = app.rows.get(app.selected) else { return };
    let s = &row.session;
    let (icon, color, label) = status_display(s.status, app.tick);

    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(0)])
        .split(inner);

    let live = row.live.map(|st| format!("{st:?}")).unwrap_or_else(|| "-".into());
    let mut head = vec![
        Line::from(Span::styled(s.title.clone(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD))),
        Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(color)),
            Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {}", elapsed(&s.updated_at)), Style::default().fg(SUBTEXT)),
        ]),
        Line::from(vec![
            Span::styled("session  ", Style::default().fg(SUBTEXT)),
            Span::raw(s.id.to_string()),
        ]),
        Line::from(vec![
            Span::styled("sandbox  ", Style::default().fg(SUBTEXT)),
            Span::raw(s.sandbox_id.as_ref().map(|i| i.to_string()).unwrap_or_default()),
            Span::styled(format!("  ({live})"), Style::default().fg(SUBTEXT)),
        ]),
        Line::from(vec![
            Span::styled("branch   ", Style::default().fg(SUBTEXT)),
            Span::styled(s.branch.clone(), Style::default().fg(GREEN)),
        ]),
    ];
    if let Some(err) = &s.error {
        head.push(Line::from(Span::styled(format!("error    {err}"), Style::default().fg(RED))));
    }
    f.render_widget(Paragraph::new(head).wrap(Wrap { trim: true }), split[0]);

    // Activity / log tail, anchored to the bottom.
    let log_block = Block::default().borders(Borders::TOP).title(" activity ").border_style(Style::default().fg(SUBTEXT));
    let log_inner = log_block.inner(split[1]);
    f.render_widget(log_block, split[1]);
    let height = log_inner.height as usize;
    let offset = app.log_lines.len().saturating_sub(height);
    let body = if app.log_lines.is_empty() {
        Paragraph::new(Line::from(Span::styled("no activity yet (run with --agent to see the agent work)", Style::default().fg(SUBTEXT))))
    } else {
        Paragraph::new(app.log_lines.clone()).scroll((offset as u16, 0)).wrap(Wrap { trim: false })
    };
    f.render_widget(body, log_inner);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let path = app
        .rows
        .get(app.selected)
        .map(|r| format!("{} ", repo_label(&r.session.workspace)))
        .unwrap_or_default();
    let branch = app.rows.get(app.selected).map(|r| r.session.branch.clone()).unwrap_or_default();
    let spans = vec![
        Span::styled(format!(" {path}"), Style::default().fg(SUBTEXT)),
        Span::styled(format!("› {branch}   "), Style::default().fg(GREEN)),
        Span::styled("↑/↓", Style::default().fg(ACCENT)),
        Span::styled(" move  ", Style::default().fg(SUBTEXT)),
        Span::styled("enter/a", Style::default().fg(ACCENT)),
        Span::styled(" attach  ", Style::default().fg(SUBTEXT)),
        Span::styled("d", Style::default().fg(ACCENT)),
        Span::styled(" delete  ", Style::default().fg(SUBTEXT)),
        Span::styled("q", Style::default().fg(ACCENT)),
        Span::styled(" quit", Style::default().fg(SUBTEXT)),
    ];
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Status glyph, color, and label, with a spinner for active states.
fn status_display(status: SessionStatus, tick: u64) -> (String, Color, &'static str) {
    let spin = SPINNER[(tick as usize) % SPINNER.len()].to_string();
    match status {
        SessionStatus::Running => (spin, GREEN, "working"),
        SessionStatus::Seeding => (spin, YELLOW, "loading"),
        SessionStatus::Pending => ("◌".into(), SUBTEXT, "pending"),
        SessionStatus::Idle => ("◉".into(), TEAL, "idle"),
        SessionStatus::Suspended => ("⏸".into(), BLUE, "suspended"),
        SessionStatus::Done => ("✓".into(), GREEN, "done"),
        SessionStatus::Error => ("✗".into(), RED, "error"),
    }
}

/// A short workspace name from the repo URL (its last path segment).
fn repo_label(spec: &WorkspaceSpec) -> String {
    match spec {
        WorkspaceSpec::Git(g) => g
            .repo_url
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("repo")
            .trim_end_matches(".git")
            .to_string(),
        WorkspaceSpec::Archive(_) => "archive".to_string(),
    }
}

/// Human elapsed time since an RFC3339 timestamp (e.g. "13m 36s").
fn elapsed(ts: &str) -> String {
    let Ok(then) = chrono::DateTime::parse_from_rfc3339(ts) else { return String::new() };
    let secs = (chrono::Utc::now() - then.with_timezone(&chrono::Utc)).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use shepherd_core::ids::{SandboxId, SessionId};
    use shepherd_core::workspace::GitWorkspaceSpec;

    fn sample(status: SessionStatus, title: &str) -> Row {
        Row {
            session: Session {
                id: SessionId::new(),
                title: title.into(),
                status,
                provider_id: "docker".into(),
                sandbox_id: Some(SandboxId::new()),
                workspace: WorkspaceSpec::Git(GitWorkspaceSpec {
                    repo_url: "https://github.com/nimaibhat/shepherd.git".into(),
                    reference: Some("main".into()),
                    depth: None,
                    dirty_overlay: None,
                    mount_path: None,
                }),
                agent_session_id: None,
                branch: "agent/x".into(),
                created_at: "2026-06-30T00:00:00Z".into(),
                updated_at: "2026-06-30T00:00:00Z".into(),
                error: None,
            },
            live: Some(SandboxStatus::Running),
        }
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                s.push_str(buf[(x, y)].symbol());
            }
        }
        s
    }

    fn sample_repo(status: SessionStatus, title: &str, url: &str) -> Row {
        let mut r = sample(status, title);
        if let WorkspaceSpec::Git(g) = &mut r.session.workspace {
            g.repo_url = url.into();
        }
        r
    }

    #[test]
    fn renders_board() {
        let app = App {
            rows: vec![
                sample(SessionStatus::Running, "build the stream parser"),
                sample(SessionStatus::Seeding, "seed and clone repo"),
                sample(SessionStatus::Idle, "fix the flaky test"),
                sample_repo(SessionStatus::Done, "ship the release", "https://github.com/nimaibhat/llm-proxy.git"),
                sample_repo(SessionStatus::Error, "investigate crash", "https://github.com/nimaibhat/llm-proxy.git"),
            ],
            selected: 0,
            tick: 0,
            log_lines: vec![
                Line::raw("I will add the parser module and a test."),
                Line::from(Span::styled("  · Edit", Style::default().fg(TEAL))),
                Line::from(Span::styled("  · Bash", Style::default().fg(TEAL))),
                Line::raw("Done. Added stream_parser.rs with 6 tests."),
            ],
        };
        let backend = TestBackend::new(110, 26);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| ui(f, &app)).unwrap();
        let content = buffer_text(&term);
        if std::env::var("TUI_PREVIEW").is_ok() {
            println!("\n{content}");
        }
        assert!(content.contains("shepherd"), "title/workspace missing");
        assert!(content.contains("working"), "working status missing");
        assert!(content.contains("idle"), "idle status missing");
        assert!(content.contains("build the stream parser"), "session title missing");
    }

    #[test]
    fn repo_label_from_url() {
        let spec = WorkspaceSpec::Git(GitWorkspaceSpec {
            repo_url: "https://github.com/nimaibhat/shepherd.git".into(),
            reference: None,
            depth: None,
            dirty_overlay: None,
            mount_path: None,
        });
        assert_eq!(repo_label(&spec), "shepherd");
    }
}

fn setup_terminal() -> Result<Term> {
    enable_raw_mode()?;
    let mut out = std::io::stdout();
    execute!(out, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(out))?)
}

fn restore_terminal(term: &mut Term) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}
