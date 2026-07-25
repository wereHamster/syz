use crate::core::database::{Bump, BumpDep, Dependency, Package, Project};
use crate::core::message::Payload;
use crate::tui::app::{Backend, Event, HotkeyDescriptor, View, ViewAction, ViewType};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Cell, Paragraph, Row, Table, TableState},
};

struct AffectedProjectRow {
    project_id: String,
    bump_id: String,
    platform: String,
    repository: String,
    current: String,
    target: String,
    head: String,
    approved: bool,
    pr_url: String,
}

pub struct BumpDetailView {
    pub name: String,
    pub major: bool,
    pub selected_index: usize,
    pub table_state: TableState,
}

impl BumpDetailView {
    pub fn new(name: String, major: bool) -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            name,
            major,
            selected_index: 0,
            table_state,
        }
    }

    fn affected_projects(&self, backend: &Backend) -> Vec<AffectedProjectRow> {
        let bumps: Vec<Bump> = backend
            .db
            .iter()
            .filter(|(path, _)| path.starts_with("bump/"))
            .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
            .filter(|b| b.name == self.name && b.major == self.major)
            .collect();

        let mut rows: Vec<AffectedProjectRow> = bumps
            .iter()
            .filter_map(|bump| {
                let project_val = backend.db.get(&format!("project/{}", bump.project_id))?;
                let project = serde_json::from_value::<Project>(project_val.clone()).ok()?;

                let mut currents = Vec::new();
                let mut targets = Vec::new();
                let mut heads = Vec::new();
                for (path, value) in backend.db.iter() {
                    if path.starts_with("bumpdep/") {
                        if let Ok(bd) = serde_json::from_value::<BumpDep>(value.clone()) {
                            if bd.bump_id == bump.id {
                                if let Some(current) = backend
                                    .db
                                    .get(&format!("dependency/{}", bd.dependency_id))
                                    .and_then(|v| {
                                        serde_json::from_value::<Dependency>(v.clone()).ok()
                                    })
                                    .and_then(|dep| {
                                        backend
                                            .db
                                            .get(&format!("package/{}", dep.package_id))
                                            .cloned()
                                    })
                                    .and_then(|v| serde_json::from_value::<Package>(v).ok())
                                    .map(|pkg| pkg.version)
                                {
                                    currents.push(current);
                                }
                                targets.push(bd.target_version.clone());
                                heads.push(bd.head_version.clone());
                            }
                        }
                    }
                }

                let current = if currents.is_empty() {
                    "".to_string()
                } else if currents.iter().all(|v| v == &currents[0]) {
                    currents[0].clone()
                } else {
                    "multiple".to_string()
                };
                let target = if targets.is_empty() {
                    "".to_string()
                } else if targets.iter().all(|v| v == &targets[0]) {
                    targets[0].clone()
                } else {
                    "multiple".to_string()
                };
                let head = if heads.is_empty() {
                    "".to_string()
                } else if heads.iter().all(|v| v == &heads[0]) {
                    heads[0].clone()
                } else {
                    "multiple".to_string()
                };
                let head = if !target.is_empty() && target != "multiple" && head == target {
                    "-".to_string()
                } else {
                    head
                };

                Some(AffectedProjectRow {
                    project_id: project.id.clone(),
                    bump_id: bump.id.clone(),
                    platform: project.platform,
                    repository: project.repository,
                    current,
                    target,
                    head,
                    approved: bump.approved,
                    pr_url: bump.url.clone().unwrap_or_default(),
                })
            })
            .collect();

        rows.sort_by(|a, b| {
            a.platform
                .cmp(&b.platform)
                .then_with(|| a.repository.cmp(&b.repository))
        });

        rows
    }
}

