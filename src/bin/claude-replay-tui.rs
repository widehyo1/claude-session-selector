use std::{
    env,
    io::{self, Read},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;

#[derive(Debug, Clone)]
enum ClaudeEvent {
    UserText(String),
    AssistantText(String),
    ToolUse { name: String, detail: String },
}

#[derive(Debug, Clone)]
enum EntryKind {
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    Timeline,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fullscreen {
    None,
    Timeline,
    Detail,
}

#[derive(Debug, Clone)]
struct Entry {
    index: usize,
    kind: EntryKind,
    title: String,
    detail: String,
}

struct App {
    entries: Vec<Entry>,
    list_state: ListState,
    detail_scroll: u16,
    focus: PaneFocus,
    fullscreen: Fullscreen,
    show_help: bool,
    status: Option<String>,
}

impl App {
    fn new(entries: Vec<Entry>) -> Self {
        let mut list_state = ListState::default();
        if !entries.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            entries,
            list_state,
            detail_scroll: 0,
            focus: PaneFocus::Timeline,
            fullscreen: Fullscreen::None,
            show_help: false,
            status: None,
        }
    }

    fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.selected_index().and_then(|i| self.entries.get(i))
    }

    fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }

        let next = match self.selected_index() {
            Some(i) if i + 1 < self.entries.len() => i + 1,
            _ => 0,
        };

        self.list_state.select(Some(next));
        self.detail_scroll = 0;
        self.status = None;
    }

    fn previous(&mut self) {
        if self.entries.is_empty() {
            return;
        }

        let previous = match self.selected_index() {
            Some(0) | None => self.entries.len() - 1,
            Some(i) => i - 1,
        };

        self.list_state.select(Some(previous));
        self.detail_scroll = 0;
        self.status = None;
    }

    fn detail_page_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(10);
        self.status = None;
    }

    fn detail_page_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(10);
        self.status = None;
    }

    fn detail_line_down(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_add(1);
        self.status = None;
    }

    fn detail_line_up(&mut self) {
        self.detail_scroll = self.detail_scroll.saturating_sub(1);
        self.status = None;
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            PaneFocus::Timeline => PaneFocus::Detail,
            PaneFocus::Detail => PaneFocus::Timeline,
        };
        self.status = None;
    }

    fn toggle_timeline_fullscreen(&mut self) {
        self.fullscreen = match self.fullscreen {
            Fullscreen::Timeline => Fullscreen::None,
            _ => Fullscreen::Timeline,
        };
        self.status = None;
    }

    fn toggle_detail_fullscreen(&mut self) {
        self.fullscreen = match self.fullscreen {
            Fullscreen::Detail => Fullscreen::None,
            _ => Fullscreen::Detail,
        };
        self.status = None;
    }

    fn exit_fullscreen(&mut self) {
        self.fullscreen = Fullscreen::None;
        self.status = None;
    }

    fn selected_detail_string(&self) -> Option<String> {
        let entry = self.selected_entry()?;

        Some(format!("{}\n\n{}", entry.title, entry.detail))
    }

    fn copy_detail_to_clipboard(&mut self) {
        let Some(text) = self.selected_detail_string() else {
            self.status = Some("nothing to copy".to_string());
            return;
        };

        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            Ok(()) => {
                self.status = Some("copied detail pane to clipboard".to_string());
            }
            Err(err) => {
                self.status = Some(format!("clipboard copy failed: {err}"));
            }
        }
    }
}

fn main() -> Result<()> {
    let input = read_input()?;
    let entries = load_entries_from_str(&input)?;

    let mut terminal = ratatui::init();
    let result = run_app(&mut terminal, App::new(entries));
    restore_terminal();

    result
}

fn restore_terminal() {
    ratatui::restore();
    let _ = execute!(io::stdout(), cursor::Show);
}

fn read_input() -> Result<String> {
    let arg = env::args_os().nth(1);

    match arg {
        Some(path) if path != "-" => {
            let path = PathBuf::from(path);
            std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))
        }
        Some(_) | None => {
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .context("failed to read stdin")?;

            if input.trim().is_empty() {
                anyhow::bail!(
                    "usage: claude-replay-tui <events.json|events.jsonl>\n       jq -c . a.json | claude-replay-tui"
                );
            }

            Ok(input)
        }
    }
}

