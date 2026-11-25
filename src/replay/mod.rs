use std::{collections::HashMap, io::Read, path::Path, time::Duration};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use serde_json::Value;

use crate::{
    cli::ReplayOptions,
    session_event::{self, MessageRole},
    terminal,
    ui::{bottom_scroll_offset, fit_width, half_page_height, pane_content_area},
    ui_state::ExecVisibility,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    User,
    Agent,
    Exec,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Search,
}

const DETAIL_HEADER_LINES: usize = 3;
const MATCH_SCROLL_MARGIN: usize = 2;
const TIMELINE_SUMMARY_CHARS: usize = 113;
// Highlight symbol, kind label, and " #0000 " come before the summary.
const TIMELINE_PREFIX_WIDTH: usize = 2 + 5 + 7;
const TIMELINE_TIME_WIDTH: usize = "YYYY-mm-dd HH:mm".len();

#[derive(Debug, Clone)]
struct Entry {
    kind: EntryKind,
    summary: String,
    detail: String,
    // The record timestamp as `YYYY-mm-dd HH:mm`, as written in the transcript (UTC).
    time: Option<String>,
}

struct ReplayApp {
    all_entries: Vec<Entry>,
    visible_indices: Vec<usize>,
    // Position of each visible entry in the exec-filtered list, so numbers survive a search.
    display_numbers: Vec<usize>,
    list_state: ListState,
    detail_scroll: u16,
    timeline_page_height: u16,
    detail_page_height: u16,
    detail_width: u16,
    detail_max_scroll: u16,
    mode: Mode,
    query: String,
    // Selection to fall back to when a search leaves nothing selected.
    search_anchor: Option<usize>,
    // (all_index, match index) of the match `n`/`N` last moved to.
    detail_match: Option<(usize, usize)>,
    focus: PaneFocus,
    fullscreen: Fullscreen,
    show_help: bool,
    exec_visibility: ExecVisibility,
    status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayControl {
    Continue,
    Quit,
}

impl ReplayApp {
    fn new(all_entries: Vec<Entry>, exec_visibility: ExecVisibility) -> Self {
        let (visible_indices, display_numbers) = filter_entries(&all_entries, exec_visibility, &[]);
        let mut list_state = ListState::default();
        if !visible_indices.is_empty() {
            list_state.select(Some(0));
        }

        Self {
            all_entries,
            visible_indices,
            display_numbers,
            list_state,
            detail_scroll: 0,
            timeline_page_height: 1,
            detail_page_height: 1,
            detail_width: 1,
            detail_max_scroll: 0,
            mode: Mode::Normal,
            query: String::new(),
            search_anchor: None,
            detail_match: None,
            focus: PaneFocus::Timeline,
            fullscreen: Fullscreen::None,
            show_help: false,
            exec_visibility,
            status: None,
        }
    }

    fn selected_visible_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn selected_all_index(&self) -> Option<usize> {
        self.selected_visible_index()
            .and_then(|index| self.visible_indices.get(index))
            .copied()
    }

    fn selected_entry(&self) -> Option<(usize, &Entry)> {
        let visible_index = self.selected_visible_index()?;
        let all_index = *self.visible_indices.get(visible_index)?;
        let number = *self.display_numbers.get(visible_index)?;
        self.all_entries.get(all_index).map(|entry| (number, entry))
    }

    fn rebuild_visible_indices(&mut self, previous_all_index: Option<usize>) {
        (self.visible_indices, self.display_numbers) = filter_entries(
            &self.all_entries,
            self.exec_visibility,
            &query_terms(&self.query),
        );

        if self.visible_indices.is_empty() {
            self.list_state.select(None);
            return;
        }

        let selected_visible_index = previous_all_index
            .and_then(|all_index| {
                self.visible_indices
                    .iter()
                    .position(|candidate| *candidate == all_index)
            })
            .or_else(|| {
                previous_all_index.and_then(|all_index| {
                    self.visible_indices
                        .iter()
                        .position(|candidate| *candidate > all_index)
                })
            })
            .or_else(|| previous_all_index.map(|_| self.visible_indices.len() - 1))
            .unwrap_or(0);

        self.list_state.select(Some(selected_visible_index));
    }

    fn toggle_exec_visibility(&mut self) {
        let previous_all_index = self.selected_all_index().or(self.search_anchor);
        self.exec_visibility.toggle();
        self.rebuild_visible_indices(previous_all_index);
        self.detail_scroll = 0;
        self.detail_match = None;
        self.status = None;
    }

    fn start_search(&mut self) {
        self.search_anchor = self.selected_all_index().or(self.search_anchor);
        self.mode = Mode::Search;
        self.status = None;
    }

    // Keeps the selected entry when it still matches; the detail scroll follows the entry.
    fn refresh_filter(&mut self) {
        let before = self.selected_all_index();
        self.rebuild_visible_indices(before.or(self.search_anchor));
        if self.selected_all_index() != before {
            self.detail_scroll = 0;
        }
        self.detail_match = None;
        self.status = None;
    }

    fn clear_filter(&mut self) {
        self.query.clear();
        self.mode = Mode::Normal;
        self.refresh_filter();
    }

    fn jump_to_match(&mut self, forward: bool) {
        let terms = query_terms(&self.query);
        if terms.is_empty() {
            self.status = Some("no active search".to_string());
            return;
        }
        let Some(all_index) = self.selected_all_index() else {
            return;
        };
        let Some((number, entry)) = self.selected_entry() else {
            return;
        };

        let match_lines = detail_match_lines(entry, &terms);
        let count = match_lines.len();
        if count == 0 {
            self.status = Some("no match in this entry".to_string());
            return;
        }

        let current = self
            .detail_match
            .filter(|(entry_index, _)| *entry_index == all_index)
            .map(|(_, match_index)| match_index);
        let target = match (current, forward) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            (None, true) => 0,
            (None, false) => count - 1,
        };
        let row = detail_row_of_line(entry, number, match_lines[target], self.detail_width);

