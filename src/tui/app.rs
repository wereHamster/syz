use crate::core::database::{Bump, BumpDep, Dependency, Package, Project};
use crate::core::event::Op;
use crate::core::message::Payload;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState},
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

pub enum Effect {
    SendPayload(Payload),
}

pub enum View {
    /// Overview showing all projects.
    Overview,

    /// Project view showing the details of a specific project.
    Project(String),
}

#[derive(Eq, PartialEq)]
pub enum Lifecycle {
    Running,
    Exiting,
}

pub struct App {
    pub lifecycle: Lifecycle,
    pub backend: Backend,
    pub view: View,

    pub dirty: bool,
    pub selected_index: usize,
    pub table_state: TableState,
    pub selected_bump_index: usize,
    pub bump_table_state: TableState,
}

pub enum Event {
    Term(crossterm::event::Event),
    Core(crate::core::event::Event),
    Draw,
}

impl App {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            lifecycle: Lifecycle::Running,
            backend: Backend::new(),
            view: View::Overview,
            dirty: false,
            selected_index: 0,
            table_state,
            selected_bump_index: 0,
            bump_table_state: TableState::default(),
        }
    }

    pub fn update(&mut self, event: Event) -> Vec<Effect> {
        let mut effects = Vec::new();
        match event {
            Event::Term(event) => match event {
                crossterm::event::Event::Key(key) => match key.code {
                    KeyCode::Char('q') => {
                        self.lifecycle = Lifecycle::Exiting;
                    }
                    KeyCode::Up => {
                        if let View::Overview = self.view {
                            let projects_count = self
                                .backend
                                .db
                                .iter()
                                .filter(|(path, _)| path.starts_with("project/"))
                                .count();
                            if projects_count > 0 {
                                if self.selected_index > 0 {
                                    self.selected_index -= 1;
                                } else {
                                    self.selected_index = projects_count - 1;
                                }
                                self.table_state.select(Some(self.selected_index));
                            }
                        } else if let View::Project(_) = self.view {
                            let mut bumps: Vec<Bump> = self
                                .backend
                                .db
                                .iter()
                                .filter(|(path, _)| path.starts_with("bump/"))
                                .filter_map(|(_, value)| {
                                    serde_json::from_value::<Bump>(value.clone()).ok()
                                })
                                .filter(|b| {
                                    let project_id = match &self.view {
                                        View::Project(pid) => pid,
                                        _ => return false,
                                    };
                                    b.project_id == *project_id
                                })
                                .collect();
                            bumps.sort_by(|a, b| a.name.cmp(&b.name));

                            if !bumps.is_empty() {
                                if self.selected_bump_index > 0 {
                                    self.selected_bump_index -= 1;
                                } else {
                                    self.selected_bump_index = bumps.len() - 1;
                                }
                                self.bump_table_state.select(Some(self.selected_bump_index));
                            }
                        }
                    }
                    KeyCode::Down => {
                        if let View::Overview = self.view {
                            let projects_count = self
                                .backend
                                .db
                                .iter()
                                .filter(|(path, _)| path.starts_with("project/"))
                                .count();
                            if projects_count > 0 {
                                if self.selected_index < projects_count - 1 {
                                    self.selected_index += 1;
                                } else {
                                    self.selected_index = 0;
                                }
                                self.table_state.select(Some(self.selected_index));
                            }
                        } else if let View::Project(_) = self.view {
                            let mut bumps: Vec<Bump> = self
                                .backend
                                .db
                                .iter()
                                .filter(|(path, _)| path.starts_with("bump/"))
                                .filter_map(|(_, value)| {
                                    serde_json::from_value::<Bump>(value.clone()).ok()
                                })
                                .filter(|b| {
                                    let project_id = match &self.view {
                                        View::Project(pid) => pid,
                                        _ => return false,
                                    };
                                    b.project_id == *project_id
                                })
                                .collect();
                            bumps.sort_by(|a, b| a.name.cmp(&b.name));

                            if !bumps.is_empty() {
                                if self.selected_bump_index < bumps.len() - 1 {
                                    self.selected_bump_index += 1;
                                } else {
                                    self.selected_bump_index = 0;
                                }
                                self.bump_table_state.select(Some(self.selected_bump_index));
                            }
                        }
                    }
                    KeyCode::Right => {
                        if let View::Overview = self.view {
                            let mut projects: Vec<Project> = self
                                .backend
                                .db
                                .iter()
                                .filter(|(path, _)| path.starts_with("project/"))
                                .filter_map(|(_, value)| {
                                    serde_json::from_value::<Project>(value.clone()).ok()
                                })
                                .collect();
                            projects.sort_by(|a, b| {
                                a.platform
                                    .cmp(&b.platform)
                                    .then_with(|| a.repository.cmp(&b.repository))
                            });

                            if self.selected_index < projects.len() {
                                let project = &projects[self.selected_index];
                                self.view = View::Project(project.id.clone());
                                self.bump_table_state.select(Some(0));
                                self.selected_bump_index = 0;
                            }
                        }
                    }
                    KeyCode::Left => {
                        if let View::Project(_) = self.view {
                            self.view = View::Overview;
                        }
                    }
                    KeyCode::Char(' ') => {
                        if let View::Project(project_id) = &self.view {
                            let mut bumps: Vec<Bump> = self
                                .backend
                                .db
                                .iter()
                                .filter(|(path, _)| path.starts_with("bump/"))
                                .filter_map(|(_, value)| {
                                    serde_json::from_value::<Bump>(value.clone()).ok()
                                })
                                .filter(|b| b.project_id == *project_id)
                                .collect();
                            bumps.sort_by(|a, b| a.name.cmp(&b.name));

                            if let Some(bump) = bumps.get(self.selected_bump_index) {
                                let payload = if bump.approved {
                                    Payload::RetractBumpApproval {
                                        bump_id: bump.id.clone(),
                                    }
                                } else {
                                    Payload::ApproveBump {
                                        bump_id: bump.id.clone(),
                                    }
                                };
                                effects.push(Effect::SendPayload(payload));
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            },

            Event::Core(event) => {
                self.backend.process_server_event(event);
            }

            Event::Draw => {
                self.dirty = true;
            }
        }
        effects
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1), // Blank line above Logs
                Constraint::Length(8), // 1 (line) + 1 (header) + 1 (blank) + 4 (logs) + 1 (blank)
                Constraint::Length(1), // line above hotkeys
                Constraint::Length(1), // hotkeys
            ])
            .split(area);

        let log_section_area = chunks[2];
        let line_above_hotkeys_area = chunks[3];
        let hotkeys_area = chunks[4];

        // 1. Top area
        if let View::Overview = self.view {
            let overview_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // Header
                    Constraint::Length(1), // Blank
                    Constraint::Min(0),    // Table
                ])
                .split(chunks[0]);

            // Header
            frame.render_widget(
                Paragraph::new("Projects").style(Style::default().add_modifier(Modifier::BOLD)),
                overview_chunks[0],
            );

            // Blank
            frame.render_widget(Paragraph::new(""), overview_chunks[1]);

            // Table
            let mut projects: Vec<Project> = self
                .backend
                .db
                .iter()
                .filter(|(path, _)| path.starts_with("project/"))
                .filter_map(|(_, value)| serde_json::from_value::<Project>(value.clone()).ok())
                .collect();
            projects.sort_by(|a, b| {
                a.platform
                    .cmp(&b.platform)
                    .then_with(|| a.repository.cmp(&b.repository))
            });

            let max_platform_len = projects
                .iter()
                .map(|p| p.platform.len())
                .max()
                .unwrap_or(0)
                .max(8);
            let platform_width = (max_platform_len + 2).min(overview_chunks[2].width as usize);

            let table_rows: Vec<Row> = projects
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let style = if Some(i) == self.table_state.selected() {
                        Style::default().bg(Color::Blue).fg(Color::White)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    Row::new(vec![
                        Cell::from(p.platform.clone()),
                        Cell::from(p.repository.clone()),
                    ])
                    .style(style)
                })
                .collect();

            let table = Table::new(
                table_rows,
                [
                    Constraint::Length(platform_width as u16),
                    Constraint::Min(0),
                ],
            )
            .header(
                Row::new(vec!["Platform", "Repository"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            );

            frame.render_stateful_widget(table, overview_chunks[2], &mut self.table_state);
        } else if let View::Project(project_id) = &self.view {
            let project_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1), // Header
                    Constraint::Length(1), // Blank
                    Constraint::Min(0),    // Table
                ])
                .split(chunks[0]);

            // Project info
            if let Some(project_val) = self.backend.db.get(&format!("project/{}", project_id)) {
                if let Ok(project) = serde_json::from_value::<Project>(project_val.clone()) {
                    // Header
                    frame.render_widget(
                        Paragraph::new(format!(
                            "Project {}:{}",
                            project.platform, project.repository
                        ))
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                        project_chunks[0],
                    );

                    // Blank
                    frame.render_widget(Paragraph::new(""), project_chunks[1]);

                    // Table of bumps
                    let mut bumps: Vec<Bump> = self
                        .backend
                        .db
                        .iter()
                        .filter(|(path, _)| path.starts_with("bump/"))
                        .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
                        .filter(|b| b.project_id == *project_id)
                        .collect();
                    bumps.sort_by(|a, b| a.name.cmp(&b.name));

                    // Pre-calculate bump_deps for this project
                    let mut bump_deps_map: HashMap<String, Vec<BumpDep>> = HashMap::new();
                    for (path, value) in self.backend.db.iter() {
                        if path.starts_with("bumpdep/") {
                            if let Ok(bd) = serde_json::from_value::<BumpDep>(value.clone()) {
                                if bumps.iter().any(|b| b.id == bd.bump_id) {
                                    bump_deps_map
                                        .entry(bd.bump_id.clone())
                                        .or_default()
                                        .push(bd);
                                }
                            }
                        }
                    }

                    let max_name_len = bumps.iter().map(|b| b.name.len()).max().unwrap_or(0);

                    let table_rows: Vec<Row> = bumps
                        .iter()
                        .enumerate()
                        .map(|(i, b)| {
                            let style = if Some(i) == self.bump_table_state.selected() {
                                Style::default().bg(Color::Blue).fg(Color::White)
                            } else {
                                Style::default().fg(Color::Gray)
                            };

                            let mut currents = Vec::new();
                            let mut targets = Vec::new();
                            let mut heads = Vec::new();

                            if let Some(deps) = bump_deps_map.get(&b.id) {
                                for bd in deps {
                                    if let Some(dep_val) = self
                                        .backend
                                        .db
                                        .get(&format!("dependency/{}", bd.dependency_id))
                                    {
                                        if let Ok(dep) =
                                            serde_json::from_value::<Dependency>(dep_val.clone())
                                        {
                                            if let Some(pkg_val) = self
                                                .backend
                                                .db
                                                .get(&format!("package/{}", dep.package_id))
                                            {
                                                if let Ok(pkg) = serde_json::from_value::<Package>(
                                                    pkg_val.clone(),
                                                ) {
                                                    currents.push(pkg.version);
                                                }
                                            }
                                        }
                                    }
                                    targets.push(bd.target_version.clone());
                                    heads.push(bd.head_version.clone());
                                }
                            }

                            let current_col = if currents.is_empty() {
                                "".to_string()
                            } else if currents.iter().all(|v| v == &currents[0]) {
                                let mut v = currents[0].clone();
                                if v.len() > 15 {
                                    v.truncate(14);
                                    v.push_str("…");
                                }
                                v
                            } else {
                                "multiple".to_string()
                            };

                            let target_col = if targets.is_empty() {
                                "".to_string()
                            } else if targets.iter().all(|v| v == &targets[0]) {
                                let mut v = targets[0].clone();
                                if v.len() > 15 {
                                    v.truncate(14);
                                    v.push_str("…");
                                }
                                v
                            } else {
                                "multiple".to_string()
                            };

                            let head_col = if heads.is_empty() {
                                "".to_string()
                            } else if heads.iter().all(|v| v == &heads[0]) {
                                let mut v = heads[0].clone();
                                if v.len() > 15 {
                                    v.truncate(14);
                                    v.push_str("…");
                                }
                                v
                            } else {
                                "multiple".to_string()
                            };

                            let final_head_col = if target_col == head_col
                                && target_col != "multiple"
                                && !target_col.is_empty()
                            {
                                "-".to_string()
                            } else {
                                head_col
                            };

                            let approved_checkbox = if b.approved { "[x]" } else { "[ ]" };

                            Row::new(vec![
                                Cell::from(approved_checkbox),
                                Cell::from(b.name.clone()),
                                Cell::from(current_col),
                                Cell::from(target_col),
                                Cell::from(final_head_col),
                            ])
                            .style(style)
                        })
                        .collect();

                    let table = Table::new(
                        table_rows,
                        [
                            Constraint::Length(3),
                            Constraint::Length((max_name_len + 2) as u16),
                            Constraint::Length(15),
                            Constraint::Length(15),
                            Constraint::Length(15),
                        ],
                    )
                    .header(
                        Row::new(vec!["", "Name", "Current", "Target", "Head"])
                            .style(Style::default().add_modifier(Modifier::BOLD)),
                    );

                    frame.render_stateful_widget(
                        table,
                        project_chunks[2],
                        &mut self.bump_table_state,
                    );
                }
            }
        }

        // 2. Log section
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

        // 3. Line above hotkeys
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray)),
            line_above_hotkeys_area,
        );

        // 4. Hotkeys
        let mut hotkeys = vec![
            Span::styled("Q", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(" Quit", Style::default().fg(Color::Gray)),
        ];

        if let View::Project(_) = self.view {
            hotkeys.push(Span::from("    "));
            hotkeys.push(Span::styled(
                "Space",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            hotkeys.push(Span::styled(
                " Toggle Approval",
                Style::default().fg(Color::Gray),
            ));
        }

        let hotkey_text = Line::from(hotkeys);
        frame.render_widget(Paragraph::new(hotkey_text), hotkeys_area);
    }
}