fn run_app(terminal: &mut ratatui::DefaultTerminal, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Char('q') => break,

                    KeyCode::Esc => {
                        if app.fullscreen != Fullscreen::None {
                            app.exit_fullscreen();
                        } else if app.show_help {
                            app.show_help = false;
                        } else {
                            break;
                        }
                    }

                    KeyCode::Tab => app.toggle_focus(),

                    KeyCode::Char('1') => app.toggle_timeline_fullscreen(),
                    KeyCode::Char('2') => app.toggle_detail_fullscreen(),
                    KeyCode::Char('f') => match app.focus {
                        PaneFocus::Timeline => app.toggle_timeline_fullscreen(),
                        PaneFocus::Detail => app.toggle_detail_fullscreen(),
                    },

                    KeyCode::Char('y') => {
                        if app.focus == PaneFocus::Detail || app.fullscreen == Fullscreen::Detail {
                            app.copy_detail_to_clipboard();
                        }
                    }

                    KeyCode::Char('j') | KeyCode::Down => match app.focus {
                        PaneFocus::Timeline => app.next(),
                        PaneFocus::Detail => app.detail_line_down(),
                    },

                    KeyCode::Char('k') | KeyCode::Up => match app.focus {
                        PaneFocus::Timeline => app.previous(),
                        PaneFocus::Detail => app.detail_line_up(),
                    },

                    KeyCode::Char('d') | KeyCode::PageDown => match app.focus {
                        PaneFocus::Timeline => app.next(),
                        PaneFocus::Detail => app.detail_page_down(),
                    },

                    KeyCode::Char('u') | KeyCode::PageUp => match app.focus {
                        PaneFocus::Timeline => app.previous(),
                        PaneFocus::Detail => app.detail_page_up(),
                    },

                    KeyCode::Char('g') | KeyCode::Home => {
                        match app.focus {
                            PaneFocus::Timeline => {
                                app.list_state.select(Some(0));
                                app.detail_scroll = 0;
                            }
                            PaneFocus::Detail => {
                                app.detail_scroll = 0;
                            }
                        }
                        app.status = None;
                    }

                    KeyCode::Char('G') | KeyCode::End => {
                        match app.focus {
                            PaneFocus::Timeline => {
                                if !app.entries.is_empty() {
                                    app.list_state.select(Some(app.entries.len() - 1));
                                    app.detail_scroll = 0;
                                }
                            }
                            PaneFocus::Detail => {
                                app.detail_scroll = u16::MAX;
                            }
                        }
                        app.status = None;
                    }

                    KeyCode::Char('?') => app.show_help = !app.show_help,

                    _ => {}
                }
            }
        }
    }

    Ok(())
}

fn load_entries_from_str(input: &str) -> Result<Vec<Entry>> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let values = parse_json_values(trimmed)?;
    let mut events = Vec::new();

    for value in values {
        events.extend(normalize_claude_record(&value));
    }

    Ok(events
        .into_iter()
        .enumerate()
        .map(|(index, event)| to_entry(index, event))
        .collect())
}

fn parse_json_values(input: &str) -> Result<Vec<Value>> {
    if input.starts_with('[') {
        let values: Vec<Value> = serde_json::from_str(input).context("invalid json array input")?;
        return Ok(values);
    }

    let stream = serde_json::Deserializer::from_str(input).into_iter::<Value>();

    let mut values = Vec::new();

    for value in stream {
        values.push(value.context("invalid json/jsonl stream object")?);
    }

    Ok(values)
}

fn normalize_claude_record(value: &Value) -> Vec<ClaudeEvent> {
    let Some(message) = value.get("message") else {
        return Vec::new();
    };

    match message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "user" => normalize_user_message(message),
        "assistant" => normalize_assistant_message(message),
        _ => Vec::new(),
    }
}

fn normalize_user_message(message: &Value) -> Vec<ClaudeEvent> {
    let Some(text) = message.get("content").and_then(Value::as_str) else {
        return Vec::new();
    };

    if text.starts_with("<task-notification>") || text.trim().is_empty() {
        return Vec::new();
    }

    vec![ClaudeEvent::UserText(text.to_string())]
}

fn normalize_assistant_message(message: &Value) -> Vec<ClaudeEvent> {
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut events = Vec::new();

    for item in content {
        match item.get("type").and_then(Value::as_str).unwrap_or_default() {
            "text" => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        events.push(ClaudeEvent::AssistantText(text.to_string()));
                    }
                }
            }
            "thinking" => {}
            "tool_use" => {
                if let Some(event) = normalize_tool_use(item) {
                    events.push(event);
                }
            }
            _ => {}
        }
    }

    events
}

fn normalize_tool_use(item: &Value) -> Option<ClaudeEvent> {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();

    let input = item.get("input").unwrap_or(&Value::Null);
    let detail = match name.as_str() {
        "Bash" => format_bash(input),
        "Read" => format_read(input),
        "Write" => format_write(input),
        _ => None,
    }?;

    Some(ClaudeEvent::ToolUse { name, detail })
}

fn format_bash(input: &Value) -> Option<String> {
    let command = input.get("command").and_then(Value::as_str)?;
    let command = command.trim();

    if command.is_empty() {
        return None;
    }

    Some(format!("$ {command}"))
}

