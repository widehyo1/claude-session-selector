use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap,
    },
};

use crate::{
    SessionRow, filter_sessions,
    indexer::search::{ContentScope, QueryError, SearchIndex, is_corruption},
    session_date, terminal,
    ui::{bottom_scroll_offset, fit_width, fit_width_tail, half_page_height, pane_content_area},
    ui_state::ExecVisibility,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Search,
    ColumnSearch(Column),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneFocus {
    Sessions,
    Message,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchTarget {
    Any,
    Content(ContentScope),
}
impl SearchTarget {
    fn next(self) -> Self {
        match self {
            Self::Any => Self::Content(ContentScope::All),
            Self::Content(ContentScope::All) => Self::Content(ContentScope::User),
            Self::Content(ContentScope::User) => Self::Content(ContentScope::Agent),
            Self::Content(ContentScope::Agent) => Self::Content(ContentScope::Exec),
            Self::Content(ContentScope::Exec) => Self::Any,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Content(ContentScope::All) => "content:all",
            Self::Content(ContentScope::User) => "content:user",
            Self::Content(ContentScope::Agent) => "content:agent",
            Self::Content(ContentScope::Exec) => "content:exec",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Column {
    Modified,
    Path,
    Branch,
    Repository,
    Title,
}

impl Column {
    const ALL: [Self; 5] = [
        Self::Modified,
        Self::Path,
        Self::Branch,
        Self::Repository,
        Self::Title,
    ];

    fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Path => "Path",
            Self::Branch => "Branch",
            Self::Repository => "Repository",
            Self::Title => "Title",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Path => "path",
            Self::Branch => "branch",
            Self::Repository => "repository",
            Self::Title => "title",
        }
    }

    /// Width in terminal cells; the title column is unbounded.
    fn width(self) -> Option<usize> {
        match self {
            Self::Modified => Some(MODIFIED_WIDTH),
            Self::Path => Some(PATH_WIDTH),
            Self::Branch => Some(BRANCH_WIDTH),
            Self::Repository => Some(REPOSITORY_WIDTH),
            Self::Title => None,
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Modified => Color::Cyan,
            Self::Path => Color::Green,
            Self::Branch => Color::Yellow,
            Self::Repository => Color::Magenta,
            Self::Title => Color::White,
        }
    }

    fn value(self, row: &SessionRow) -> &str {
        match self {
            Self::Modified => row.file_modified_at.as_deref(),
            Self::Path => row.cwd.as_deref(),
            Self::Branch => row.branch.as_deref(),
            Self::Repository => row.repository_url.as_deref(),
            Self::Title => row.title.as_deref(),
        }
        .unwrap_or_default()
    }

    fn searchable(self) -> bool {
        self != Self::Modified
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortDirection {
    Desc,
    Asc,
}

type ColumnFilters = [String; Column::ALL.len()];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentJump {
    Moved,
    NotSubsession,
    NotInView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SelectorAction {
    Quit,
    OpenReplay(PathBuf),
}

pub(crate) struct SelectorApp {
    search_index: SearchIndex,
    all_rows: Vec<SessionRow>,
    total_rows: usize,
    show_subsession_badges: bool,
    /// Result of the global search, before column filters and sort.
    search_rows: Vec<SessionRow>,
    filtered: Vec<SessionRow>,
    list_state: ListState,
    query: String,
    search_target: SearchTarget,
    column_filters: ColumnFilters,
    sort: Option<(Column, SortDirection)>,
    /// Input and target to restore when a search is cancelled with Esc.
    search_backup: Option<(String, SearchTarget)>,
    mode: Mode,
    focus: PaneFocus,
    header_focus: bool,
    header_column: Column,
    metadata_scroll: usize,
    sessions_view_width: u16,
    message_scroll: u16,
    sessions_page_height: u16,
    message_page_height: u16,
    message_max_scroll: u16,
    show_help: bool,
    show_details: bool,
    details_scroll: u16,
    details_max_scroll: u16,
    details_page_height: u16,
    exec_visibility: ExecVisibility,
    status: Option<String>,
}

impl SelectorApp {
    pub(crate) fn new(search_index: SearchIndex, exec_visibility: ExecVisibility) -> Result<Self> {
        let filtered = search_index.all_sessions()?;
        let total_rows = filtered.len();
        let show_subsession_badges = search_index.includes_subsessions();
        let mut list_state = ListState::default();
        if !filtered.is_empty() {
            list_state.select(Some(0));
        }

        Ok(Self {
            search_index,
            all_rows: filtered.clone(),
            total_rows,
            show_subsession_badges,
            search_rows: filtered.clone(),
            filtered,
            list_state,
            query: String::new(),
            search_target: SearchTarget::Any,
            column_filters: Default::default(),
            sort: None,
            search_backup: None,
            mode: Mode::Normal,
            focus: PaneFocus::Sessions,
            header_focus: false,
            header_column: Column::Modified,
            metadata_scroll: 0,
            sessions_view_width: 0,
            message_scroll: 0,
            sessions_page_height: 1,
            message_page_height: 1,
            message_max_scroll: 0,
            show_help: false,
            show_details: false,
            details_scroll: 0,
            details_max_scroll: 0,
            details_page_height: 1,
            exec_visibility,
            status: None,
        })
    }

    fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    fn selected_row(&self) -> Option<&SessionRow> {
        self.selected_index()
            .and_then(|index| self.filtered.get(index))
    }

    fn selected_path(&self) -> Option<PathBuf> {
        self.selected_row().map(|row| row.path.clone())
    }

    fn selected_resume_command(&self) -> Option<String> {
        self.selected_row()
            .and_then(|row| resume_command(row, &self.all_rows))
    }

    // Searches only the current list so the query is kept.
    fn jump_to_parent(&mut self) -> ParentJump {
        let Some(parent_id) = self
            .selected_row()
            .and_then(|row| row.parent_session_id.clone())
        else {
            return ParentJump::NotSubsession;
        };
        match self.filtered.iter().position(|candidate| {
            !candidate.is_subsession && candidate.id.as_deref() == Some(parent_id.as_str())
        }) {
            Some(index) => {
                self.list_state.select(Some(index));
                self.message_scroll = 0;
                ParentJump::Moved
            }
            None => ParentJump::NotInView,
        }
    }

    fn select_parent(&mut self) {
        self.status = match self.jump_to_parent() {
            ParentJump::Moved => None,
            ParentJump::NotSubsession => Some("selected session has no parent".to_string()),
            ParentJump::NotInView => Some("parent session is not in the current list".to_string()),
        };
    }

    fn refresh_filter(&mut self) {
        let result = match self.search_target {
            SearchTarget::Any => Ok(filter_sessions(&self.all_rows, &self.query)),
            SearchTarget::Content(scope) => self
                .search_index
                .search_content(&self.query, scope)
                .map(|hits| hits.into_iter().map(|hit| hit.row).collect()),
        };
        match result {
            Ok(rows) => {
                self.search_rows = rows;
                self.apply_view(false);
                self.status = None;
            }
            Err(error) => {
                self.status = Some(format_search_error(&error));
            }
        }
    }

    /// Applies the column filters and the sort to the global search result.
    /// A sort change keeps the selected session; a filter change selects the
    /// first row.
    fn apply_view(&mut self, keep_selection: bool) {
        let selected = if keep_selection {
            self.selected_path()
        } else {
            None
        };
        let mut rows: Vec<SessionRow> = self
            .search_rows
            .iter()
            .filter(|row| matches_column_filters(&self.column_filters, row))
            .cloned()
            .collect();
        if let Some((column, direction)) = self.sort {
            rows.sort_by(|left, right| compare_column(column, direction, left, right));
        }
        self.filtered = rows;
        let index = selected
            .and_then(|path| self.filtered.iter().position(|row| row.path == path))
            .or((!self.filtered.is_empty()).then_some(0));
        if !keep_selection || index != self.selected_index() {
            self.message_scroll = 0;
        }
        self.list_state.select(index);
    }

    /// Cycles the column through desc, asc, and the default order.
    fn cycle_sort(&mut self, column: Column) {
        self.sort = match self.sort {
            Some((current, SortDirection::Desc)) if current == column => {
                Some((column, SortDirection::Asc))
            }
            Some((current, SortDirection::Asc)) if current == column => None,
            _ => Some((column, SortDirection::Desc)),
        };
        self.apply_view(true);
    }

    fn enter_header(&mut self) {
        self.focus = PaneFocus::Sessions;
        self.header_focus = true;
        self.ensure_column_visible();
    }

    fn focus_column(&mut self, column: Column) {
        self.header_column = column;
        self.ensure_column_visible();
    }

    fn focus_adjacent_column(&mut self, forward: bool) {
        let index = self.header_column.index();
        let next = if forward {
            (index + 1).min(Column::ALL.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.focus_column(Column::ALL[next]);
    }

    /// Scrolls horizontally so the focused column is in view.
    fn ensure_column_visible(&mut self) {
        let view = usize::from(self.sessions_view_width);
        let column = self.header_column;
        if column == Column::ALL[0] {
            self.metadata_scroll = 0;
            return;
        }
        let start = column_start(column, self.show_subsession_badges);
        let width = column.width().unwrap_or(column.label().len() + 2);
        if start < self.metadata_scroll {
            self.metadata_scroll = start;
        } else if view > 0 && start + width > self.metadata_scroll + view {
            self.metadata_scroll = (start + width).saturating_sub(view);
        }
    }

    fn start_search(&mut self, mode: Mode) {
        self.mode = mode;
        self.search_backup = Some((self.search_input().clone(), self.search_target));
    }

    fn search_input(&self) -> &String {
        match self.mode {
            Mode::ColumnSearch(column) => &self.column_filters[column.index()],
            _ => &self.query,
        }
    }

    fn search_input_mut(&mut self) -> &mut String {
        match self.mode {
            Mode::ColumnSearch(column) => &mut self.column_filters[column.index()],
            _ => &mut self.query,
        }
    }

    fn refresh_search(&mut self) {
        match self.mode {
            Mode::ColumnSearch(_) => self.apply_view(false),
            _ => self.refresh_filter(),
        }
    }

    fn cancel_search(&mut self) {
        if let Some((input, target)) = self.search_backup.take() {
            let changed = *self.search_input() != input || self.search_target != target;
            *self.search_input_mut() = input;
            self.search_target = target;
            if changed {
                self.refresh_search();
            }
        }
        self.mode = Mode::Normal;
    }

    /// Esc in the session list: clears the global search, then the column
    /// filters, then the sort. Returns false when nothing was active.
    fn clear_one_layer(&mut self) -> bool {
        if !self.query.is_empty() {
            self.query.clear();
            self.refresh_filter();
        } else if self.column_filters.iter().any(|filter| !filter.is_empty()) {
            self.column_filters = Default::default();
            self.apply_view(false);
        } else if self.sort.is_some() {
            self.sort = None;
            self.apply_view(true);
        } else {
            return false;
        }
        true
    }

    fn handle_header_key(&mut self, key: KeyEvent) -> Option<SelectorAction> {
        match key.code {
            KeyCode::Char('q') => return Some(SelectorAction::Quit),
            KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('j') | KeyCode::Down => {
                self.header_focus = false
            }
            KeyCode::Char('h') | KeyCode::Left => self.focus_adjacent_column(false),
            KeyCode::Char('l') | KeyCode::Right => self.focus_adjacent_column(true),
            KeyCode::Char(ch @ '1'..='5') => {
                self.focus_column(Column::ALL[usize::from(ch as u8 - b'1')])
            }
            KeyCode::Enter => self.cycle_sort(self.header_column),
            KeyCode::Char('/') => {
                if self.header_column.searchable() {
                    self.start_search(Mode::ColumnSearch(self.header_column));
                } else {
                    self.status = Some(format!(
                        "{} is not searchable; Enter sorts it",
                        self.header_column.name()
                    ));
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Tab => {
                self.header_focus = false;
                self.toggle_focus();
            }
            _ => {}
        }
        None
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            PaneFocus::Sessions => PaneFocus::Message,
            PaneFocus::Message => PaneFocus::Sessions,
        };
    }

    fn next_search_target(&mut self) {
        self.search_target = self.search_target.next();
        self.refresh_filter();
    }

    fn metadata_scroll_right(&mut self) {
        self.metadata_scroll = self.metadata_scroll.saturating_add(4);
    }

    fn metadata_scroll_left(&mut self) {
        self.metadata_scroll = self.metadata_scroll.saturating_sub(4);
    }

    fn metadata_scroll_home(&mut self) {
        self.metadata_scroll = 0;
    }

    fn message_line_down(&mut self) {
        self.message_scroll = self
            .message_scroll
            .saturating_add(1)
            .min(self.message_max_scroll);
    }

    fn message_line_up(&mut self) {
        self.message_scroll = self.message_scroll.saturating_sub(1);
    }

    fn message_page_down(&mut self) {
        self.message_scroll = self
            .message_scroll
            .saturating_add(half_page_height(self.message_page_height))
            .min(self.message_max_scroll);
    }

    fn message_page_up(&mut self) {
        self.message_scroll = self
            .message_scroll
            .saturating_sub(half_page_height(self.message_page_height));
    }

    fn copy_resume_command_to_clipboard(&mut self) {
        let Some(command) = self.selected_resume_command() else {
            self.status = Some(match self.selected_row() {
                Some(row) if row.is_subsession => {
                    "parent session is not indexed in this view".to_string()
                }
                _ => "selected session has no id".to_string(),
            });
            return;
        };

        match arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(command.clone()))
        {
            Ok(()) => {
                self.status = Some(format!("copied: {command}"));
            }
            Err(err) => {
                self.status = Some(format!("clipboard copy failed: {err}"));
            }
        }
    }

    pub(crate) fn exec_visibility(&self) -> ExecVisibility {
        self.exec_visibility
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }

    pub(crate) fn set_exec_visibility(&mut self, visibility: ExecVisibility) {
        self.exec_visibility = visibility;
    }

    fn toggle_exec_visibility(&mut self) {
        self.exec_visibility.toggle();
        self.status = None;
    }

    fn move_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let next = match self.selected_index() {
            Some(index) if index + 1 < self.filtered.len() => index + 1,
            _ => 0,
        };
        self.list_state.select(Some(next));
        self.message_scroll = 0;
    }

    fn move_previous(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let previous = match self.selected_index() {
            Some(0) | None => self.filtered.len() - 1,
            Some(index) => index - 1,
        };
        self.list_state.select(Some(previous));
        self.message_scroll = 0;
    }

    fn move_first(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(0));
            self.message_scroll = 0;
        }
    }

    fn move_last(&mut self) {
        if !self.filtered.is_empty() {
            self.list_state.select(Some(self.filtered.len() - 1));
            self.message_scroll = 0;
        }
    }

    fn page_down(&mut self, amount: usize) {
        for _ in 0..amount.max(1) {
            self.move_next();
        }
    }

    fn page_up(&mut self, amount: usize) {
        for _ in 0..amount.max(1) {
            self.move_previous();
        }
    }

    pub(crate) fn clear_status(&mut self) {
        self.status = None;
    }

    pub(crate) fn set_status(&mut self, status: impl Into<String>) {
        self.status = Some(status.into());
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<SelectorAction> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(SelectorAction::Quit);
        }

        if self.show_details {
            match key.code {
                KeyCode::Esc | KeyCode::Char('i') | KeyCode::Char('q') => self.show_details = false,
                KeyCode::Char('j') | KeyCode::Down => {
                    self.details_scroll = self
                        .details_scroll
                        .saturating_add(1)
                        .min(self.details_max_scroll)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.details_scroll = self.details_scroll.saturating_sub(1)
                }
                KeyCode::Char('d') | KeyCode::PageDown => {
                    self.details_scroll = self
                        .details_scroll
                        .saturating_add(half_page_height(self.details_page_height))
                        .min(self.details_max_scroll)
                }
                KeyCode::Char('u') | KeyCode::PageUp => {
                    self.details_scroll = self
                        .details_scroll
                        .saturating_sub(half_page_height(self.details_page_height))
                }
                KeyCode::Char('g') | KeyCode::Home => self.details_scroll = 0,
                KeyCode::Char('G') | KeyCode::End => self.details_scroll = self.details_max_scroll,
                _ => {}
            }
            return None;
        }

        if self.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => self.show_help = false,
                KeyCode::Char('q') => return Some(SelectorAction::Quit),
                _ => {}
            }
            return None;
        }

        if self.mode == Mode::Normal && self.header_focus {
            return self.handle_header_key(key);
        }

        match self.mode {
            Mode::Normal => match key.code {
                KeyCode::Char('q') => Some(SelectorAction::Quit),
                KeyCode::Esc => {
                    if self.clear_one_layer() {
                        None
                    } else {
                        Some(SelectorAction::Quit)
                    }
                }
                KeyCode::Char('s') if key.modifiers.is_empty() => {
                    self.enter_header();
                    None
                }
                KeyCode::Enter => self.selected_path().map(SelectorAction::OpenReplay),
                KeyCode::Char('e') if key.modifiers.is_empty() => {
                    self.toggle_exec_visibility();
                    None
                }
                KeyCode::Char('/') => {
                    self.start_search(Mode::Search);
                    None
                }
                KeyCode::Char('?') => {
                    self.show_help = true;
                    None
                }
                KeyCode::Char('i') if key.modifiers.is_empty() => {
                    if self.selected_row().is_some() {
                        self.show_details = true;
                        self.details_scroll = 0;
                    }
                    None
                }
                KeyCode::Char('y') => {
                    self.copy_resume_command_to_clipboard();
                    None
                }
                KeyCode::Char('p') => {
                    self.select_parent();
                    None
                }
                KeyCode::Tab => {
                    self.toggle_focus();
                    None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    match self.focus {
                        PaneFocus::Sessions => self.move_next(),
                        PaneFocus::Message => self.message_line_down(),
                    }
                    None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    match self.focus {
                        PaneFocus::Sessions => self.move_previous(),
                        PaneFocus::Message => self.message_line_up(),
                    }
                    None
                }
                KeyCode::Char('d') | KeyCode::PageDown => {
                    match self.focus {
                        PaneFocus::Sessions => {
                            self.page_down(usize::from(half_page_height(self.sessions_page_height)))
                        }
                        PaneFocus::Message => self.message_page_down(),
                    }
                    None
                }
                KeyCode::Char('u') | KeyCode::PageUp => {
                    match self.focus {
                        PaneFocus::Sessions => {
                            self.page_up(usize::from(half_page_height(self.sessions_page_height)))
                        }
                        PaneFocus::Message => self.message_page_up(),
                    }
                    None
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    match self.focus {
                        PaneFocus::Sessions => self.move_first(),
                        PaneFocus::Message => self.message_scroll = 0,
                    }
                    None
                }
                KeyCode::Char('G') | KeyCode::End => {
                    match self.focus {
                        PaneFocus::Sessions => self.move_last(),
                        PaneFocus::Message => self.message_scroll = self.message_max_scroll,
                    }
                    None
                }
                KeyCode::Char('h') | KeyCode::Left => {
                    self.metadata_scroll_left();
                    None
                }
                KeyCode::Char('l') | KeyCode::Right => {
                    self.metadata_scroll_right();
                    None
                }
                KeyCode::Char('0') => {
                    self.metadata_scroll_home();
                    None
                }
                _ => None,
            },
            Mode::Search | Mode::ColumnSearch(_) => {
                match key.code {
                    KeyCode::Enter => {
                        self.mode = Mode::Normal;
                        self.search_backup = None;
                    }
                    KeyCode::Esc => self.cancel_search(),
                    KeyCode::Tab if self.mode == Mode::Search => self.next_search_target(),
                    KeyCode::Backspace => {
                        self.search_input_mut().pop();
                        self.refresh_search();
                    }
                    KeyCode::Char(ch) => {
                        self.search_input_mut().push(ch);
                        self.refresh_search();
                    }
                    _ => {}
                }
                None
            }
        }
    }
}

pub(crate) fn run(app: &mut SelectorApp) -> Result<SelectorAction> {
    terminal::with_terminal(|terminal| run_event_loop(terminal, app))
}

fn run_event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut SelectorApp,
) -> Result<SelectorAction> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if let Some(action) = app.handle_key(key) {
                return Ok(action);
            }
        }
    }
}

