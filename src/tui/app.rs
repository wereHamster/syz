use crate::core::event::Op;
use crate::core::message::Payload;
use crate::tui::views::bump_detail::BumpDetailView;
use crate::tui::views::bumps::BumpsView;
use crate::tui::views::overview::OverviewView;
use crate::tui::views::project::ProjectView;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, TableState},
};
use serde_json::Value;
use std::collections::HashMap;

pub struct Backend {
    /// The last 100 log messages that the server has sent.
    pub logs: Vec<String>,

    /// The complete mirror of the server's database.
    pub db: HashMap<String, Value>,
}

impl Backend {
    pub fn new() -> Self {
        Self {
            logs: Vec::new(),
            db: HashMap::new(),
        }
    }

    pub fn process_server_event(&mut self, event: crate::core::event::Event) {
        match event {
            crate::core::event::Event::Trace { level: _, message } => {
                self.logs.push(message);

                if self.logs.len() > 100 {
                    self.logs.remove(0);
                }
            }
            crate::core::event::Event::Commit { ops } => {
                for op in ops {
                    match op {
                        Op::Upsert { path, data } => {
                            self.db.insert(path, data);
                        }
                        Op::Delete { path } => {
                            self.db.remove(&path);
                        }
                    }
                }
            }
        }
    }
}

pub enum ViewType {
    /// Overview showing all projects.
    Overview,

    /// Bumps showing all bumps grouped by name, across all projects.
    Bumps,

    /// Project view showing the details of a specific project.
    Project(String),

    /// Bump detail view showing the projects affected by a specific bump group.
    BumpDetail { name: String, major: bool },
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub enum Focus {
    /// Keyboard focus is on the top-level tab bar.
    TabBar,

    /// Keyboard focus is on the active view's content.
    Content,
}

pub enum ViewAction {
    /// Switch the currently active view.
    SwitchView(ViewType),

    /// Send a command to the server.
    SendPayload(Payload),
}

pub enum Effect {
    /// A side-effect that must be handled by the main loop.
    SendPayload(Payload),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeyDescriptor {
    /// The key (or sequence of keys) that triggers this hotkey.
    pub key: String,

    /// A human-readable description of what this hotkey does.
    pub description: String,
}

impl HotkeyDescriptor {
    pub fn render(&self) -> Vec<Span<'static>> {
        vec![
            Span::styled(
                self.key.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", self.description),
                Style::default().fg(Color::Gray),
            ),
        ]
    }
}

pub trait View {
    /// Handles user input and returns a list of side-effects (e.g., sending payloads or switching views).
    fn update(&mut self, event: &Event, backend: &Backend) -> Vec<ViewAction>;

    /// Renders the view's content into the provided terminal area. `focused` indicates
    /// whether keyboard focus is on this view's content (vs. e.g. the tab bar), and
    /// should be used to decide whether to show the selected-row highlight.
    fn draw(&mut self, frame: &mut Frame, backend: &Backend, area: Rect, focused: bool);

    /// Returns a list of hotkeys supported by this view for the UI footer.
    fn hotkeys(&self) -> Vec<HotkeyDescriptor>;
}

#[derive(Eq, PartialEq)]
pub enum Lifecycle {
    Running,
    Exiting,
}

pub struct App {
    pub lifecycle: Lifecycle,
    pub backend: Backend,
    pub current_view_type: ViewType,
    pub focus: Focus,
    pub overview_view: OverviewView,
    pub project_view: ProjectView,
    pub bumps_view: BumpsView,
    pub bump_detail_view: BumpDetailView,
    pub dirty: bool,
}

pub enum Event {
    Term(crossterm::event::Event),
    Core(crate::core::event::Event),
    Draw,
}

impl App {
    pub fn new() -> Self {
        Self {
            lifecycle: Lifecycle::Running,
            backend: Backend::new(),
            current_view_type: ViewType::Overview,
            focus: Focus::Content,
            overview_view: OverviewView::new(),
            project_view: ProjectView::new("".to_string()),
            bumps_view: BumpsView::new(),
            bump_detail_view: BumpDetailView::new("".to_string(), false),
            dirty: false,
        }
    }