fn format_read(input: &Value) -> Option<String> {
    let file_path = input.get("file_path").and_then(Value::as_str)?;

    if file_path.trim().is_empty() {
        return None;
    }

    match input.get("limit").and_then(Value::as_i64) {
        Some(limit) => Some(format!("`{file_path}`, limit: {limit}")),
        None => Some(format!("`{file_path}`")),
    }
}

fn format_write(input: &Value) -> Option<String> {
    let file_path = input.get("file_path").and_then(Value::as_str)?;

    if file_path.trim().is_empty() {
        return None;
    }

    let content = input
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();

    Some(format!("`{file_path}`\n------\n{content}"))
}

fn to_entry(index: usize, event: ClaudeEvent) -> Entry {
    match event {
        ClaudeEvent::UserText(text) => Entry {
            index,
            kind: EntryKind::User,
            title: format!("#{index:04} USER {}", one_line_summary(&text, 100)),
            detail: text,
        },

        ClaudeEvent::AssistantText(text) => Entry {
            index,
            kind: EntryKind::Assistant,
            title: format!("#{index:04} ASSISTANT {}", one_line_summary(&text, 100)),
            detail: text,
        },

        ClaudeEvent::ToolUse { name, detail } => Entry {
            index,
            kind: EntryKind::Tool,
            title: format!("#{index:04} TOOL {name} {}", one_line_summary(&detail, 80)),
            detail,
        },
    }
}

fn one_line_summary(value: &str, max_chars: usize) -> String {
    truncate(&value.replace('\n', " "), max_chars)
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out = String::new();

    for _ in 0..max_chars {
        match chars.next() {
            Some(ch) => out.push(ch),
            None => return out,
        }
    }

    if chars.next().is_some() {
        out.push('…');
    }

    out
}

fn render(frame: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, root[0], app);

    match app.fullscreen {
        Fullscreen::None => {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(36), Constraint::Percentage(64)])
                .split(root[1]);

            render_list(frame, body[0], app);
            render_detail(frame, body[1], app);
        }
        Fullscreen::Timeline => {
            render_list(frame, root[1], app);
        }
        Fullscreen::Detail => {
            render_detail(frame, root[1], app);
        }
    }

    render_footer(frame, root[2], app);

    if app.show_help {
        render_help(frame);
    }
}

fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let selected = app
        .selected_index()
        .map(|i| format!("{}/{}", i + 1, app.entries.len()))
        .unwrap_or_else(|| "0/0".to_string());

    let header = Line::from(vec![
        Span::styled(
            " Claude Replay ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(selected, Style::default().fg(Color::Cyan)),
    ]);

    frame.render_widget(header, area);
}

fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let items: Vec<ListItem> = app
        .entries
        .iter()
        .map(|entry| {
            let style = match entry.kind {
                EntryKind::User => Style::default().fg(Color::Green),
                EntryKind::Assistant => Style::default().fg(Color::Blue),
                EntryKind::Tool => Style::default().fg(Color::Yellow),
            };

            ListItem::new(Line::from(vec![
                Span::styled(kind_label(&entry.kind), style.add_modifier(Modifier::BOLD)),
                Span::raw(" "),
                Span::raw(truncate(&entry.title, 120)),
            ]))
        })
        .collect();

    let focused = app.focus == PaneFocus::Timeline || app.fullscreen == Fullscreen::Timeline;

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let title = if focused {
        " Timeline [focus] "
    } else {
        " Timeline "
    };

    let list = List::new(items)
        .block(
            Block::new()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_detail(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let Some(entry) = app.selected_entry() else {
        let empty = Paragraph::new("No entries")
            .block(Block::new().title(" Detail ").borders(Borders::ALL));
        frame.render_widget(empty, area);
        return;
    };

    let focused = app.focus == PaneFocus::Detail || app.fullscreen == Fullscreen::Detail;

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let focus_suffix = if focused { " [focus]" } else { "" };
    let fullscreen_suffix = if app.fullscreen == Fullscreen::Detail {
        " [fullscreen]"
    } else {
        ""
    };

    let title = format!(" Detail{focus_suffix}{fullscreen_suffix}: {} ", entry.title);
    let text = detail_text(entry);

    let paragraph = Paragraph::new(text)
        .block(
            Block::new()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn detail_text(entry: &Entry) -> Text<'static> {
    let mut lines = Vec::new();

    lines.push(Line::from(vec![
        Span::styled("index: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(entry.index.to_string()),
    ]));

    lines.push(Line::from(vec![
        Span::styled("kind:  ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(kind_label(&entry.kind)),
    ]));

    lines.push(Line::raw(""));

    for line in entry.detail.lines() {
        lines.push(Line::raw(line.to_string()));
    }

    Text::from(lines)
}

fn kind_label(kind: &EntryKind) -> &'static str {
    match kind {
        EntryKind::User => "USER     ",
        EntryKind::Assistant => "ASSISTANT",
        EntryKind::Tool => "TOOL     ",
    }
}

fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let focus = match app.focus {
        PaneFocus::Timeline => "timeline",
        PaneFocus::Detail => "detail",
    };

    let fullscreen = match app.fullscreen {
        Fullscreen::None => "",
        Fullscreen::Timeline => " | fullscreen: timeline",
        Fullscreen::Detail => " | fullscreen: detail",
    };

    let status = app
        .status
        .as_deref()
        .map(|s| format!(" | {s}"))
        .unwrap_or_default();

    let footer = Line::from(vec![
        Span::raw(format!(" focus: {focus}{fullscreen}{status} ")),
        Span::raw(" | Tab focus "),
        Span::raw(" 1/2 fullscreen "),
        Span::raw(" f fullscreen-focus "),
        Span::raw(" y copy-detail "),
        Span::raw(" q quit "),
        Span::raw(" ? help "),
    ])
    .gray();

    frame.render_widget(footer, area);
}

fn render_help(frame: &mut Frame) {
    let area = centered_rect(76, 82, frame.area());

    let help = Paragraph::new(vec![
        Line::styled(
            "Claude Replay TUI",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw("Focus"),
        Line::raw("  Tab             switch focus between timeline/detail"),
        Line::raw(""),
        Line::raw("Timeline focus"),
        Line::raw("  j / Down        next event"),
        Line::raw("  k / Up          previous event"),
        Line::raw("  d / PageDown    next event"),
        Line::raw("  u / PageUp      previous event"),
        Line::raw("  g / Home        first event"),
        Line::raw("  G / End         last event"),
        Line::raw(""),
        Line::raw("Detail focus"),
        Line::raw("  j / Down        scroll detail down one line"),
        Line::raw("  k / Up          scroll detail up one line"),
        Line::raw("  d / PageDown    scroll detail down one page"),
        Line::raw("  u / PageUp      scroll detail up one page"),
        Line::raw("  g / Home        scroll detail to top"),
        Line::raw("  G / End         scroll detail to bottom"),
        Line::raw("  y               copy selected detail to clipboard"),
        Line::raw(""),
        Line::raw("Fullscreen"),
        Line::raw("  1               toggle timeline fullscreen"),
        Line::raw("  2               toggle detail fullscreen"),
        Line::raw("  f               toggle focused pane fullscreen"),
        Line::raw("  Esc             leave fullscreen/help or quit"),
        Line::raw(""),
        Line::raw("Other"),
        Line::raw("  ?               toggle help"),
        Line::raw("  q / Ctrl-C      quit"),
    ])
    .block(Block::new().title(" Help ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });

    frame.render_widget(Clear, area);
    frame.render_widget(help, area);
}

fn centered_rect(
    percent_x: u16,
    percent_y: u16,
    area: ratatui::layout::Rect,
) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_raw_claude_jsonl_events() {
        let input = r#"{"type":"file-history-snapshot","snapshot":{}}
{"type":"user","message":{"role":"user","content":"hello"}}
{"type":"user","message":{"role":"user","content":"<task-notification>skip this"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"skip"},{"type":"text","text":"world"},{"type":"tool_use","name":"Bash","input":{"command":"ls -la"}},{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/a.txt","limit":5}},{"type":"tool_use","name":"Write","input":{"file_path":"/tmp/b.txt","content":"done"}},{"type":"tool_use","name":"TaskCreate","input":{"todos":[]}}]}}"#;

        let entries = load_entries_from_str(input).expect("jsonl should parse");

        assert_eq!(entries.len(), 5);
        assert!(matches!(entries[0].kind, EntryKind::User));
        assert_eq!(entries[0].title, "#0000 USER hello");
        assert_eq!(entries[0].detail, "hello");
        assert!(matches!(entries[1].kind, EntryKind::Assistant));
        assert_eq!(entries[1].detail, "world");
        assert!(matches!(entries[2].kind, EntryKind::Tool));
        assert_eq!(entries[2].detail, "$ ls -la");
        assert_eq!(entries[3].detail, "`/tmp/a.txt`, limit: 5");
        assert_eq!(entries[4].detail, "`/tmp/b.txt`\n------\ndone");
    }

    #[test]
    fn loads_claude_json_array_records() {
        let input = r#"[{"type":"user","message":{"role":"user","content":"array input"}}]"#;

        let entries = load_entries_from_str(input).expect("json array should parse");

        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].kind, EntryKind::User));
        assert_eq!(entries[0].detail, "array input");
    }
}