fn render(frame: &mut Frame, app: &mut SelectorApp) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, root[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(root[1]);

    render_list(frame, body[0], app);
    render_message(frame, body[1], app);
    render_footer(frame, root[2], app);

    if app.show_help {
        render_help(frame);
    } else if app.show_details {
        render_details(frame, app);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &SelectorApp) {
    let selected = app
        .selected_index()
        .map(|index| format!("{}/{}", index + 1, app.filtered.len()))
        .unwrap_or_else(|| "0/0".to_string());

    let query = if app.query.is_empty() {
        String::new()
    } else {
        format!(" | /{}", app.query)
    };

    let header = Line::from(vec![
        Span::styled(
            " Claude Sessions ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(selected, Style::default().fg(Color::Cyan)),
        Span::raw(format!(
            " of {}{query} | exec: {}",
            app.total_rows,
            app.exec_visibility.label()
        )),
        Span::styled(view_summary(app), Style::default().fg(Color::Yellow)),
    ]);

    frame.render_widget(header, area);
}

const HIGHLIGHT_SYMBOL: &str = "▶ ";

/// Active sort and column filters, e.g. ` | sort: branch▼ | filter: path~ocr`.
fn view_summary(app: &SelectorApp) -> String {
    let mut summary = String::new();
    if let Some((column, direction)) = app.sort {
        let arrow = match direction {
            SortDirection::Desc => "▼",
            SortDirection::Asc => "▲",
        };
        summary.push_str(&format!(" | sort: {}{arrow}", column.name()));
    }
    let filters: Vec<String> = Column::ALL
        .into_iter()
        .filter(|column| !app.column_filters[column.index()].is_empty())
        .map(|column| format!("{}~{}", column.name(), app.column_filters[column.index()]))
        .collect();
    if !filters.is_empty() {
        summary.push_str(&format!(" | filter: {}", filters.join(" ")));
    }
    summary
}

fn render_list(frame: &mut Frame, area: Rect, app: &mut SelectorApp) {
    let content = pane_content_area(area);
    let [header_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(content);
    app.sessions_page_height = list_area.height.max(1);
    app.sessions_view_width = list_area
        .width
        .saturating_sub(HIGHLIGHT_SYMBOL.chars().count() as u16);

    let items: Vec<ListItem> = app
        .filtered
        .iter()
        .map(|row| {
            ListItem::new(metadata_line(
                row,
                app.show_subsession_badges,
                app.metadata_scroll,
            ))
        })
        .collect();

    let header_focused = app.mode == Mode::Normal && app.header_focus;
    let title = if app.mode != Mode::Normal {
        " Sessions [search] "
    } else if header_focused {
        " Sessions [header] "
    } else if app.focus == PaneFocus::Sessions {
        " Sessions [focus] "
    } else {
        " Sessions "
    };

    let border_style = if app.mode != Mode::Normal {
        Style::default().fg(Color::Yellow)
    } else if app.focus == PaneFocus::Sessions {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    frame.render_widget(
        Block::new()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style),
        area,
    );

    let focused_column = match app.mode {
        Mode::ColumnSearch(column) => Some(column),
        Mode::Normal if app.header_focus => Some(app.header_column),
        _ => None,
    };
    let mut header = scrolled_segments(
        &header_segments(
            app.show_subsession_badges,
            app.sort,
            &app.column_filters,
            focused_column,
        ),
        app.metadata_scroll,
    );
    header
        .spans
        .insert(0, Span::raw(" ".repeat(HIGHLIGHT_SYMBOL.chars().count())));
    frame.render_widget(
        header.style(Style::default().add_modifier(Modifier::BOLD)),
        header_area,
    );

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(HIGHLIGHT_SYMBOL)
        .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(list, list_area, &mut app.list_state);
}

fn render_message(frame: &mut Frame, area: Rect, app: &mut SelectorApp) {
    let content_area = pane_content_area(area);
    app.message_page_height = content_area.height.max(1);

    let Some((first_message, date)) = app
        .selected_row()
        .map(|row| (row.first_message.clone(), session_date(row)))
    else {
        app.message_max_scroll = 0;
        app.message_scroll = 0;
        let empty = Paragraph::new("No matching sessions")
            .block(Block::new().title(" First Message ").borders(Borders::ALL));
        frame.render_widget(empty, area);
        return;
    };

    app.message_max_scroll = bottom_scroll_offset(
        message_line_count(&first_message, content_area.width),
        content_area.height,
    );
    app.message_scroll = app.message_scroll.min(app.message_max_scroll);

    let title = if app.focus == PaneFocus::Message {
        format!(" First Message [focus]: {date} ")
    } else {
        format!(" First Message: {date} ")
    };
    let border_style = if app.focus == PaneFocus::Message {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let text = message_text(&first_message);

    let paragraph = Paragraph::new(text)
        .block(
            Block::new()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .wrap(Wrap { trim: false })
        .scroll((app.message_scroll, 0));

    frame.render_widget(paragraph, area);
}

fn message_line_count(message: &str, width: u16) -> usize {
    Paragraph::new(message_text(message))
        .wrap(Wrap { trim: false })
        .line_count(width)
}

fn message_text(message: &str) -> Text<'static> {
    Text::from(
        message
            .lines()
            .map(|line| Line::raw(line.to_string()))
            .collect::<Vec<_>>(),
    )
}

fn render_footer(frame: &mut Frame, area: Rect, app: &SelectorApp) {
    let status = app
        .status
        .as_deref()
        .map(|status| format!(" | {status}"))
        .unwrap_or_default();

    let footer = match app.mode {
        Mode::Normal if app.header_focus => Line::from(vec![
            Span::raw(" focus: "),
            Span::styled("header", Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::raw(" 1-5 column "),
            Span::raw(" h/l ←/→ move "),
            Span::raw(" Enter sort desc/asc/default "),
            Span::raw(" / filter column "),
            Span::raw(" Esc/s/j back "),
            Span::raw(" q quit "),
            Span::raw(" ? help "),
            Span::styled(status, Style::default().fg(Color::Yellow)),
        ]),
        Mode::Normal => Line::from(vec![
            Span::raw(" focus: "),
            Span::styled(app.focus.label(), Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::raw(" j/k ↑/↓ move "),
            Span::raw(" d/u half-page "),
            Span::raw(" g/G first/last "),
            Span::raw(" h/l ←/→ x-scroll "),
            Span::raw(" Tab focus "),
            Span::raw(" / search "),
            Span::raw(" s header "),
            Span::raw(" Enter replay "),
            Span::raw(" i details "),
            Span::raw(" e exec "),
            Span::raw(" y copy-resume "),
            Span::raw(" p parent "),
            Span::raw(" q quit "),
            Span::raw(" Esc clear/quit "),
            Span::raw(" Ctrl-C quit "),
            Span::raw(" ? help "),
            Span::styled(status, Style::default().fg(Color::Yellow)),
        ]),
        Mode::Search => Line::from(vec![
            Span::raw(" search: "),
            Span::styled(app.search_target.label(), Style::default().fg(Color::Cyan)),
            Span::raw(" /"),
            Span::styled(app.query.clone(), Style::default().fg(Color::Yellow)),
            Span::raw("  Tab target  Enter accept  Esc cancel  Ctrl-C quit  Backspace delete "),
            Span::styled(status, Style::default().fg(Color::Yellow)),
        ]),
        Mode::ColumnSearch(column) => Line::from(vec![
            Span::raw(" filter: "),
            Span::styled(column.name(), Style::default().fg(Color::Cyan)),
            Span::raw(" /"),
            Span::styled(
                app.column_filters[column.index()].clone(),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw("  Enter accept (empty removes)  Esc cancel  Ctrl-C quit  Backspace delete "),
            Span::styled(status, Style::default().fg(Color::Yellow)),
        ]),
    }
    .gray();

    let legend = Line::from(vec![
        Span::raw(" badges: "),
        Span::styled("+", Style::default().fg(Color::Cyan)),
        Span::raw(" main  "),
        Span::styled("-", Style::default().fg(Color::Yellow)),
        Span::raw(" sub  "),
        Span::styled(">", Style::default().fg(Color::Magenta)),
        Span::raw(" continued"),
    ]);
    frame.render_widget(Paragraph::new(vec![legend, footer]), area);
}

fn render_help(frame: &mut Frame) {
    let area = centered_rect(76, 70, frame.area());
    let help = Paragraph::new(vec![
        Line::styled(
            "Claude Session Selector",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
        Line::raw("Left pane shows modified time, path, branch, repository, and title."),
        Line::raw("Badges: + main, - sub, > continued in another session."),
        Line::raw("Right pane shows the selected session's first user message."),
        Line::raw(""),
        Line::raw("Focus"),
        Line::raw("  Tab             switch focus between sessions/message"),
        Line::raw(""),
        Line::raw("Movement"),
        Line::raw("  j / Down        next session or scroll message down one line"),
        Line::raw("  k / Up          previous session or scroll message up one line"),
        Line::raw("  d / PageDown    move or scroll the focused pane down half a page"),
        Line::raw("  u / PageUp      move or scroll the focused pane up half a page"),
        Line::raw("  g / Home        first session or message top"),
        Line::raw("  G / End         last session or message bottom (last line visible)"),
        Line::raw("  h / Left        horizontal scroll session metadata left"),
        Line::raw("  l / Right       horizontal scroll session metadata right"),
        Line::raw("  0               reset horizontal scroll"),
        Line::raw(""),
        Line::raw("Header (sort and column filters)"),
        Line::raw("  s               focus the header row; s/Esc/j/Down return to rows"),
        Line::raw("  1-5             focus Modified/Path/Branch/Repository/Title"),
        Line::raw("  h/l, Left/Right previous/next column"),
        Line::raw("  Enter           sort the column: desc -> asc -> default"),
        Line::raw("  /               filter Path/Branch/Repository/Title by substring"),
        Line::raw(""),
        Line::raw("Search"),
        Line::raw("  /               interactive search"),
        Line::raw("  Tab             cycle any/content:all/user/agent/exec"),
        Line::raw("  Enter           accept search (an empty column filter is removed)"),
        Line::raw("  Esc             cancel: restore the previous input"),
        Line::raw("  Esc (list)      clear search, then column filters, then sort; then quit"),
        Line::raw(""),
        Line::raw("Other"),
        Line::raw("  i               show full metadata for the selected session"),
        Line::raw("                  j/k, d/u, g/G scroll; i/Esc/q close"),
        Line::raw("  Enter           replay the selected jsonl path"),
        Line::raw("                  return from replay to this selector when replay exits"),
        Line::raw("  e               toggle exec entries for the next replay"),
        Line::raw("  y               copy `cd <cwd> && claude --resume <id>` to clipboard"),
        Line::raw("                  a subsession copies its parent session's command"),
        Line::raw("  p               select the parent session of a subsession"),
        Line::raw("  ?               toggle help"),
        Line::raw("  q / Ctrl-C      quit"),
    ])
    .block(Block::new().title(" Help ").borders(Borders::ALL))
    .wrap(Wrap { trim: false });

    frame.render_widget(Clear, area);
    frame.render_widget(help, area);
}

fn details_text(row: &SessionRow) -> String {
    format!(
        "Date: {}\n\nFile modified (UTC): {}\n\nTitle: {}\n\nWorking directory: {}\n\nBranch: {}\n\nRepository: {}",
        dash_empty(&session_date(row)),
        dash_empty(row.file_modified_at.as_deref().unwrap_or_default()),
        dash_empty(row.title.as_deref().unwrap_or_default()),
        dash_empty(row.cwd.as_deref().unwrap_or_default()),
        dash_empty(row.branch.as_deref().unwrap_or_default()),
        dash_empty(row.repository_url.as_deref().unwrap_or_default()),
    )
}

fn render_details(frame: &mut Frame, app: &mut SelectorApp) {
    let Some(text) = app.selected_row().map(details_text) else {
        return;
    };
    let area = centered_rect(80, 80, frame.area());
    let content = pane_content_area(area);
    app.details_page_height = content.height.max(1);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    app.details_max_scroll =
        bottom_scroll_offset(paragraph.line_count(content.width), content.height);
    app.details_scroll = app.details_scroll.min(app.details_max_scroll);
    let paragraph = paragraph
        .block(
            Block::new()
                .title(" Session details (i/Esc/q close) ")
                .borders(Borders::ALL),
        )
        .scroll((app.details_scroll, 0));
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn metadata_line(row: &SessionRow, show_subsession_badge: bool, scroll: usize) -> Line<'static> {
    let segments = metadata_segments(row, show_subsession_badge);
    scrolled_segments(&segments, scroll)
}

const MODIFIED_WIDTH: usize = 17;
const PATH_WIDTH: usize = 24;
const BRANCH_WIDTH: usize = 12;
const REPOSITORY_WIDTH: usize = 28;

fn row_prefix_width(show_subsession_badge: bool) -> usize {
    if show_subsession_badge { 2 } else { 1 }
}

const SEPARATOR: &str = " | ";

/// Offset of the column from the start of a row, before horizontal scroll.
fn column_start(column: Column, show_subsession_badge: bool) -> usize {
    row_prefix_width(show_subsession_badge)
        + Column::ALL[..column.index()]
            .iter()
            .map(|previous| previous.width().unwrap_or(0) + SEPARATOR.len())
            .sum::<usize>()
}

/// Column headers aligned with `metadata_segments`. A sorted column shows
/// ▼/▲, a filtered one `*`, and the focused one is reversed.
fn header_segments(
    show_subsession_badge: bool,
    sort: Option<(Column, SortDirection)>,
    filters: &ColumnFilters,
    focused: Option<Column>,
) -> Vec<(String, Style)> {
    let mut segments = vec![(
        " ".repeat(row_prefix_width(show_subsession_badge)),
        Style::default(),
    )];
    for (index, column) in Column::ALL.into_iter().enumerate() {
        if index > 0 {
            segments.push((SEPARATOR.to_string(), Style::default().fg(Color::DarkGray)));
        }
        let mut label = column.label().to_string();
        match sort {
            Some((sorted, SortDirection::Desc)) if sorted == column => label.push_str(" ▼"),
            Some((sorted, SortDirection::Asc)) if sorted == column => label.push_str(" ▲"),
            _ => {}
        }
        if !filters[column.index()].is_empty() {
            label.push('*');
        }
        let mut style = Style::default().fg(column.color());
        if focused == Some(column) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let text = match column.width() {
            Some(width) => fit_width(&label, width),
            None => label,
        };
        segments.push((text, style));
    }
    segments
}

fn matches_column_filters(filters: &ColumnFilters, row: &SessionRow) -> bool {
    Column::ALL.into_iter().all(|column| {
        let filter = &filters[column.index()];
        if filter.trim().is_empty() {
            return true;
        }
        let value = column.value(row).to_lowercase();
        filter
            .split_whitespace()
            .all(|term| value.contains(&term.to_lowercase()))
    })
}

/// Case-insensitive order; missing values stay last in both directions.
fn compare_column(
    column: Column,
    direction: SortDirection,
    left: &SessionRow,
    right: &SessionRow,
) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (left, right) = (column.value(left), column.value(right));
    match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            let order = left.to_lowercase().cmp(&right.to_lowercase());
            match direction {
                SortDirection::Desc => order.reverse(),
                SortDirection::Asc => order,
            }
        }
    }
}

fn metadata_segments(row: &SessionRow, show_subsession_badge: bool) -> Vec<(String, Style)> {
    let mut segments = Vec::new();
    if show_subsession_badge {
        let (label, style) = if row.is_subsession {
            ("-", Style::default().fg(Color::Yellow))
        } else {
            ("+", Style::default().fg(Color::Cyan))
        };
        segments.push((label.to_string(), style));
    }
    segments.push((
        if row.continued_in_session_id.is_some() {
            ">"
        } else {
            " "
        }
        .to_string(),
        Style::default().fg(Color::Magenta),
    ));
    segments.extend([
        (
            fixed_column(
                row.file_modified_at.as_deref().unwrap_or_default(),
                MODIFIED_WIDTH,
            ),
            Style::default().fg(Color::Cyan),
        ),
        (" | ".to_string(), Style::default().fg(Color::DarkGray)),
        (
            fixed_column_tail(row.cwd.as_deref().unwrap_or_default(), PATH_WIDTH),
            Style::default().fg(Color::Green),
        ),
        (" | ".to_string(), Style::default().fg(Color::DarkGray)),
        (
            fixed_column_tail(row.branch.as_deref().unwrap_or_default(), BRANCH_WIDTH),
            Style::default().fg(Color::Yellow),
        ),
        (" | ".to_string(), Style::default().fg(Color::DarkGray)),
        (
            fixed_column(
                row.repository_url.as_deref().unwrap_or_default(),
                REPOSITORY_WIDTH,
            ),
            Style::default().fg(Color::Magenta),
        ),
        (" | ".to_string(), Style::default().fg(Color::DarkGray)),
        (
            dash_empty(row.title.as_deref().unwrap_or_default()).to_string(),
            Style::default().fg(Color::White),
        ),
    ]);
    segments
}

// Widths are terminal cells, not bytes or Unicode scalar values.
fn fixed_column(value: &str, width: usize) -> String {
    fit_width(dash_empty(value), width)
}

fn fixed_column_tail(value: &str, width: usize) -> String {
    fit_width_tail(dash_empty(value), width)
}

#[cfg(test)]
fn metadata_plain(row: &SessionRow, show_subsession_badge: bool) -> String {
    metadata_segments(row, show_subsession_badge)
        .into_iter()
        .map(|(text, _)| text)
        .collect::<Vec<_>>()
        .join("")
}

fn scrolled_segments(segments: &[(String, Style)], scroll: usize) -> Line<'static> {
    let mut remaining = scroll;
    let mut spans = Vec::new();

    for (text, style) in segments {
        let char_count = text.chars().count();
        if remaining >= char_count {
            remaining -= char_count;
            continue;
        }

        let visible = text.chars().skip(remaining).collect::<String>();
        remaining = 0;
        if !visible.is_empty() {
            spans.push(Span::styled(visible, *style));
        }
    }

    Line::from(spans)
}

// POSIX sh single quoting: each ' becomes '\''.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

// A subsession has no resumable id of its own, so it resolves to its parent main session.
fn resume_command(row: &SessionRow, all_rows: &[SessionRow]) -> Option<String> {
    let target = if row.is_subsession {
        let parent_id = row.parent_session_id.as_deref()?;
        all_rows.iter().find(|candidate| {
            !candidate.is_subsession && candidate.id.as_deref() == Some(parent_id)
        })?
    } else {
        row
    };
    let id = target.id.as_deref().filter(|id| !id.trim().is_empty())?;
    Some(
        match target.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) {
            Some(cwd) => format!("cd {} && claude --resume {id}", shell_single_quote(cwd)),
            None => format!("claude --resume {id}"),
        },
    )
}

fn dash_empty(value: &str) -> &str {
    if value.trim().is_empty() { "-" } else { value }
}

impl PaneFocus {
    fn label(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::Message => "message",
        }
    }
}

fn format_search_error(error: &anyhow::Error) -> String {
    if let Some(query_error) = error.downcast_ref::<QueryError>() {
        return format!("search query error: {query_error}");
    }
    if is_corruption(error) {
        return format!("search failed: {error}; run `select-claude-session index --rebuild`");
    }
    format!("search failed: {error}; refresh or rebuild the index")
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
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
    use crate::{
        cli::IndexOptions,
        indexer::{build_index, store::SessionView},
        test_support::SessionFixture,
    };
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn render_at(app: &mut SelectorApp, width: u16, height: u16) {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, app)).unwrap();
    }

    fn sample_rows() -> Vec<SessionRow> {
        vec![
            SessionRow {
                path: PathBuf::from("/tmp/a.jsonl"),
                id: None,
                timestamp: Some("2026-05-27T01:00:00Z".to_string()),
                cwd: Some("/repo/alpha".to_string()),
                repository_url: Some("https://git.example/alpha.git".to_string()),
                branch: Some("main".to_string()),
                first_message: "Fix README parser".to_string(),
                is_subsession: false,
                ..SessionRow::default()
            },
            SessionRow {
                path: PathBuf::from("/tmp/b.jsonl"),
                id: None,
                timestamp: Some("2026-05-28T01:00:00Z".to_string()),
                cwd: Some("/repo/beta".to_string()),
                repository_url: Some("https://git.example/beta.git".to_string()),
                branch: Some("feature/search".to_string()),
                first_message: "add selector".to_string(),
                is_subsession: false,
                ..SessionRow::default()
            },
        ]
    }

    fn sample_app(visibility: ExecVisibility) -> (SessionFixture, SelectorApp) {
        let fixture = SessionFixture::new();
        fixture.write_named_session("a.jsonl", "Fix README parser", false);
        fixture.write_named_session("b.jsonl", "add selector", false);
        let db = fixture.path("index.sqlite3");
        build_index(&IndexOptions {
            output: db.clone(),
            sessions_root: fixture.sessions_root(),
            rebuild: false,
            quick_check: false,
            include_subsessions: false,
            include_empty_messages: false,
            include_exec: false,
        })
        .unwrap();
        let index = SearchIndex::open(&db, SessionView::default()).unwrap();
        let app = SelectorApp::new(index, visibility).unwrap();
        (fixture, app)
    }

    #[test]
    fn metadata_columns_align_with_badges_and_long_values() {
        use unicode_width::UnicodeWidthStr;
        let mut rows = sample_rows();
        rows[0].continued_in_session_id = Some("next".into());
        rows[0].cwd = Some("/a/very/long/한글/path/that/exceeds/the/column".into());
        rows[0].title = Some("A long title".into());
        rows[1].is_subsession = true;
        for show_subsessions in [false, true] {
            let first = metadata_plain(&rows[0], show_subsessions);
            let second = metadata_plain(&rows[1], show_subsessions);
            let positions = |line: &str| {
                line.match_indices(" | ")
                    .map(|(i, _)| UnicodeWidthStr::width(&line[..i]))
                    .collect::<Vec<_>>()
            };
            assert_eq!(positions(&first), positions(&second));
            assert!(first.starts_with(if show_subsessions { "+>" } else { ">" }));
            assert!(first.contains('…'));
            assert!(first.ends_with(" | A long title"));
            assert!(second.starts_with(if show_subsessions { "- " } else { " " }));
            if show_subsessions {
                assert!(first.starts_with("+>"));
                assert!(second.starts_with("- "));
            }
        }
    }

    #[test]
    fn file_modified_time_follows_the_date_column() {
        let (_fixture, app) = sample_app(ExecVisibility::Hidden);
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        for row in &app.filtered {
            let modified = std::fs::metadata(&row.path)
                .unwrap()
                .modified()
                .unwrap()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64;
            let expected: String = conn
                .query_row(
                    "SELECT strftime('%Y-%m-%dT%H:%MZ', ?1, 'unixepoch')",
                    [modified],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(row.file_modified_at.as_deref(), Some(expected.as_str()));
            let line = metadata_plain(row, false);
            assert!(line.starts_with(&format!(" {expected} | ")), "{line}");
            assert!(details_text(row).contains(&format!("File modified (UTC): {expected}")));
        }
    }

    #[test]
    fn selector_app_shows_badges_only_for_an_include_subsessions_view() {
        let fixture = SessionFixture::new();
        fixture.write_named_session("main.jsonl", "main task", false);
        fixture.write_named_session("sub.jsonl", "sub task", true);
        let db = fixture.path("index.sqlite3");
        build_index(&IndexOptions {
            output: db.clone(),
            sessions_root: fixture.sessions_root(),
            rebuild: false,
            quick_check: false,
            include_subsessions: false,
            include_empty_messages: false,
            include_exec: false,
        })
        .unwrap();

        let default_index = SearchIndex::open(&db, SessionView::default()).unwrap();
        let default_app = SelectorApp::new(default_index, ExecVisibility::Hidden).unwrap();
        assert!(!default_app.show_subsession_badges);

        let included_index = SearchIndex::open(
            &db,
            SessionView {
                include_subsessions: true,
                include_empty_messages: false,
            },
        )
        .unwrap();
        let included_app = SelectorApp::new(included_index, ExecVisibility::Hidden).unwrap();
        assert!(included_app.show_subsession_badges);
    }

    #[test]
    fn search_target_cycles_through_content_scopes() {
        let scopes = [
            SearchTarget::Any,
            SearchTarget::Content(ContentScope::All),
            SearchTarget::Content(ContentScope::User),
            SearchTarget::Content(ContentScope::Agent),
            SearchTarget::Content(ContentScope::Exec),
            SearchTarget::Any,
        ];

        for pair in scopes.windows(2) {
            assert_eq!(pair[0].next(), pair[1]);
        }
    }

    #[test]
    fn search_target_labels_are_visible() {
        assert_eq!(SearchTarget::Any.label(), "any");
        assert_eq!(
            SearchTarget::Content(ContentScope::Exec).label(),
            "content:exec"
        );
    }

    /// Three sessions with distinct sortable values; the order is the default.
    fn header_app() -> (SessionFixture, SelectorApp) {
        let (fixture, mut app) = sample_app(ExecVisibility::Hidden);
        let template = app.search_rows[0].clone();
        let row = |path: &str, cwd: &str, branch: Option<&str>| SessionRow {
            path: PathBuf::from(path),
            cwd: Some(cwd.to_string()),
            branch: branch.map(str::to_string),
            ..template.clone()
        };
        app.search_rows = vec![
            row("/tmp/1.jsonl", "/repo/beta", Some("main")),
            row("/tmp/2.jsonl", "/repo/alpha", None),
            row("/tmp/3.jsonl", "/repo/Gamma", Some("feat/ocr")),
        ];
        app.all_rows = app.search_rows.clone();
        app.apply_view(false);
        (fixture, app)
    }

    fn paths(app: &SelectorApp) -> Vec<String> {
        app.filtered
            .iter()
            .map(|row| row.path.file_stem().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn type_text(app: &mut SelectorApp, text: &str) {
        for ch in text.chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
    }

    #[test]
    fn s_enters_the_header_and_number_or_h_l_move_the_column_focus() {
        let (_fixture, mut app) = header_app();
        app.focus = PaneFocus::Message;
        app.handle_key(key(KeyCode::Char('s')));
        assert!(app.header_focus);
        assert_eq!(app.focus, PaneFocus::Sessions);
        assert_eq!(app.header_column, Column::Modified);

        app.handle_key(key(KeyCode::Char('3')));
        assert_eq!(app.header_column, Column::Branch);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.header_column, Column::Repository);
        app.handle_key(key(KeyCode::Char('5')));
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.header_column, Column::Title);
        app.handle_key(key(KeyCode::Char('1')));
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.header_column, Column::Modified);

        // j in the header returns to the rows without moving the selection.
        app.handle_key(key(KeyCode::Char('j')));
        assert!(!app.header_focus);
        assert_eq!(app.selected_index(), Some(0));
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.header_column, Column::Modified, "column is remembered");
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.header_focus);
    }

    #[test]
    fn enter_cycles_desc_asc_default_and_keeps_the_selected_session() {
        let (_fixture, mut app) = header_app();
        app.list_state.select(Some(1)); // "2" (/repo/alpha)
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('2')));

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.sort, Some((Column::Path, SortDirection::Desc)));
        assert_eq!(paths(&app), ["3", "1", "2"], "case-insensitive");
        assert_eq!(
            app.selected_row().unwrap().path,
            PathBuf::from("/tmp/2.jsonl")
        );

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(paths(&app), ["2", "1", "3"]);

        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.sort, None);
        assert_eq!(paths(&app), ["1", "2", "3"]);
        assert_eq!(
            app.selected_row().unwrap().path,
            PathBuf::from("/tmp/2.jsonl")
        );
    }

    #[test]
    fn missing_values_sort_last_in_both_directions() {
        let (_fixture, mut app) = header_app();
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('3')));
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(paths(&app), ["1", "3", "2"]);
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(paths(&app), ["3", "1", "2"]);
    }

    #[test]
    fn column_filter_narrows_the_list_and_esc_restores_the_previous_value() {
        let (_fixture, mut app) = header_app();
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('2')));
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::ColumnSearch(Column::Path));

        type_text(&mut app, "ALP");
        assert_eq!(paths(&app), ["2"], "filters while typing");
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.header_focus, "back to the header after a filter");
        assert_eq!(app.column_filters[Column::Path.index()], "ALP");

        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "ha zzz");
        assert!(app.filtered.is_empty());
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.column_filters[Column::Path.index()], "ALP");
        assert_eq!(paths(&app), ["2"]);

        app.handle_key(key(KeyCode::Char('/')));
        for _ in 0..3 {
            app.handle_key(key(KeyCode::Backspace));
        }
        app.handle_key(key(KeyCode::Enter));
        assert_eq!(app.column_filters[Column::Path.index()], "");
        assert_eq!(paths(&app), ["1", "2", "3"]);
    }

    #[test]
    fn column_filters_combine_with_and() {
        let (_fixture, mut app) = header_app();
        app.column_filters[Column::Path.index()] = "repo".into();
        app.column_filters[Column::Branch.index()] = "ma".into();
        app.apply_view(false);
        assert_eq!(paths(&app), ["1"], "a missing branch never matches");
    }

    #[test]
    fn modified_column_is_sortable_but_not_searchable() {
        let (_fixture, mut app) = header_app();
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('/')));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.status.as_deref(),
            Some("modified is not searchable; Enter sorts it")
        );
    }

    #[test]
    fn esc_in_the_list_clears_search_then_filters_then_sort_then_quits() {
        let (_fixture, mut app) = header_app();
        app.query = "x".into();
        app.column_filters[Column::Branch.index()] = "main".into();
        app.sort = Some((Column::Path, SortDirection::Asc));

        assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
        assert!(app.query.is_empty());
        assert_eq!(app.column_filters[Column::Branch.index()], "main");
        assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
        assert!(app.column_filters.iter().all(String::is_empty));
        assert!(app.sort.is_some());
        assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
        assert_eq!(app.sort, None);
        assert_eq!(
            app.handle_key(key(KeyCode::Esc)),
            Some(SelectorAction::Quit)
        );
    }

    #[test]
    fn esc_cancels_a_global_search_and_restores_query_and_target() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.handle_key(key(KeyCode::Char('/')));
        type_text(&mut app, "readme");
        app.handle_key(key(KeyCode::Enter));
        let accepted = app.filtered.clone();
        assert_eq!(accepted.len(), 1);

        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Tab));
        type_text(&mut app, " zzz");
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.query, "readme");
        assert_eq!(app.search_target, SearchTarget::Any);
        assert_eq!(app.filtered, accepted);
    }

    #[test]
    fn focusing_a_column_scrolls_it_into_view() {
        let (_fixture, mut app) = header_app();
        render_at(&mut app, 100, 12);
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('5')));
        let view = usize::from(app.sessions_view_width);
        let start = column_start(Column::Title, false);
        assert!(app.metadata_scroll > 0);
        assert!(start >= app.metadata_scroll && start + 7 <= app.metadata_scroll + view);
        app.handle_key(key(KeyCode::Char('1')));
        assert_eq!(app.metadata_scroll, 0);
    }

    #[test]
    fn header_shows_sort_arrow_filter_mark_and_focus() {
        let mut filters = ColumnFilters::default();
        filters[Column::Branch.index()] = "feat".into();
        let segments = header_segments(
            false,
            Some((Column::Repository, SortDirection::Asc)),
            &filters,
            Some(Column::Path),
        );
        let text: String = segments.iter().map(|(t, _)| t.as_str()).collect();
        assert!(text.contains("| Branch*      |"), "{text}");
        assert!(text.contains("| Repository ▲ "), "{text}");
        let path = segments
            .iter()
            .find(|(t, _)| t.starts_with("Path"))
            .unwrap();
        assert!(path.1.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn header_columns_align_with_row_columns() {
        let rows = sample_rows();
        for badge in [false, true] {
            let header: String = header_segments(badge, None, &ColumnFilters::default(), None)
                .into_iter()
                .map(|(t, _)| t)
                .collect();
            let row = metadata_plain(&rows[0], badge);
            let separators = |line: &str| {
                line.match_indices(" | ")
                    .map(|(i, _)| unicode_width::UnicodeWidthStr::width(&line[..i]))
                    .take(4)
                    .collect::<Vec<_>>()
            };
            assert_eq!(separators(&header), separators(&row), "{header}\n{row}");
            assert!(header.trim_start().starts_with("Modified"));
        }
    }

    #[test]
    fn scrolled_segments_preserve_text_after_horizontal_offset() {
        let rows = sample_rows();
        let line = scrolled_segments(&metadata_segments(&rows[1], false), 1 + MODIFIED_WIDTH + 3);
        let text = line
            .spans
            .into_iter()
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("");

        assert!(text.starts_with("/repo/beta"));
    }

    fn main_row(id: &str, cwd: Option<&str>) -> SessionRow {
        SessionRow {
            path: PathBuf::from(format!("/tmp/{id}.jsonl")),
            id: Some(id.to_string()),
            cwd: cwd.map(str::to_string),
            first_message: "main".to_string(),
            ..SessionRow::default()
        }
    }

    fn sub_row(id: &str, parent: &str) -> SessionRow {
        SessionRow {
            path: PathBuf::from(format!("/tmp/{parent}/subagents/agent-{id}.jsonl")),
            id: Some(id.to_string()),
            cwd: Some("/elsewhere".to_string()),
            first_message: "sub".to_string(),
            is_subsession: true,
            parent_session_id: Some(parent.to_string()),
            ..SessionRow::default()
        }
    }

    #[test]
    fn resume_command_changes_directory_first() {
        let row = main_row("main-1", Some("/tmp/fixture"));
        assert_eq!(
            resume_command(&row, &[]).as_deref(),
            Some("cd '/tmp/fixture' && claude --resume main-1")
        );
    }

    #[test]
    fn resume_command_quotes_single_quotes_in_cwd() {
        let row = main_row("main-1", Some("/tmp/it's"));
        assert_eq!(
            resume_command(&row, &[]).as_deref(),
            Some(r"cd '/tmp/it'\''s' && claude --resume main-1")
        );
    }

    #[test]
    fn resume_command_without_cwd_omits_cd() {
        let row = main_row("main-1", None);
        assert_eq!(
            resume_command(&row, &[]).as_deref(),
            Some("claude --resume main-1")
        );

        let mut no_id = row;
        no_id.id = None;
        assert_eq!(resume_command(&no_id, &[]), None);
    }

    #[test]
    fn subsession_resume_command_targets_the_parent() {
        let rows = vec![
            main_row("main-1", Some("/tmp/fixture")),
            sub_row("a1", "main-1"),
        ];
        assert_eq!(
            resume_command(&rows[1], &rows).as_deref(),
            Some("cd '/tmp/fixture' && claude --resume main-1")
        );
    }

    #[test]
    fn subsession_resume_command_needs_the_parent_row() {
        let rows = vec![sub_row("a1", "main-1")];
        assert_eq!(resume_command(&rows[0], &rows), None);
    }

    fn parent_app(query: &str) -> (SessionFixture, SelectorApp) {
        let fixture = SessionFixture::new();
        fixture.write_named_session("parent-session.jsonl", "main task", false);
        fixture.write_named_session("sub.jsonl", "sub task", true);
        let db = fixture.path("index.sqlite3");
        build_index(&IndexOptions {
            output: db.clone(),
            sessions_root: fixture.sessions_root(),
            rebuild: false,
            quick_check: false,
            include_subsessions: false,
            include_empty_messages: false,
            include_exec: false,
        })
        .unwrap();
        let index = SearchIndex::open(
            &db,
            SessionView {
                include_subsessions: true,
                include_empty_messages: false,
            },
        )
        .unwrap();
        let mut app = SelectorApp::new(index, ExecVisibility::Hidden).unwrap();
        app.query = query.to_string();
        app.refresh_filter();
        let sub = app
            .filtered
            .iter()
            .position(|row| row.is_subsession)
            .unwrap();
        app.list_state.select(Some(sub));
        (fixture, app)
    }

    #[test]
    fn p_moves_selection_to_the_parent_row() {
        let (_fixture, mut app) = parent_app("");

        assert_eq!(app.handle_key(key(KeyCode::Char('p'))), None);
        let selected = app.selected_row().unwrap();
        assert!(!selected.is_subsession);
        assert_eq!(selected.id.as_deref(), Some("parent-session"));
        assert_eq!(app.status, None);
    }

    #[test]
    fn p_on_main_row_reports_no_parent() {
        let (_fixture, mut app) = parent_app("");
        app.handle_key(key(KeyCode::Char('p')));
        let selected = app.selected_index();

        app.handle_key(key(KeyCode::Char('p')));
        assert_eq!(app.jump_to_parent(), ParentJump::NotSubsession);
        assert_eq!(app.selected_index(), selected);
        assert_eq!(
            app.status.as_deref(),
            Some("selected session has no parent")
        );
    }

    #[test]
    fn p_reports_parent_outside_the_filtered_list() {
        let (_fixture, mut app) = parent_app("sub");
        let selected = app.selected_index();

        app.handle_key(key(KeyCode::Char('p')));
        assert_eq!(app.selected_index(), selected);
        assert_eq!(app.query, "sub");
        assert_eq!(
            app.status.as_deref(),
            Some("parent session is not in the current list")
        );
    }

    #[test]
    fn p_is_search_text_while_searching() {
        let (_fixture, mut app) = parent_app("");
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('p')));

        assert_eq!(app.query, "p");
        assert_eq!(app.mode, Mode::Search);
    }

    #[test]
    fn selector_initial_visibility_matches_cli_state() {
        let (_hidden_fixture, hidden) = sample_app(ExecVisibility::Hidden);
        let (_shown_fixture, shown) = sample_app(ExecVisibility::Shown);

        assert_eq!(hidden.exec_visibility(), ExecVisibility::Hidden);
        assert_eq!(shown.exec_visibility(), ExecVisibility::Shown);
        assert_eq!(hidden.filtered.len(), 2);
        assert_eq!(shown.filtered.len(), 2);
    }

    #[test]
    fn message_half_page_and_end_follow_the_rendered_pane_height() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.filtered[0].first_message = (1..=30)
            .map(|line| format!("line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.focus = PaneFocus::Message;

        render_at(&mut app, 100, 18);
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.message_scroll, 6);

        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.message_scroll, 17);
    }

    #[test]
    fn sessions_half_page_follows_the_rendered_pane_height() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.filtered = vec![app.filtered[0].clone(); 20];
        app.list_state.select(Some(0));

        render_at(&mut app, 100, 18);
        app.handle_key(key(KeyCode::Char('d')));
        assert_eq!(app.selected_index(), Some(6));

        app.handle_key(key(KeyCode::Char('u')));
        assert_eq!(app.selected_index(), Some(0));

        app.handle_key(key(KeyCode::Char('G')));
        render_at(&mut app, 100, 18);
        assert_eq!(app.selected_index(), Some(19));
        // One row of the pane is the column header.
        assert_eq!(app.list_state.offset(), 8);
    }

    #[test]
    fn selector_e_toggles_only_in_normal_mode() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);

        assert_eq!(app.handle_key(key(KeyCode::Char('e'))), None);
        assert_eq!(app.exec_visibility(), ExecVisibility::Shown);

        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('e')));
        assert_eq!(app.exec_visibility(), ExecVisibility::Shown);
        assert_eq!(app.query, "e");

        app.handle_key(key(KeyCode::Enter));
        app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL));
        assert_eq!(app.exec_visibility(), ExecVisibility::Shown);
    }

    #[test]
    fn details_show_full_values_and_scroll_without_changing_selection() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        let long_path = format!("/{}", "long-directory/".repeat(30));
        app.filtered[0].cwd = Some(long_path.clone());
        assert!(details_text(&app.filtered[0]).contains(&long_path));
        app.handle_key(key(KeyCode::Char('i')));
        assert!(app.show_details);
        render_at(&mut app, 50, 15);
        assert!(app.details_max_scroll > 0);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.details_scroll, app.details_max_scroll);
        assert_eq!(app.selected_index(), Some(0));
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.details_scroll, 0);
        app.handle_key(key(KeyCode::Esc));
        assert!(!app.show_details);
        app.list_state.select(None);
        app.handle_key(key(KeyCode::Char('i')));
        assert!(!app.show_details);
    }

    #[test]
    fn selector_help_is_modal_for_exec_toggle() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        let selected = app.selected_index();
        let focus = app.focus;

        app.handle_key(key(KeyCode::Char('?')));
        assert!(app.show_help);
        for code in [KeyCode::Char('e'), KeyCode::Char('j'), KeyCode::Tab] {
            assert_eq!(app.handle_key(key(code)), None);
        }
        assert_eq!(app.exec_visibility(), ExecVisibility::Hidden);
        assert_eq!(app.selected_index(), selected);
        assert_eq!(app.focus, focus);

        assert_eq!(app.handle_key(key(KeyCode::Esc)), None);
        assert!(!app.show_help);
    }

    #[test]
    fn selector_toggle_preserves_existing_state() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.list_state.select(Some(1));
        app.query = "selector".to_string();
        app.search_target = SearchTarget::Content(ContentScope::Agent);
        app.focus = PaneFocus::Message;
        app.metadata_scroll = 8;
        app.message_scroll = 5;
        app.status = Some("transient error".to_string());
        let total_rows = app.total_rows;
        let filtered = app.filtered.clone();

        app.handle_key(key(KeyCode::Char('e')));

        assert_eq!(app.exec_visibility(), ExecVisibility::Shown);
        assert_eq!(app.total_rows, total_rows);
        assert_eq!(app.filtered, filtered);
        assert_eq!(app.selected_index(), Some(1));
        assert_eq!(app.query, "selector");
        assert_eq!(
            app.search_target,
            SearchTarget::Content(ContentScope::Agent)
        );
        assert_eq!(app.focus, PaneFocus::Message);
        assert_eq!(app.metadata_scroll, 8);
        assert_eq!(app.message_scroll, 5);
        assert_eq!(app.status, None);
    }

    #[test]
    fn selector_toggle_is_safe_with_empty_search_result() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.query = "definitely-not-present".to_string();
        app.refresh_filter();
        app.mode = Mode::Normal;

        app.handle_key(key(KeyCode::Char('e')));

        assert!(app.filtered.is_empty());
        assert_eq!(app.selected_index(), None);
        assert_eq!(app.exec_visibility(), ExecVisibility::Shown);
    }

    #[test]
    fn query_error_preserves_previous_results_and_selection() {
        let (_fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.search_target = SearchTarget::Content(ContentScope::All);
        app.query = "fix".to_owned();
        app.refresh_filter();
        let filtered = app.filtered.clone();
        let selected = app.selected_index();
        app.message_scroll = 7;

        app.query = "---".to_owned();
        app.refresh_filter();

        assert_eq!(app.filtered, filtered);
        assert_eq!(app.selected_index(), selected);
        assert_eq!(app.message_scroll, 7);
        assert_eq!(
            app.status.as_deref(),
            Some("search query error: query contains no searchable token")
        );
    }

    #[test]
    fn database_error_preserves_previous_results_and_selection() {
        let (fixture, mut app) = sample_app(ExecVisibility::Hidden);
        app.search_target = SearchTarget::Content(ContentScope::All);
        let filtered = app.filtered.clone();
        let selected = app.selected_index();
        app.message_scroll = 4;
        let conn = rusqlite::Connection::open(fixture.path("index.sqlite3")).unwrap();
        conn.execute(
            "UPDATE sessions SET first_message = first_message || ' external'",
            [],
        )
        .unwrap();
        drop(conn);

        app.query = "fix".to_owned();
        app.refresh_filter();

        assert_eq!(app.filtered, filtered);
        assert_eq!(app.selected_index(), selected);
        assert_eq!(app.message_scroll, 4);
        assert!(
            app.status
                .as_deref()
                .unwrap()
                .starts_with("search failed: FTS index is dirty")
        );
    }
}