        self.detail_scroll = row
            .saturating_sub(MATCH_SCROLL_MARGIN)
            .min(usize::from(self.detail_max_scroll)) as u16;
        self.detail_match = Some((all_index, target));
        self.status = Some(format!("match {}/{count}", target + 1));
    }

    fn next(&mut self) {
        if self.visible_indices.is_empty() {
            return;
        }

        let next = match self.selected_visible_index() {
            Some(i) if i + 1 < self.visible_indices.len() => i + 1,
            _ => 0,
        };

        self.list_state.select(Some(next));
        self.detail_scroll = 0;
        self.status = None;
    }

    fn previous(&mut self) {
        if self.visible_indices.is_empty() {
            return;
        }

        let previous = match self.selected_visible_index() {
            Some(0) | None => self.visible_indices.len() - 1,
            Some(i) => i - 1,
        };

        self.list_state.select(Some(previous));
        self.detail_scroll = 0;
        self.status = None;
    }

    fn detail_page_down(&mut self) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_add(half_page_height(self.detail_page_height))
            .min(self.detail_max_scroll);
        self.status = None;
    }

    fn detail_page_up(&mut self) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_sub(half_page_height(self.detail_page_height));
        self.status = None;
    }

    fn detail_line_down(&mut self) {
        self.detail_scroll = self
            .detail_scroll
            .saturating_add(1)
            .min(self.detail_max_scroll);
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
        if self.fullscreen == Fullscreen::Timeline {
            self.fullscreen = Fullscreen::None;
        } else {
            self.focus = PaneFocus::Timeline;
            self.fullscreen = Fullscreen::Timeline;
        }
        self.status = None;
    }

    fn toggle_detail_fullscreen(&mut self) {
        if self.fullscreen == Fullscreen::Detail {
            self.fullscreen = Fullscreen::None;
        } else {
            self.focus = PaneFocus::Detail;
            self.fullscreen = Fullscreen::Detail;
        }
        self.status = None;
    }

    fn exit_fullscreen(&mut self) {
        self.fullscreen = Fullscreen::None;
        self.status = None;
    }

    fn selected_detail_string(&self) -> Option<String> {
        let (visible_index, entry) = self.selected_entry()?;

        Some(format!(
            "{}\n\n{}",
            display_title(visible_index, entry),
            entry.detail
        ))
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

    fn handle_key(&mut self, key: KeyEvent) -> ReplayControl {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return ReplayControl::Quit;
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => self.show_help = false,
                KeyCode::Char('q') => return ReplayControl::Quit,
                _ => {}
            }
            return ReplayControl::Continue;
        }

        if self.mode == Mode::Search {
            match key.code {
                KeyCode::Esc => self.clear_filter(),
                KeyCode::Enter => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    self.query.pop();
                    self.refresh_filter();
                }
                KeyCode::Char(ch) => {
                    self.query.push(ch);
                    self.refresh_filter();
                }
                _ => {}
            }
            return ReplayControl::Continue;
        }

        match key.code {
            KeyCode::Char('q') => ReplayControl::Quit,
            KeyCode::Esc => {
                if self.fullscreen != Fullscreen::None {
                    self.exit_fullscreen();
                    ReplayControl::Continue
                } else if !self.query.is_empty() {
                    self.clear_filter();
                    ReplayControl::Continue
                } else {
                    ReplayControl::Quit
                }
            }
            KeyCode::Char('/') => {
                self.start_search();
                ReplayControl::Continue
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                if self.focus == PaneFocus::Detail {
                    self.jump_to_match(key.code == KeyCode::Char('n'));
                }
                ReplayControl::Continue
            }
            KeyCode::Char('e') if key.modifiers.is_empty() => {
                self.toggle_exec_visibility();
                ReplayControl::Continue
            }
            KeyCode::Tab => {
                self.toggle_focus();
                ReplayControl::Continue
            }
            KeyCode::Char('1') => {
                self.toggle_timeline_fullscreen();
                ReplayControl::Continue
            }
            KeyCode::Char('2') => {
                self.toggle_detail_fullscreen();
                ReplayControl::Continue
            }
            KeyCode::Char('f') => {
                match self.focus {
                    PaneFocus::Timeline => self.toggle_timeline_fullscreen(),
                    PaneFocus::Detail => self.toggle_detail_fullscreen(),
                }
                ReplayControl::Continue
            }
            KeyCode::Char('y') => {
                if self.focus == PaneFocus::Detail || self.fullscreen == Fullscreen::Detail {
                    self.copy_detail_to_clipboard();
                }
                ReplayControl::Continue
            }
            KeyCode::Char('j') | KeyCode::Down => {
                match self.focus {
                    PaneFocus::Timeline => self.next(),
                    PaneFocus::Detail => self.detail_line_down(),
                }
                ReplayControl::Continue
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.focus {
                    PaneFocus::Timeline => self.previous(),
                    PaneFocus::Detail => self.detail_line_up(),
                }
                ReplayControl::Continue
            }
            KeyCode::Char('d') | KeyCode::PageDown => {
                match self.focus {
                    PaneFocus::Timeline => {
                        for _ in 0..half_page_height(self.timeline_page_height) {
                            self.next();
                        }
                    }
                    PaneFocus::Detail => self.detail_page_down(),
                }
                ReplayControl::Continue
            }
            KeyCode::Char('u') | KeyCode::PageUp => {
                match self.focus {
                    PaneFocus::Timeline => {
                        for _ in 0..half_page_height(self.timeline_page_height) {
                            self.previous();
                        }
                    }
                    PaneFocus::Detail => self.detail_page_up(),
                }
                ReplayControl::Continue
            }
            KeyCode::Char('g') | KeyCode::Home => {
                match self.focus {
                    PaneFocus::Timeline => {
                        if !self.visible_indices.is_empty() {
                            self.list_state.select(Some(0));
                            self.detail_scroll = 0;
                        }
                    }
                    PaneFocus::Detail => {
                        self.detail_scroll = 0;
                    }
                }
                self.status = None;
                ReplayControl::Continue
            }
            KeyCode::Char('G') | KeyCode::End => {
                match self.focus {
                    PaneFocus::Timeline => {
                        if !self.visible_indices.is_empty() {
                            self.list_state.select(Some(self.visible_indices.len() - 1));
                            self.detail_scroll = 0;
                        }
                    }
                    PaneFocus::Detail => {
                        self.detail_scroll = self.detail_max_scroll;
                    }
                }
                self.status = None;
                ReplayControl::Continue
            }
            KeyCode::Char('?') => {
                self.show_help = true;
                ReplayControl::Continue
            }
            _ => ReplayControl::Continue,
        }
    }
}