    pub fn update(&mut self, event: Event) -> Vec<Effect> {
        let mut external_effects = Vec::new();
        match event {
            Event::Term(ref term_event) => {
                if let crossterm::event::Event::Key(key) = term_event {
                    match key.code {
                        KeyCode::Char('q') => {
                            self.lifecycle = Lifecycle::Exiting;
                        }
                        KeyCode::Tab => {
                            self.focus = match self.focus {
                                Focus::TabBar => Focus::Content,
                                Focus::Content => Focus::TabBar,
                            };
                            self.dirty = true;
                        }
                        KeyCode::Left
                            if self.focus == Focus::Content
                                && matches!(
                                    self.current_view_type,
                                    ViewType::Overview | ViewType::Bumps
                                ) =>
                        {
                            self.focus = Focus::TabBar;
                            self.dirty = true;
                        }
                        _ if self.focus == Focus::TabBar => {
                            match key.code {
                                KeyCode::Left => {
                                    self.current_view_type = ViewType::Overview;
                                }
                                KeyCode::Right => {
                                    self.current_view_type = ViewType::Bumps;
                                }
                                KeyCode::Down => {
                                    self.focus = Focus::Content;
                                }
                                _ => {}
                            }
                            self.dirty = true;
                        }
                        _ => {
                            let actions = match self.current_view_type {
                                ViewType::Overview => {
                                    self.overview_view.update(&event, &self.backend)
                                }
                                ViewType::Bumps => self.bumps_view.update(&event, &self.backend),
                                ViewType::Project(_) => {
                                    self.project_view.update(&event, &self.backend)
                                }
                                ViewType::BumpDetail { .. } => {
                                    self.bump_detail_view.update(&event, &self.backend)
                                }
                            };

                            for action in actions {
                                match action {
                                    ViewAction::SwitchView(new_view) => {
                                        self.current_view_type = new_view;
                                        if let ViewType::Project(id) = &self.current_view_type {
                                            if id != &self.project_view.project_id {
                                                self.project_view.project_id = id.clone();
                                                self.project_view.selected_bump_index = 0;
                                                self.project_view.bump_table_state =
                                                    TableState::default();
                                                self.project_view.bump_table_state.select(Some(0));
                                            }
                                        }
                                        if let ViewType::BumpDetail { name, major } =
                                            &self.current_view_type
                                        {
                                            if name != &self.bump_detail_view.name
                                                || *major != self.bump_detail_view.major
                                            {
                                                self.bump_detail_view.name = name.clone();
                                                self.bump_detail_view.major = *major;
                                                self.bump_detail_view.selected_index = 0;
                                                self.bump_detail_view.table_state =
                                                    TableState::default();
                                                self.bump_detail_view.table_state.select(Some(0));
                                            }
                                        }
                                    }
                                    ViewAction::SendPayload(payload) => {
                                        external_effects.push(Effect::SendPayload(payload));
                                    }
                                }
                            }
                            self.dirty = true;
                        }
                    }
                }
            }

            Event::Core(core_event) => {
                self.backend.process_server_event(core_event);
            }

            Event::Draw => {
                self.dirty = true;
            }
        }
        external_effects
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Tab bar
                Constraint::Length(1), // line below tab bar
                Constraint::Min(0),    // Top area
                Constraint::Length(1), // Blank line above Logs
                Constraint::Length(8), // 1 (line) + 1 (header) + 1 (blank) + 4 (logs) + 1 (blank)
                Constraint::Length(1), // line above hotkeys
                Constraint::Length(1), // hotkeys
            ])
            .split(area);

        let tab_bar_area = chunks[0];
        let line_below_tab_bar_area = chunks[1];
        let top_area = chunks[2];
        let log_section_area = chunks[4];
        let line_above_hotkeys_area = chunks[5];
        let hotkeys_area = chunks[6];

        // 1. Tab bar
        let active_tab = match self.current_view_type {
            ViewType::Overview | ViewType::Project(_) => "Projects",
            ViewType::Bumps | ViewType::BumpDetail { .. } => "Bumps",
        };

        let tab_style = |label: &str| {
            if label != active_tab {
                return Style::default().fg(Color::Gray);
            }
            let style = Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD);
            if self.focus == Focus::TabBar {
                style.bg(Color::Blue)
            } else {
                style
            }
        };

        let tab_line = Line::from(vec![
            Span::styled(" Projects ", tab_style("Projects")),
            Span::raw(" "),
            Span::styled(" Bumps ", tab_style("Bumps")),
        ]);
        frame.render_widget(Paragraph::new(tab_line), tab_bar_area);

        frame.render_widget(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
            line_below_tab_bar_area,
        );

        // 2. Top area
        let content_focused = self.focus == Focus::Content;
        match self.current_view_type {
            ViewType::Overview => {
                self.overview_view
                    .draw(frame, &self.backend, top_area, content_focused)
            }
            ViewType::Bumps => {
                self.bumps_view
                    .draw(frame, &self.backend, top_area, content_focused)
            }
            ViewType::Project(_) => {
                self.project_view
                    .draw(frame, &self.backend, top_area, content_focused)
            }
            ViewType::BumpDetail { .. } => {
                self.bump_detail_view
                    .draw(frame, &self.backend, top_area, content_focused)
            }
        }

        // 3. Log section
        let log_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(4),
                Constraint::Length(1),
            ])
            .split(log_section_area);

        // Render log line
        frame.render_widget(
            Block::default()
                .borders(Borders::BOTTOM)
                .border_style(Style::default().fg(Color::DarkGray)),
            log_chunks[0],
        );

        // Render log header
        frame.render_widget(
            Paragraph::new("Log").style(Style::default().add_modifier(Modifier::BOLD)),
            log_chunks[1],
        );

        // Render log content
        let log_lines: Vec<&str> = self
            .backend
            .logs
            .iter()
            .rev()
            .take(4)
            .rev()
            .map(|s| s.as_str())
            .collect();
        let log_text = log_lines.join("\n");
        frame.render_widget(
            Paragraph::new(log_text).style(Style::default().fg(Color::Gray)),
            log_chunks[3],
        );

        // 4. Line above hotkeys
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
            line_above_hotkeys_area,
        );

        // 5. Hotkeys
        let mut hotkey_spans = Vec::new();

        let quit_hotkey = HotkeyDescriptor {
            key: "Q".to_string(),
            description: "Quit".to_string(),
        };
        hotkey_spans.extend(quit_hotkey.render());

        hotkey_spans.push(Span::from("    "));
        let tab_hotkey = HotkeyDescriptor {
            key: "Tab".to_string(),
            description: "Switch Focus".to_string(),
        };
        hotkey_spans.extend(tab_hotkey.render());

        let view_hotkeys = match self.current_view_type {
            ViewType::Overview => self.overview_view.hotkeys(),
            ViewType::Bumps => self.bumps_view.hotkeys(),
            ViewType::Project(_) => self.project_view.hotkeys(),
            ViewType::BumpDetail { .. } => self.bump_detail_view.hotkeys(),
        };

        for hotkey in view_hotkeys {
            hotkey_spans.push(Span::from("    "));
            hotkey_spans.extend(hotkey.render());
        }

        let hotkey_text = Line::from(hotkey_spans);
        frame.render_widget(Paragraph::new(hotkey_text), hotkeys_area);
    }
}
