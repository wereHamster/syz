use crate::core::database::{Bump, BumpDep, Dependency, Package};
use crate::core::message::Payload;
use crate::tui::app::{Backend, Event, HotkeyDescriptor, View, ViewAction, ViewType};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    text::{Line, Span},
    widgets::{Cell, Row, Table, TableState},
};
use std::collections::{HashMap, HashSet};

struct BumpGroupRow {
    name: String,
    major: bool,
    current: String,
    target: String,
    head: String,
    affected_projects: usize,
    approved: usize,
    prs: usize,
    /// (bump id, approved) for every bump in this group, across all affected projects.
    bumps: Vec<(String, bool)>,
}

pub struct BumpsView {
    pub selected_index: usize,
    pub table_state: TableState,
}

impl BumpsView {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            selected_index: 0,
            table_state,
        }
    }

    fn grouped_rows(backend: &Backend) -> Vec<BumpGroupRow> {
        let bumps: Vec<Bump> = backend
            .db
            .iter()
            .filter(|(path, _)| path.starts_with("bump/"))
            .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
            .collect();

        let mut bump_id_to_group: HashMap<String, (String, bool)> = HashMap::new();
        let mut projects_by_group: HashMap<(String, bool), HashSet<String>> = HashMap::new();
        let mut approved_by_group: HashMap<(String, bool), usize> = HashMap::new();
        let mut prs_by_group: HashMap<(String, bool), usize> = HashMap::new();
        let mut bumps_by_group: HashMap<(String, bool), Vec<(String, bool)>> = HashMap::new();

        for bump in &bumps {
            let key = (bump.name.clone(), bump.major);
            bump_id_to_group.insert(bump.id.clone(), key.clone());
            projects_by_group
                .entry(key.clone())
                .or_default()
                .insert(bump.project_id.clone());
            if bump.approved {
                *approved_by_group.entry(key.clone()).or_insert(0) += 1;
            }
            if bump.url.is_some() {
                *prs_by_group.entry(key.clone()).or_insert(0) += 1;
            }
            bumps_by_group
                .entry(key)
                .or_default()
                .push((bump.id.clone(), bump.approved));
        }

        let mut currents_by_group: HashMap<(String, bool), Vec<String>> = HashMap::new();
        let mut targets_by_group: HashMap<(String, bool), Vec<String>> = HashMap::new();
        let mut heads_by_group: HashMap<(String, bool), Vec<String>> = HashMap::new();

        for (path, value) in backend.db.iter() {
            if path.starts_with("bumpdep/") {
                if let Ok(bd) = serde_json::from_value::<BumpDep>(value.clone()) {
                    if let Some(key) = bump_id_to_group.get(&bd.bump_id) {
                        if let Some(current) = backend
                            .db
                            .get(&format!("dependency/{}", bd.dependency_id))
                            .and_then(|v| serde_json::from_value::<Dependency>(v.clone()).ok())
                            .and_then(|dep| {
                                backend.db.get(&format!("package/{}", dep.package_id)).cloned()
                            })
                            .and_then(|v| serde_json::from_value::<Package>(v).ok())
                            .map(|pkg| pkg.version)
                        {
                            currents_by_group
                                .entry(key.clone())
                                .or_default()
                                .push(current);
                        }
                        targets_by_group
                            .entry(key.clone())
                            .or_default()
                            .push(bd.target_version.clone());
                        heads_by_group
                            .entry(key.clone())
                            .or_default()
                            .push(bd.head_version.clone());
                    }
                }
            }
        }

        let mut rows: Vec<BumpGroupRow> = projects_by_group
            .into_iter()
            .map(|(key, project_ids)| {
                let (name, major) = key.clone();

                let currents = currents_by_group.get(&key).cloned().unwrap_or_default();
                let current = if currents.is_empty() {
                    "".to_string()
                } else if currents.iter().all(|v| v == &currents[0]) {
                    currents[0].clone()
                } else {
                    "multiple".to_string()
                };

                let targets = targets_by_group.get(&key).cloned().unwrap_or_default();
                let target = if targets.is_empty() {
                    "".to_string()
                } else if targets.iter().all(|v| v == &targets[0]) {
                    targets[0].clone()
                } else {
                    "multiple".to_string()
                };

                let heads = heads_by_group.get(&key).cloned().unwrap_or_default();
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

                BumpGroupRow {
                    name,
                    major,
                    current,
                    target,
                    head,
                    affected_projects: project_ids.len(),
                    approved: *approved_by_group.get(&key).unwrap_or(&0),
                    prs: *prs_by_group.get(&key).unwrap_or(&0),
                    bumps: bumps_by_group.get(&key).cloned().unwrap_or_default(),
                }
            })
            .collect();

        rows.sort_by(|a, b| {
            b.affected_projects
                .cmp(&a.affected_projects)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.major.cmp(&b.major))
        });

        rows
    }
}