pub(crate) fn run(options: &ReplayOptions) -> Result<ExecVisibility> {
    let input = read_input(options.input.as_deref())?;
    let entries = load_entries_from_str(&input)?;
    let exec_visibility = ExecVisibility::from_include_exec(options.include_exec);

    terminal::with_terminal(|terminal| {
        run_event_loop(terminal, ReplayApp::new(entries, exec_visibility))
    })
}

fn read_input(input: Option<&Path>) -> Result<String> {
    match input {
        Some(path) if path != Path::new("-") => std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display())),
        Some(_) | None => {
            let mut stdin = std::io::stdin().lock();
            let input = read_input_from_reader(&mut stdin)?;

            if input.trim().is_empty() {
                anyhow::bail!(
                    "usage: select-claude-session replay <events.json|events.jsonl>\n       jq -c . a.json | select-claude-session replay"
                );
            }

            Ok(input)
        }
    }
}

fn read_input_from_reader(reader: &mut impl Read) -> Result<String> {
    let mut input = String::new();
    reader
        .read_to_string(&mut input)
        .context("failed to read stdin")?;
    Ok(input)
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: ReplayApp,
) -> Result<ExecVisibility> {
    loop {
        terminal.draw(|frame| render(frame, &mut app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if app.handle_key(key) == ReplayControl::Quit {
                return Ok(app.exec_visibility);
            }
        }
    }
}

fn load_entries_from_str(input: &str) -> Result<Vec<Entry>> {
    let trimmed = input.trim();

    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    Ok(load_entries(parse_json_values(trimmed)?))
}

// Messages come before the Bash calls of the same record; results attach to their call.
fn load_entries(values: Vec<Value>) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut exec_by_call_id: HashMap<String, usize> = HashMap::new();

    for value in values {
        let time = record_time(&value);
        if let Some(message) = session_event::normalize_message_record(&value) {
            entries.push(Entry {
                time: time.clone(),
                ..message_entry(message.role, message.content)
            });
        }
        for call in session_event::bash_calls(&value) {
            exec_by_call_id.insert(call.call_id.clone(), entries.len());
            entries.push(Entry {
                time: time.clone(),
                ..bash_entry(&call.call_id, &call.command)
            });
        }
        for output in session_event::tool_outputs(&value) {
            if let Some(&index) = exec_by_call_id.get(&output.call_id) {
                append_exec_output(&mut entries[index], &output.output);
            }
        }
    }

    entries
}

// `2026-11-01T09:05:30.000Z` -> `2026-11-01 09:05`; anything else is not shown.
fn record_time(value: &Value) -> Option<String> {
    let timestamp = value.get("timestamp")?.as_str()?;
    let (date, time) = timestamp.get(..16)?.split_once('T')?;
    (date.len() == 10 && time.len() == 5).then(|| format!("{date} {time}"))
}

fn visible_entry_indices(entries: &[Entry], visibility: ExecVisibility) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(all_index, entry)| {
            let visible = visibility.is_shown() || !matches!(entry.kind, EntryKind::Exec);
            visible.then_some(all_index)
        })
        .collect()
}

fn query_terms(query: &str) -> Vec<String> {
    query.split_whitespace().map(str::to_lowercase).collect()
}

fn entry_matches(entry: &Entry, terms: &[String]) -> bool {
    if terms.is_empty() {
        return true;
    }
    let summary = entry.summary.to_lowercase();
    let detail = entry.detail.to_lowercase();
    terms
        .iter()
        .all(|term| summary.contains(term.as_str()) || detail.contains(term.as_str()))
}

// Returns the visible entries and, for each, its number in the exec-filtered list.
fn filter_entries(
    entries: &[Entry],
    visibility: ExecVisibility,
    terms: &[String],
) -> (Vec<usize>, Vec<usize>) {
    visible_entry_indices(entries, visibility)
        .into_iter()
        .enumerate()
        .filter(|(_, all_index)| entry_matches(&entries[*all_index], terms))
        .map(|(number, all_index)| (all_index, number))
        .unzip()
}

