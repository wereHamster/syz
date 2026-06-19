use crate::core::database::{Bump, BumpDep, Dependency, Package, Project};
use crate::core::message::Payload;
use crate::tui::app::{Backend, Event, HotkeyDescriptor, View, ViewAction, ViewType};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Cell, Paragraph, Row, Table, TableState},
};
use std::collections::HashMap;

pub struct ProjectView {
    pub project_id: String,
    pub selected_bump_index: usize,
    pub bump_table_state: TableState,
}

impl ProjectView {
    pub fn new(project_id: String) -> Self {
        let mut bump_table_state = TableState::default();
        bump_table_state.select(Some(0));
        Self {
            project_id,
            selected_bump_index: 0,
            bump_table_state,
        }
    }
}

impl View for ProjectView {
    fn update(&mut self, event: &Event, backend: &Backend) -> Vec<ViewAction> {
        if let Event::Term(crossterm::event::Event::Key(key)) = event {
            match key.code {
                KeyCode::Up => {
                    let mut bumps: Vec<Bump> = backend
                        .db
                        .iter()
                        .filter(|(path, _)| path.starts_with("bump/"))
                        .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
                        .filter(|b| b.project_id == self.project_id)
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
                KeyCode::Down => {
                    let mut bumps: Vec<Bump> = backend
                        .db
                        .iter()
                        .filter(|(path, _)| path.starts_with("bump/"))
                        .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
                        .filter(|b| b.project_id == self.project_id)
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
                KeyCode::Left => {
                    return vec![ViewAction::SwitchView(ViewType::Overview)];
                }
                KeyCode::Char(' ') => {
                    let mut bumps: Vec<Bump> = backend
                        .db
                        .iter()
                        .filter(|(path, _)| path.starts_with("bump/"))
                        .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
                        .filter(|b| b.project_id == self.project_id)
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
                        return vec![ViewAction::SendPayload(payload)];
                    }
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    return vec![ViewAction::SendPayload(
                        Payload::AnalyzeProjectDependencies {
                            project_id: self.project_id.clone(),
                            trigger_bumps: false,
                        },
                    )];
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    let mut bumps: Vec<Bump> = backend
                        .db
                        .iter()
                        .filter(|(path, _)| path.starts_with("bump/"))
                        .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
                        .filter(|b| b.project_id == self.project_id)
                        .collect();
                    bumps.sort_by(|a, b| a.name.cmp(&b.name));

                    if let Some(bump) = bumps.get(self.selected_bump_index) {
                        return vec![ViewAction::SendPayload(Payload::ProcessBump {
                            bump_id: bump.id.clone(),
                        })];
                    }
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    return vec![ViewAction::SendPayload(
                        Payload::UpdateVulnerableDependencies {
                            project_id: self.project_id.clone(),
                        },
                    )];
                }
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    return vec![ViewAction::SendPayload(
                        Payload::UpdateTransitiveDependencies {
                            project_id: self.project_id.clone(),
                        },
                    )];
                }
                _ => {}
            }
        }
        vec![]
    }

    fn draw(&mut self, frame: &mut Frame, backend: &Backend, area: Rect) {
        let project_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Length(1), // Blank
                Constraint::Min(0),    // Table
            ])
            .split(area);

        // Project info
        if let Some(project_val) = backend.db.get(&format!("project/{}", self.project_id)) {
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
                let mut bumps: Vec<Bump> = backend
                    .db
                    .iter()
                    .filter(|(path, _)| path.starts_with("bump/"))
                    .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
                    .filter(|b| b.project_id == self.project_id)
                    .collect();
                bumps.sort_by(|a, b| a.name.cmp(&b.name));

                // Pre-calculate bump_deps for this project
                let mut bump_deps_map: HashMap<String, Vec<BumpDep>> = HashMap::new();
                for (path, value) in backend.db.iter() {
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
                                if let Some(dep_val) =
                                    backend.db.get(&format!("dependency/{}", bd.dependency_id))
                                {
                                    if let Ok(dep) =
                                        serde_json::from_value::<Dependency>(dep_val.clone())
                                    {
                                        if let Some(pkg_val) =
                                            backend.db.get(&format!("package/{}", dep.package_id))
                                        {
                                            if let Ok(pkg) =
                                                serde_json::from_value::<Package>(pkg_val.clone())
                                            {
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
                                v.push('…');
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
                                v.push('…');
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
                                v.push('…');
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
                        let pr_url = b.url.clone().unwrap_or_default();

                        Row::new(vec![
                            Cell::from(approved_checkbox),
                            Cell::from(b.name.clone()),
                            Cell::from(current_col),
                            Cell::from(target_col),
                            Cell::from(final_head_col),
                            Cell::from(pr_url),
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
                        Constraint::Min(0),
                    ],
                )
                .header(
                    Row::new(vec![
                        "",
                        "Name",
                        "Current",
                        "Target",
                        "Head",
                        "Pull Request",
                    ])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
                );

                frame.render_stateful_widget(table, project_chunks[2], &mut self.bump_table_state);
            }
        }
    }

    fn hotkeys(&self) -> Vec<HotkeyDescriptor> {
        vec![
            HotkeyDescriptor {
                key: "S".to_string(),
                description: "Scan".to_string(),
            },
            HotkeyDescriptor {
                key: "Space".to_string(),
                description: "Toggle".to_string(),
            },
            HotkeyDescriptor {
                key: "P".to_string(),
                description: "Process".to_string(),
            },
            HotkeyDescriptor {
                key: "A".to_string(),
                description: "Audit".to_string(),
            },
            HotkeyDescriptor {
                key: "T".to_string(),
                description: "Transitive".to_string(),
            },
        ]
    }
}