impl Default for BumpsView {
    fn default() -> Self {
        Self::new()
    }
}

impl View for BumpsView {
    fn update(&mut self, event: &Event, backend: &Backend) -> Vec<ViewAction> {
        if let Event::Term(crossterm::event::Event::Key(key)) = event {
            match key.code {
                KeyCode::Up => {
                    let count = Self::grouped_rows(backend).len();
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
                    let count = Self::grouped_rows(backend).len();
                    if count > 0 {
                        if self.selected_index < count - 1 {
                            self.selected_index += 1;
                        } else {
                            self.selected_index = 0;
                        }
                        self.table_state.select(Some(self.selected_index));
                    }
                }
                KeyCode::Right => {
                    let rows = Self::grouped_rows(backend);
                    if let Some(row) = rows.get(self.selected_index) {
                        return vec![ViewAction::SwitchView(ViewType::BumpDetail {
                            name: row.name.clone(),
                            major: row.major,
                        })];
                    }
                }
                KeyCode::Char(' ') => {
                    let rows = Self::grouped_rows(backend);
                    if let Some(row) = rows.get(self.selected_index) {
                        let all_approved = row.bumps.iter().all(|(_, approved)| *approved);
                        return row
                            .bumps
                            .iter()
                            .map(|(bump_id, _)| {
                                ViewAction::SendPayload(if all_approved {
                                    Payload::RetractBumpApproval {
                                        bump_id: bump_id.clone(),
                                    }
                                } else {
                                    Payload::ApproveBump {
                                        bump_id: bump_id.clone(),
                                    }
                                })
                            })
                            .collect();
                    }
                }
                KeyCode::Char('p') => {
                    let rows = Self::grouped_rows(backend);
                    if let Some(row) = rows.get(self.selected_index) {
                        return row
                            .bumps
                            .iter()
                            .map(|(bump_id, _)| {
                                ViewAction::SendPayload(Payload::ProcessBump {
                                    bump_id: bump_id.clone(),
                                })
                            })
                            .collect();
                    }
                }
                KeyCode::Char('s') => {
                    return vec![ViewAction::SendPayload(
                        Payload::AnalyzeAllProjectsDependencies,
                    )];
                }
                _ => {}
            }
        }
        vec![]
    }

    fn draw(&mut self, frame: &mut Frame, backend: &Backend, area: Rect, focused: bool) {
        let rows = Self::grouped_rows(backend);

        let max_name_len = rows.iter().map(|r| r.name.len()).max().unwrap_or(0).max(4);

        let add_count_spans = |val: usize, spans: &mut Vec<Span>| {
            if val == 0 {
                spans.push(Span::styled("  ", Style::default().fg(Color::DarkGray)));
            } else {
                spans.push(Span::styled(
                    val.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ));
            }
        };

        let table_rows: Vec<Row> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let style = if focused && Some(i) == self.table_state.selected() {
                    Style::default().bg(Color::Blue).fg(Color::White)
                } else {
                    Style::default().fg(Color::Gray)
                };

                let mut projects_spans = Vec::new();
                add_count_spans(r.affected_projects, &mut projects_spans);
                let mut approved_spans = Vec::new();
                add_count_spans(r.approved, &mut approved_spans);
                let mut prs_spans = Vec::new();
                add_count_spans(r.prs, &mut prs_spans);

                let approved_checkbox = if r.approved == 0 {
                    "[ ]"
                } else if r.approved == r.bumps.len() {
                    "[x]"
                } else {
                    "[-]"
                };

                Row::new(vec![
                    Cell::from(approved_checkbox),
                    Cell::from(r.name.clone()),
                    Cell::from(if r.major { "major" } else { "minor" }),
                    Cell::from(r.current.clone()),
                    Cell::from(r.target.clone()),
                    Cell::from(r.head.clone()),
                    Cell::from(Line::from(projects_spans)),
                    Cell::from(Line::from(approved_spans)),
                    Cell::from(Line::from(prs_spans)),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            table_rows,
            [
                Constraint::Length(3),
                Constraint::Length((max_name_len + 2) as u16),
                Constraint::Length(7),
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Length(15),
                Constraint::Length(10),
                Constraint::Length(10),
                Constraint::Min(5),
            ],
        )
        .header(
            Row::new(vec![
                "", "Name", "Type", "Current", "Target", "Head", "Projects", "Approved", "PRs",
            ])
            .style(Style::default().add_modifier(Modifier::BOLD)),
        );

        frame.render_stateful_widget(table, area, &mut self.table_state);
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
                description: "Process All".to_string(),
            },
        ]
    }
}