impl View for BumpDetailView {
    fn update(&mut self, event: &Event, backend: &Backend) -> Vec<ViewAction> {
        if let Event::Term(crossterm::event::Event::Key(key)) = event {
            match key.code {
                KeyCode::Up => {
                    let count = self.affected_projects(backend).len();
                    if count > 0 {
                        if self.selected_index > 0 {
                            self.selected_index -= 1;
                        } else {
                            self.selected_index = count - 1;
                        }
                        self.table_state.select(Some(self.selected_index));
                    }
                }
                KeyCode::Down => {
                    let count = self.affected_projects(backend).len();
                    if count > 0 {
                        if self.selected_index < count - 1 {
                            self.selected_index += 1;
                        } else {
                            self.selected_index = 0;
                        }
                        self.table_state.select(Some(self.selected_index));
                    }
                }
                KeyCode::Right | KeyCode::Enter => {
                    let rows = self.affected_projects(backend);
                    if let Some(row) = rows.get(self.selected_index) {
                        return vec![ViewAction::SwitchView(ViewType::Project(
                            row.project_id.clone(),
                        ))];
                    }
                }
                KeyCode::Left => {
                    return vec![ViewAction::SwitchView(ViewType::Bumps)];
                }
                KeyCode::Char(' ') => {
                    let rows = self.affected_projects(backend);
                    if let Some(row) = rows.get(self.selected_index) {
                        let payload = if row.approved {
                            Payload::RetractBumpApproval {
                                bump_id: row.bump_id.clone(),
                            }
                        } else {
                            Payload::ApproveBump {
                                bump_id: row.bump_id.clone(),
                            }
                        };
                        return vec![ViewAction::SendPayload(payload)];
                    }
                }
                KeyCode::Char('p') => {
                    let rows = self.affected_projects(backend);
                    return rows
                        .iter()
                        .map(|row| {
                            ViewAction::SendPayload(Payload::ProcessBump {
                                bump_id: row.bump_id.clone(),
                            })
                        })
                        .collect();
                }
                _ => {}
            }
        }
        vec![]
    }

    fn draw(&mut self, frame: &mut Frame, backend: &Backend, area: Rect, focused: bool) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Length(1), // Blank
                Constraint::Min(0),    // Table
            ])
            .split(area);

        // Header
        frame.render_widget(
            Paragraph::new(format!(
                "Bump {} ({})",
                self.name,
                if self.major { "major" } else { "minor" }
            ))
            .style(Style::default().add_modifier(Modifier::BOLD)),
            chunks[0],
        );

        // Blank
        frame.render_widget(Paragraph::new(""), chunks[1]);

        let rows = self.affected_projects(backend);

        let max_platform_len = rows
            .iter()
            .map(|r| r.platform.len())
            .max()
            .unwrap_or(0)
            .max(8);
        let platform_width = (max_platform_len + 2).min(chunks[2].width as usize);

        let max_repository_len = rows
            .iter()
            .map(|r| r.repository.len())
            .max()
            .unwrap_or(0)
            .max(10);
        let repository_width = (max_repository_len + 2).min(chunks[2].width as usize);

        let table_rows: Vec<Row> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let style = if focused && Some(i) == self.table_state.selected() {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };

                let approved_checkbox = if r.approved { "[x]" } else { "[ ]" };

                Row::new(vec![
                    Cell::from(approved_checkbox),
                    Cell::from(r.platform.clone()),
                    Cell::from(r.repository.clone()),
                    Cell::from(r.current.clone()),
                    Cell::from(r.target.clone()),
                    Cell::from(r.head.clone()),
                    Cell::from(r.pr_url.clone()),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            table_rows,
            [
                Constraint::Length(3),
                Constraint::Length(platform_width as u16),
                Constraint::Length(repository_width as u16),
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Min(0),
            ],
        )
        .header(
            Row::new(vec![
                "",
                "Platform",
                "Repository",
                "Current",
                "Target",
                "Head",
                "Pull Request",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        );

        frame.render_stateful_widget(table, chunks[2], &mut self.table_state);
    }

    fn hotkeys(&self) -> Vec<HotkeyDescriptor> {
        vec![
            HotkeyDescriptor {
                key: "Space".to_string(),
                description: "Toggle".to_string(),
            },
            HotkeyDescriptor {
                key: "P".to_string(),
                description: "Process All".to_string(),
            },
        ]
    }
}