// Byte ranges of the terms in `text`. Only ASCII case is folded so offsets stay valid.
fn match_ranges(text: &str, terms: &[String]) -> Vec<(usize, usize)> {
    if terms.is_empty() {
        return Vec::new();
    }

    let haystack = text.to_ascii_lowercase();
    let mut ranges: Vec<(usize, usize)> = terms
        .iter()
        .flat_map(|term| {
            haystack
                .match_indices(term.as_str())
                .map(|(start, matched)| (start, start + matched.len()))
        })
        .collect();
    ranges.sort_unstable();

    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn highlight_spans(
    text: &str,
    ranges: &[(usize, usize)],
    style_of: impl Fn(usize) -> Style,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut cursor = 0;

    for (index, &(start, end)) in ranges.iter().enumerate() {
        if start > cursor {
            spans.push(Span::raw(text[cursor..start].to_string()));
        }
        spans.push(Span::styled(text[start..end].to_string(), style_of(index)));
        cursor = end;
    }
    if cursor < text.len() || spans.is_empty() {
        spans.push(Span::raw(text[cursor..].to_string()));
    }

    spans
}

// Detail line index of every match, in the order `detail_text` numbers them.
fn detail_match_lines(entry: &Entry, terms: &[String]) -> Vec<usize> {
    entry
        .detail
        .lines()
        .enumerate()
        .flat_map(|(line_index, line)| {
            std::iter::repeat_n(line_index, match_ranges(line, terms).len())
        })
        .collect()
}

// Wrapped rows above the given detail line.
fn detail_row_of_line(entry: &Entry, number: usize, line_index: usize, width: u16) -> usize {
    let mut text = detail_text(entry, number, &[], None);
    text.lines.truncate(DETAIL_HEADER_LINES + line_index);
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
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

fn message_entry(role: MessageRole, content: String) -> Entry {
    let (kind, summary) = match role {
        MessageRole::User => (EntryKind::User, "USER"),
        MessageRole::Agent => (EntryKind::Agent, "AGENT"),
    };
    Entry {
        kind,
        summary: summary.to_string(),
        detail: content,
        time: None,
    }
}

fn bash_entry(call_id: &str, command: &str) -> Entry {
    let command_summary = truncate(&command.replace('\n', " "), 80);
    let mut detail = String::new();

    detail.push_str("COMMANDS\n");
    detail.push_str("--------\n");
    detail.push_str(&format!(
        "type: tool_use | name: Bash | call_id: {call_id}\n"
    ));
    detail.push_str("$ ");
    detail.push_str(command);
    detail.push('\n');
    detail.push('\n');
    detail.push_str("OUTPUT\n");
    detail.push_str("------\n");

    Entry {
        kind: EntryKind::Exec,
        summary: format!("EXEC {command_summary}"),
        detail,
        time: None,
    }
}

fn display_title(visible_index: usize, entry: &Entry) -> String {
    format!("#{visible_index:04} {}", entry.summary)
}

fn append_exec_output(entry: &mut Entry, output: &str) {
    entry.detail.push_str(output);
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

fn render(frame: &mut Frame, app: &mut ReplayApp) {
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

fn render_header(frame: &mut Frame, area: ratatui::layout::Rect, app: &ReplayApp) {
    let selected = app
        .selected_visible_index()
        .map(|i| format!("{}/{}", i + 1, app.visible_indices.len()))
        .unwrap_or_else(|| "0/0".to_string());
    let query = if app.query.is_empty() {
        String::new()
    } else {
        format!(" | /{}", app.query)
    };

    let header = Line::from(vec![
        Span::styled(
            " Claude Replay ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(selected, Style::default().fg(Color::Cyan)),
        Span::raw(format!(
            " of {}{query} | exec: {}",
            app.all_entries.len(),
            app.exec_visibility.label()
        )),
    ]);

    frame.render_widget(header, area);
}

fn render_list(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut ReplayApp) {
    let content_width = usize::from(pane_content_area(area).width);
    app.timeline_page_height = pane_content_area(area).height.max(1);
    // The time column stays at the end of the row; the summary gives way on narrow panes.
    let summary_width = content_width
        .saturating_sub(TIMELINE_PREFIX_WIDTH + 1 + TIMELINE_TIME_WIDTH)
        .min(TIMELINE_SUMMARY_CHARS + 1);

    let terms = query_terms(&app.query);
    let match_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::UNDERLINED);

    let items: Vec<ListItem> = app
        .visible_indices
        .iter()
        .zip(&app.display_numbers)
        .filter_map(|(all_index, number)| {
            let entry = app.all_entries.get(*all_index)?;
            let style = match entry.kind {
                EntryKind::User => Style::default().fg(Color::Green),
                EntryKind::Agent => Style::default().fg(Color::Blue),
                EntryKind::Exec => Style::default().fg(Color::Yellow),
            };

            let summary = fit_width(
                &truncate(&entry.summary, TIMELINE_SUMMARY_CHARS),
                summary_width,
            );
            let mut spans = vec![
                Span::styled(kind_label(&entry.kind), style.add_modifier(Modifier::BOLD)),
                Span::raw(format!(" #{number:04} ")),
            ];
            spans.extend(highlight_spans(
                &summary,
                &match_ranges(&summary, &terms),
                |_| match_style,
            ));
            spans.push(Span::styled(
                format!(" {}", entry.time.as_deref().unwrap_or("-")),
                Style::default().fg(Color::DarkGray),
            ));

            Some(ListItem::new(Line::from(spans)))
        })
        .collect();

    let focused = app.focus == PaneFocus::Timeline || app.fullscreen == Fullscreen::Timeline;

    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let title = format!(
        " Timeline{}{} ",
        if focused { " [focus]" } else { "" },
        if app.mode == Mode::Search {
            " [search]"
        } else {
            ""
        }
    );

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

fn render_detail(frame: &mut Frame, area: ratatui::layout::Rect, app: &mut ReplayApp) {
    let content_area = pane_content_area(area);
    app.detail_page_height = content_area.height.max(1);

    app.detail_width = content_area.width.max(1);

    let Some((number, entry)) = app
        .selected_entry()
        .map(|(number, entry)| (number, entry.clone()))
    else {
        app.detail_max_scroll = 0;
        app.detail_scroll = 0;
        let message = if app.query.is_empty() {
            "No entries"
        } else {
            "No matches"
        };
        let empty =
            Paragraph::new(message).block(Block::new().title(" Detail ").borders(Borders::ALL));
        frame.render_widget(empty, area);
        return;
    };

    let selected_all_index = app.selected_all_index();
    let current_match = app
        .detail_match
        .filter(|(entry_index, _)| Some(*entry_index) == selected_all_index)
        .map(|(_, match_index)| match_index);
    let text = detail_text(&entry, number, &query_terms(&app.query), current_match);

    app.detail_max_scroll = bottom_scroll_offset(
        Paragraph::new(text.clone())
            .wrap(Wrap { trim: false })
            .line_count(content_area.width),
        content_area.height,
    );
    app.detail_scroll = app.detail_scroll.min(app.detail_max_scroll);

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

    let title = format!(
        " Detail{focus_suffix}{fullscreen_suffix}: {} ",
        display_title(number, &entry)
    );

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

// `current` is the match index, counted across the whole detail, that `n`/`N` points at.
fn detail_text(
    entry: &Entry,
    visible_index: usize,
    terms: &[String],
    current: Option<usize>,
) -> Text<'static> {
    let match_style = Style::default().fg(Color::Black).bg(Color::Yellow);
    let current_style = Style::default()
        .fg(Color::Black)
        .bg(Color::LightMagenta)
        .add_modifier(Modifier::BOLD);
    let mut lines = Vec::new();
    let mut match_count = 0;

    lines.push(Line::from(vec![
        Span::styled("index: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(visible_index.to_string()),
    ]));

    lines.push(Line::from(vec![
        Span::styled("kind:  ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(kind_label(&entry.kind)),
    ]));

    lines.push(Line::raw(""));

    for line in entry.detail.lines() {
        let ranges = match_ranges(line, terms);
        lines.push(Line::from(highlight_spans(line, &ranges, |index| {
            if current == Some(match_count + index) {
                current_style
            } else {
                match_style
            }
        })));
        match_count += ranges.len();
    }

    Text::from(lines)
}

fn kind_label(kind: &EntryKind) -> &'static str {
    match kind {
        EntryKind::User => "USER ",
        EntryKind::Agent => "AGENT",
        EntryKind::Exec => "EXEC ",
    }
}

fn render_footer(frame: &mut Frame, area: ratatui::layout::Rect, app: &ReplayApp) {
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

    if app.mode == Mode::Search {
        let footer = Line::from(vec![
            Span::raw(" search: "),
            Span::styled(app.query.clone(), Style::default().fg(Color::Yellow)),
            Span::raw("  | Enter keep filter  Esc clear filter "),
        ])
        .gray();
        frame.render_widget(footer, area);
        return;
    }

    let footer = Line::from(vec![
        Span::raw(format!(" focus: {focus}{fullscreen}{status} ")),
        Span::raw(" | / search "),
        Span::raw(" Tab focus "),
        Span::raw(" 1/2 fullscreen "),
        Span::raw(" f fullscreen-focus "),
        Span::raw(" e exec "),
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
        Line::raw("  d / PageDown    move down half the timeline height"),
        Line::raw("  u / PageUp      move up half the timeline height"),
        Line::raw("  g / Home        first event"),
        Line::raw("  G / End         last event"),
        Line::raw(""),
        Line::raw("Detail focus"),
        Line::raw("  j / Down        scroll detail down one line"),
        Line::raw("  k / Up          scroll detail up one line"),
        Line::raw("  d / PageDown    scroll detail down half a page"),
        Line::raw("  u / PageUp      scroll detail up half a page"),
        Line::raw("  g / Home        scroll detail to top"),
        Line::raw("  G / End         scroll detail to bottom with the last line visible"),
        Line::raw("  y               copy selected detail to clipboard"),
        Line::raw("  n / N           scroll to the next/previous search match"),
        Line::raw(""),
        Line::raw("Search"),
        Line::raw("  /               filter the timeline to entries matching every word"),
        Line::raw("  Enter           keep the filter and leave search input"),
        Line::raw("  Esc             clear the filter, keeping the selected entry"),
        Line::raw(""),
        Line::raw("Fullscreen"),
        Line::raw("  1               toggle timeline fullscreen"),
        Line::raw("  2               toggle detail fullscreen"),
        Line::raw("  f               toggle focused pane fullscreen"),
        Line::raw("  Esc             leave fullscreen/help, clear the filter, or quit"),
        Line::raw(""),
        Line::raw("Other"),
        Line::raw("  e               toggle command execution entries"),
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
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render_at(app: &mut ReplayApp, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
    }

    const MAIN: &str = include_str!("../../tests/fixtures/main-session.jsonl");
    const SUB: &str = include_str!("../../tests/fixtures/subagent/agent-a1.jsonl");

    fn common_fixture() -> &'static str {
        r#"{"type":"user","origin":{"kind":"human"},"promptSource":"typed","message":{"role":"user","content":"hello"}}
{"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":"call-0","name":"Bash","input":{"command":"pwd"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-0","content":"/tmp"}]}}
{"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"text","text":"done"}]}}
{"type":"assistant","message":{"role":"assistant","model":"m","content":[{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"git status"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"clean"}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"unmatched","content":"not exec"}]}}"#
    }

    fn kinds(entries: &[Entry]) -> Vec<EntryKind> {
        entries.iter().map(|entry| entry.kind).collect()
    }

    #[test]
    fn loads_user_agent_and_bash_entries() {
        let entries = load_entries_from_str(MAIN).expect("fixture should parse");

        assert_eq!(
            kinds(&entries),
            vec![
                EntryKind::User,
                EntryKind::User,
                EntryKind::Agent,
                EntryKind::Exec
            ]
        );
        assert_eq!(entries[0].summary, "USER");
        assert_eq!(entries[1].detail, "fix the parser");
        assert_eq!(entries[2].summary, "AGENT");
        assert_eq!(entries[2].detail, "checking status");
        assert_eq!(entries[3].summary, "EXEC git status");

        let entries = load_entries_from_str(common_fixture()).expect("fixture should parse");
        assert_eq!(
            kinds(&entries),
            vec![
                EntryKind::User,
                EntryKind::Exec,
                EntryKind::Agent,
                EntryKind::Exec
            ]
        );
        assert!(
            entries
                .iter()
                .all(|entry| !entry.detail.contains("not exec"))
        );
    }

    #[test]
    fn bash_output_is_attached_to_its_call() {
        let entries = load_entries_from_str(MAIN).unwrap();
        let detail = &entries[3].detail;

        assert!(detail.contains("type: tool_use | name: Bash | call_id: toolu_1"));
        assert!(detail.contains("$ git status"));
        assert!(detail.ends_with("OUTPUT\n------\nclean"));
    }

    #[test]
    fn non_bash_tools_and_thinking_are_not_entries() {
        let entries = load_entries_from_str(MAIN).unwrap();

        for hidden in [
            "file body",
            "/tmp/a",
            "hidden",
            "No response requested",
            "blob",
        ] {
            assert!(
                entries.iter().all(|entry| !entry.detail.contains(hidden)),
                "{hidden} must not be replayed"
            );
        }
    }

    #[test]
    fn subagent_handback_is_an_agent_entry() {
        let entries = load_entries_from_str(SUB).unwrap();

        assert_eq!(kinds(&entries), vec![EntryKind::User, EntryKind::Agent]);
        assert_eq!(entries[0].detail, "count the files");
        assert_eq!(entries[1].detail, "there are 3 files");
    }

    #[test]
    fn loads_preprocessed_json_array_events() {
        let array = format!("[{}]", MAIN.lines().collect::<Vec<_>>().join(","));

        let from_array = load_entries_from_str(&array).expect("json array should parse");
        let from_lines = load_entries_from_str(MAIN).unwrap();

        assert_eq!(
            from_array
                .iter()
                .map(|entry| (entry.kind, entry.detail.as_str()))
                .collect::<Vec<_>>(),
            from_lines
                .iter()
                .map(|entry| (entry.kind, entry.detail.as_str()))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn initial_visibility_filters_without_dropping_entries() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let hidden = ReplayApp::new(entries.clone(), ExecVisibility::Hidden);
        let shown = ReplayApp::new(entries, ExecVisibility::Shown);

        assert_eq!(hidden.all_entries.len(), 4);
        assert_eq!(hidden.visible_indices, vec![0, 2]);
        assert_eq!(shown.all_entries.len(), 4);
        assert_eq!(shown.visible_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn toggle_preserves_selected_non_exec_entry() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Hidden);
        app.list_state.select(Some(1));

        app.toggle_exec_visibility();
        assert_eq!(app.selected_all_index(), Some(2));
        assert_eq!(app.selected_visible_index(), Some(2));

        app.toggle_exec_visibility();
        assert_eq!(app.selected_all_index(), Some(2));
        assert_eq!(app.selected_visible_index(), Some(1));
    }

    #[test]
    fn hiding_selected_exec_prefers_next_then_previous() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);
        app.list_state.select(Some(1));

        app.toggle_exec_visibility();
        assert_eq!(app.selected_all_index(), Some(2));

        let entries = vec![
            Entry {
                time: None,
                kind: EntryKind::User,
                summary: "USER".to_string(),
                detail: "hello".to_string(),
            },
            Entry {
                time: None,
                kind: EntryKind::Exec,
                summary: "EXEC pwd".to_string(),
                detail: "/tmp".to_string(),
            },
        ];
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);
        app.list_state.select(Some(1));

        app.toggle_exec_visibility();
        assert_eq!(app.selected_all_index(), Some(0));
    }

    #[test]
    fn only_exec_entries_transition_through_empty_selection() {
        let entries = vec![Entry {
            time: None,
            kind: EntryKind::Exec,
            summary: "EXEC pwd".to_string(),
            detail: "/tmp".to_string(),
        }];
        let mut app = ReplayApp::new(entries, ExecVisibility::Hidden);

        assert_eq!(app.all_entries.len(), 1);
        assert!(app.visible_indices.is_empty());
        assert_eq!(app.selected_visible_index(), None);

        app.toggle_exec_visibility();
        assert_eq!(app.visible_indices, vec![0]);
        assert_eq!(app.selected_visible_index(), Some(0));

        app.toggle_exec_visibility();
        assert!(app.visible_indices.is_empty());
        assert_eq!(app.selected_visible_index(), None);
    }

    #[test]
    fn display_titles_remain_dense_for_each_visibility() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let hidden = ReplayApp::new(entries.clone(), ExecVisibility::Hidden);
        let shown = ReplayApp::new(entries, ExecVisibility::Shown);

        let hidden_titles = hidden
            .visible_indices
            .iter()
            .enumerate()
            .map(|(visible_index, all_index)| {
                display_title(visible_index, &hidden.all_entries[*all_index])
            })
            .collect::<Vec<_>>();
        assert_eq!(hidden_titles, vec!["#0000 USER", "#0001 AGENT"]);

        let shown_titles = shown
            .visible_indices
            .iter()
            .enumerate()
            .map(|(visible_index, all_index)| {
                display_title(visible_index, &shown.all_entries[*all_index])
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shown_titles,
            vec![
                "#0000 USER",
                "#0001 EXEC pwd",
                "#0002 AGENT",
                "#0003 EXEC git status"
            ]
        );

        assert_eq!(
            detail_text(&hidden.all_entries[2], 1, &[], None).lines[0].spans[1].content,
            "1"
        );
        assert_eq!(
            detail_text(&shown.all_entries[2], 2, &[], None).lines[0].spans[1].content,
            "2"
        );
    }

    #[test]
    fn detail_half_page_and_end_follow_the_rendered_pane_height() {
        let entry = Entry {
            time: None,
            kind: EntryKind::User,
            summary: "USER".to_string(),
            detail: (1..=30)
                .map(|line| format!("line {line:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let mut app = ReplayApp::new(vec![entry], ExecVisibility::Hidden);
        app.focus = PaneFocus::Detail;

        render_at(&mut app, 100, 18);
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.detail_scroll, 7);

        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.detail_scroll, 19);
    }

    #[test]
    fn fullscreen_detail_syncs_focus_for_detail_navigation() {
        let entry = Entry {
            time: None,
            kind: EntryKind::User,
            summary: "USER".to_string(),
            detail: (1..=30)
                .map(|line| format!("line {line:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let mut app = ReplayApp::new(vec![entry], ExecVisibility::Hidden);

        render_at(&mut app, 100, 18);
        app.handle_key(key(KeyCode::Char('2')));
        assert_eq!(app.fullscreen, Fullscreen::Detail);
        assert_eq!(app.focus, PaneFocus::Detail);

        render_at(&mut app, 100, 18);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.detail_scroll, 19);
    }

    #[test]
    fn timeline_half_page_follows_the_rendered_pane_height() {
        let entries = (0..20)
            .map(|index| Entry {
                time: None,
                kind: EntryKind::User,
                summary: format!("USER {index}"),
                detail: String::new(),
            })
            .collect();
        let mut app = ReplayApp::new(entries, ExecVisibility::Hidden);

        render_at(&mut app, 100, 18);
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.selected_visible_index(), Some(7));

        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.selected_visible_index(), Some(0));

        app.handle_key(key(KeyCode::Char('G')));
        render_at(&mut app, 100, 18);
        assert_eq!(app.selected_visible_index(), Some(19));
        assert_eq!(app.list_state.offset(), 6);
    }

    #[test]
    fn replay_help_is_modal_and_fullscreen_allows_toggle() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);

        assert_eq!(
            app.handle_key(key(KeyCode::Char('?'))),
            ReplayControl::Continue
        );
        assert!(app.show_help);
        for code in [KeyCode::Char('e'), KeyCode::Char('1'), KeyCode::Tab] {
            assert_eq!(app.handle_key(key(code)), ReplayControl::Continue);
        }
        assert_eq!(app.exec_visibility, ExecVisibility::Shown);
        assert_eq!(app.fullscreen, Fullscreen::None);
        assert_eq!(app.focus, PaneFocus::Timeline);

        app.handle_key(key(KeyCode::Esc));
        assert!(!app.show_help);
        app.handle_key(key(KeyCode::Char('1')));
        assert_eq!(app.fullscreen, Fullscreen::Timeline);
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.exec_visibility, ExecVisibility::Hidden);
        assert_eq!(app.fullscreen, Fullscreen::Timeline);
    }

    #[test]
    fn repeated_toggle_reuses_all_entries() {
        let entries = (0..10_000)
            .map(|index| Entry {
                time: None,
                kind: if index % 2 == 0 {
                    EntryKind::User
                } else {
                    EntryKind::Exec
                },
                summary: format!("entry {index}"),
                detail: String::new(),
            })
            .collect();
        let mut app = ReplayApp::new(entries, ExecVisibility::Hidden);

        for _ in 0..100 {
            app.toggle_exec_visibility();
            assert_eq!(app.all_entries.len(), 10_000);
        }

        assert_eq!(app.exec_visibility, ExecVisibility::Hidden);
        assert_eq!(app.visible_indices.len(), 5_000);
    }

    fn type_query(app: &mut ReplayApp, query: &str) {
        app.handle_key(key(KeyCode::Char('/')));
        for ch in query.chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
    }

    fn terms(query: &str) -> Vec<String> {
        query_terms(query)
    }

    #[test]
    fn search_filters_timeline_and_keeps_entry_numbers() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);

        type_query(&mut app, "GIT status");
        assert_eq!(app.mode, Mode::Search);
        assert_eq!(app.visible_indices, vec![3]);
        assert_eq!(app.display_numbers, vec![3]);
        assert_eq!(
            app.selected_detail_string().unwrap().lines().next(),
            Some("#0003 EXEC git status")
        );

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.visible_indices, vec![3]);
    }

    #[test]
    fn search_input_takes_normal_mode_keys_as_text() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);

        app.handle_key(key(KeyCode::Char('/')));
        for ch in ['q', 'e', 'n'] {
            assert_eq!(
                app.handle_key(key(KeyCode::Char(ch))),
                ReplayControl::Continue
            );
        }
        assert_eq!(app.query, "qen");
        assert_eq!(app.exec_visibility, ExecVisibility::Shown);

        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.query, "qe");
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ReplayControl::Quit
        );
    }

    #[test]
    fn search_respects_exec_visibility() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Hidden);

        type_query(&mut app, "git");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.visible_indices.is_empty());

        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.visible_indices, vec![3]);
        assert_eq!(app.display_numbers, vec![3]);
    }

    #[test]
    fn esc_clears_filter_and_keeps_the_selected_entry() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);

        type_query(&mut app, "done");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.visible_indices, vec![2]);

        assert_eq!(app.handle_key(key(KeyCode::Esc)), ReplayControl::Continue);
        assert!(app.query.is_empty());
        assert_eq!(app.visible_indices, vec![0, 1, 2, 3]);
        assert_eq!(app.selected_all_index(), Some(2));

        assert_eq!(app.handle_key(key(KeyCode::Esc)), ReplayControl::Quit);
    }

    #[test]
    fn esc_in_fullscreen_leaves_fullscreen_before_clearing_filter() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);

        type_query(&mut app, "done");
        app.handle_key(key(KeyCode::Enter));
        app.handle_key(key(KeyCode::Char('2')));

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.fullscreen, Fullscreen::None);
        assert_eq!(app.query, "done");
    }

    #[test]
    fn clearing_an_empty_result_restores_the_previous_selection() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);
        app.list_state.select(Some(2));

        type_query(&mut app, "zzz");
        assert!(app.visible_indices.is_empty());
        assert_eq!(app.selected_all_index(), None);
        render_at(&mut app, 100, 18);

        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.selected_all_index(), Some(2));
    }

    #[test]
    fn match_ranges_fold_ascii_case_and_keep_byte_offsets() {
        assert_eq!(
            match_ranges("한글 Docker docker", &terms("DOCKER")),
            vec![(7, 13), (14, 20)]
        );
        assert_eq!(match_ranges("abcabc", &terms("abc bca")), vec![(0, 6)]);
        assert_eq!(match_ranges("한글 검색", &terms("검색")), vec![(7, 13)]);
        assert!(match_ranges("anything", &[]).is_empty());
    }

    #[test]
    fn detail_text_highlights_matches_and_marks_the_current_one() {
        let entry = Entry {
            time: None,
            kind: EntryKind::User,
            summary: "USER".to_string(),
            detail: "plain\nfix Docker and docker".to_string(),
        };

        let text = detail_text(&entry, 0, &terms("docker"), Some(1));
        assert_eq!(text.lines[3].spans.len(), 1);

        let spans = &text.lines[4].spans;
        let contents: Vec<&str> = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(contents, vec!["fix ", "Docker", " and ", "docker"]);
        assert_eq!(spans[1].style.bg, Some(Color::Yellow));
        assert_eq!(spans[3].style.bg, Some(Color::LightMagenta));
        assert_eq!(spans[0].style, Style::default());

        let plain = detail_text(&entry, 0, &[], None);
        assert_eq!(plain.lines[4].spans.len(), 1);
    }

    #[test]
    fn n_and_shift_n_scroll_the_detail_between_matches() {
        let entry = Entry {
            time: None,
            kind: EntryKind::User,
            summary: "USER".to_string(),
            detail: (1..=30)
                .map(|line| {
                    if line == 10 || line == 25 {
                        format!("line {line:02} needle")
                    } else {
                        format!("line {line:02}")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
        };
        let mut app = ReplayApp::new(vec![entry], ExecVisibility::Hidden);
        type_query(&mut app, "needle");
        app.handle_key(key(KeyCode::Enter));
        render_at(&mut app, 100, 18);

        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.detail_match, None, "timeline focus must not jump");

        app.focus = PaneFocus::Detail;
        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.detail_match, Some((0, 0)));
        assert_eq!(app.detail_scroll, 10);
        assert_eq!(app.status.as_deref(), Some("match 1/2"));

        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.detail_match, Some((0, 1)));
        assert_eq!(app.detail_scroll, 19);

        app.handle_key(key(KeyCode::Char('n')));
        assert_eq!(app.detail_match, Some((0, 0)));

        app.handle_key(key(KeyCode::Char('N')));
        assert_eq!(app.detail_match, Some((0, 1)));
        assert_eq!(app.status.as_deref(), Some("match 2/2"));
    }

    #[test]
    fn match_jump_counts_wrapped_rows() {
        let entry = Entry {
            time: None,
            kind: EntryKind::User,
            summary: "USER".to_string(),
            detail: format!("{}\nneedle", "word ".repeat(60)),
        };

        assert_eq!(detail_match_lines(&entry, &terms("needle")), vec![1]);
        assert_eq!(detail_row_of_line(&entry, 0, 1, 100), 3 + 3);
    }

    #[test]
    fn timeline_rows_end_with_record_time() {
        let entries = load_entries_from_str(MAIN).unwrap();
        let times: Vec<_> = entries.iter().map(|entry| entry.time.clone()).collect();
        assert!(times.iter().all(Option::is_some), "{times:?}");
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);

        for width in [200, 100] {
            let backend = TestBackend::new(width, 18);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| render(frame, &mut app)).unwrap();
            let screen = format!("{}", terminal.backend());
            let row = screen
                .lines()
                .find(|line| line.contains("#0003"))
                .expect("exec row is rendered");
            let time = times[3].as_deref().unwrap();
            assert!(row.contains(&format!(" {time}")), "{row}");
        }

        let entries = load_entries_from_str(common_fixture()).unwrap();
        assert_eq!(entries[0].time, None);
        assert_eq!(
            record_time(&serde_json::json!({"timestamp": "2026-11-01T09:05:30.000Z"})).as_deref(),
            Some("2026-11-01 09:05")
        );
    }

    #[test]
    fn search_renders_filter_state() {
        let entries = load_entries_from_str(common_fixture()).unwrap();
        let mut app = ReplayApp::new(entries, ExecVisibility::Shown);
        type_query(&mut app, "git");

        let backend = TestBackend::new(120, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let screen = format!("{}", terminal.backend());

        assert!(screen.contains("| /git"));
        assert!(screen.contains("Timeline [focus] [search]"));
        assert!(screen.contains("#0003 EXEC git status"));
        assert!(!screen.contains("#0001 EXEC pwd"));
        assert!(screen.contains("search: git"));
    }

    #[test]
    fn invalid_json_error_is_independent_of_visibility() {
        assert!(load_entries_from_str("{invalid").is_err());
    }

    #[test]
    fn reader_input_preserves_jsonl() {
        let mut input = br#"{"type":"user","message":{"role":"user","content":"hi"}}"#.as_slice();

        assert_eq!(
            read_input_from_reader(&mut input).unwrap(),
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#
        );
    }
}
