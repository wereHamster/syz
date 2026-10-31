use crate::core::actions::update_compatible_dependencies;
use crate::core::database::{Bump, BumpDep, Dependency, Package, Project, Sweep, SweepKind};
use crate::core::message::Payload;
use crate::tui::app::{Backend, Event, HotkeyDescriptor, View, ViewAction, ViewType};
use crate::tui::views::bumps::{affected_projects_by_bump_group, ecosystem_label};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Cell, Paragraph, Row, Table, TableState},
};
use std::collections::{HashMap, HashSet};

struct ProjectBumpRow {
    bump: Bump,
    ecosystem: String,
    current: String,
    target: String,
    head: String,
}

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

    /// A line showing the open pull request of the given kind of sweep, if there is one.
    fn sweep_line(backend: &Backend, project_id: &str, kind: SweepKind) -> Line<'static> {
        let label = match kind {
            SweepKind::Audit => "Audit",
            SweepKind::Transitive => "Transitive",
            SweepKind::Compatible => "Compatible",
        };

        let url = backend
            .db
            .get(&Sweep::path(project_id, kind))
            .and_then(|v| serde_json::from_value::<Sweep>(v.clone()).ok())
            .map(|pr| pr.url);

        let value = match url {
            Some(url) => Span::raw(url),
            None => Span::styled("-", Style::default().fg(Color::DarkGray)),
        };

        let mut spans = vec![
            Span::styled(
                format!("{:<12}", label),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            value,
        ];

        if kind == SweepKind::Compatible {
            let count = Self::compatible_update_count(backend, project_id);
            if count > 0 {
                spans.push(Span::styled(
                    format!(
                        "  {} {}",
                        count,
                        if count == 1 { "update" } else { "updates" }
                    ),
                    Style::default().fg(Color::DarkGray),
                ));
            }
        }

        Line::from(spans)
    }

    /// How many packages the compatible sweep would update, going by the latest scan.
    fn compatible_update_count(backend: &Backend, project_id: &str) -> usize {
        let bump_ids: HashSet<String> = backend
            .db
            .iter()
            .filter(|(path, _)| path.starts_with("bump/"))
            .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
            .filter(|b| b.project_id == project_id && !b.major)
            .map(|b| b.id)
            .collect();

        let packages: HashSet<(String, Option<String>, String, Option<String>)> = backend
            .db
            .iter()
            .filter(|(path, _)| path.starts_with("bumpdep/"))
            .filter_map(|(_, value)| serde_json::from_value::<BumpDep>(value.clone()).ok())
            .filter(|bd| bump_ids.contains(&bd.bump_id))
            .filter_map(|bd| {
                backend
                    .db
                    .get(&format!("dependency/{}", bd.dependency_id))
                    .and_then(|v| serde_json::from_value::<Dependency>(v.clone()).ok())
            })
            .filter_map(|dep| {
                backend
                    .db
                    .get(&format!("package/{}", dep.package_id))
                    .and_then(|v| serde_json::from_value::<Package>(v.clone()).ok())
            })
            .filter(|pkg| update_compatible_dependencies::ECOSYSTEMS.contains(&pkg.r#type.as_str()))
            .map(|pkg| (pkg.r#type, pkg.namespace, pkg.name, pkg.subpath))
            .collect();

        packages.len()
    }

    /// This project's bumps, grouped by ecosystem and sorted so that the ecosystem
    /// containing the single most-impactful bump (by projects affected, system-wide)
    /// sorts first.
    fn rows(backend: &Backend, project_id: &str) -> Vec<ProjectBumpRow> {
        let bumps: Vec<Bump> = backend
            .db
            .iter()
            .filter(|(path, _)| path.starts_with("bump/"))
            .filter_map(|(_, value)| serde_json::from_value::<Bump>(value.clone()).ok())
            .filter(|b| b.project_id == project_id)
            .collect();

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

        let mut rows: Vec<ProjectBumpRow> = bumps
            .into_iter()
            .map(|b| {
                let mut currents = Vec::new();
                let mut ecosystems = Vec::new();
                let mut targets = Vec::new();
                let mut heads = Vec::new();

                if let Some(deps) = bump_deps_map.get(&b.id) {
                    for bd in deps {
                        if let Some(pkg) = backend
                            .db
                            .get(&format!("dependency/{}", bd.dependency_id))
                            .and_then(|v| serde_json::from_value::<Dependency>(v.clone()).ok())
                            .and_then(|dep| {
                                backend
                                    .db
                                    .get(&format!("package/{}", dep.package_id))
                                    .cloned()
                            })
                            .and_then(|v| serde_json::from_value::<Package>(v).ok())
                        {
                            currents.push(pkg.version);
                            ecosystems.push(pkg.r#type);
                        }
                        targets.push(bd.target_version.clone());
                        heads.push(bd.head_version.clone());
                    }
                }

                let truncate = |v: &str| {
                    let mut v = v.to_string();
                    if v.len() > 15 {
                        v.truncate(14);
                        v.push('…');
                    }
                    v
                };

                let current = if currents.is_empty() {
                    "".to_string()
                } else if currents.iter().all(|v| v == &currents[0]) {
                    truncate(&currents[0])
                } else {
                    "multiple".to_string()
                };

                let ecosystem = if ecosystems.is_empty() {
                    "".to_string()
                } else if ecosystems.iter().all(|v| v == &ecosystems[0]) {
                    ecosystems[0].clone()
                } else {
                    "multiple".to_string()
                };

                let target = if targets.is_empty() {
                    "".to_string()
                } else if targets.iter().all(|v| v == &targets[0]) {
                    truncate(&targets[0])
                } else {
                    "multiple".to_string()
                };

                let head = if heads.is_empty() {
                    "".to_string()
                } else if heads.iter().all(|v| v == &heads[0]) {
                    truncate(&heads[0])
                } else {
                    "multiple".to_string()
                };

                let all_deps_matched = bump_deps_map
                    .get(&b.id)
                    .map(|deps| deps.iter().all(|bd| bd.target_version == bd.head_version))
                    .unwrap_or(false);

                let head = if target == head && !target.is_empty() {
                    if all_deps_matched {
                        "-".to_string()
                    } else {
                        head
                    }
                } else {
                    head
                };

                ProjectBumpRow {
                    bump: b,
                    ecosystem,
                    current,
                    target,
                    head,
                }
            })
            .collect();

        // Order ecosystem groups by the max number of projects affected by any single
        // bump within that ecosystem, system-wide, so the most consequential ecosystem
        // sorts first (matching the ordering used in the bumps overview).
        let affected = affected_projects_by_bump_group(backend);
        let mut max_by_ecosystem: HashMap<String, usize> = HashMap::new();
        for row in &rows {
            let count = affected
                .get(&(row.bump.name.clone(), row.bump.major))
                .copied()
                .unwrap_or(1);
            let entry = max_by_ecosystem.entry(row.ecosystem.clone()).or_insert(0);
            *entry = (*entry).max(count);
        }

        rows.sort_by(|a, b| {
            max_by_ecosystem[&b.ecosystem]
                .cmp(&max_by_ecosystem[&a.ecosystem])
                .then_with(|| a.ecosystem.cmp(&b.ecosystem))
                .then_with(|| a.bump.name.cmp(&b.bump.name))
        });

        rows
    }
}

impl View for ProjectView {
    fn update(&mut self, event: &Event, backend: &Backend) -> Vec<ViewAction> {
        if let Event::Term(crossterm::event::Event::Key(key)) = event {
            match key.code {
                KeyCode::Up => {
                    let count = Self::rows(backend, &self.project_id).len();
                    if count > 0 {
                        if self.selected_bump_index > 0 {
                            self.selected_bump_index -= 1;
                        } else {
                            self.selected_bump_index = count - 1;
                        }
                        self.bump_table_state.select(Some(self.selected_bump_index));
                    }
                }
                KeyCode::Down => {
                    let count = Self::rows(backend, &self.project_id).len();
                    if count > 0 {
                        if self.selected_bump_index < count - 1 {
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
                    let rows = Self::rows(backend, &self.project_id);
                    if let Some(row) = rows.get(self.selected_bump_index) {
                        let payload = if row.bump.approved {
                            Payload::RetractBumpApproval {
                                bump_id: row.bump.id.clone(),
                            }
                        } else {
                            Payload::ApproveBump {
                                bump_id: row.bump.id.clone(),
                            }
                        };
                        return vec![ViewAction::SendPayload(payload)];
                    }
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    return vec![ViewAction::SendPayload(
                        Payload::AnalyzeProjectDependencies {
                            project_id: self.project_id.clone(),
                        },
                    )];
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    let rows = Self::rows(backend, &self.project_id);
                    if let Some(row) = rows.get(self.selected_bump_index) {
                        return vec![ViewAction::SendPayload(Payload::ProcessBump {
                            bump_id: row.bump.id.clone(),
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
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    return vec![ViewAction::SendPayload(
                        Payload::UpdateCompatibleDependencies {
                            project_id: self.project_id.clone(),
                        },
                    )];
                }
                _ => {}
            }
        }
        vec![]
    }

    fn draw(&mut self, frame: &mut Frame, backend: &Backend, area: Rect, focused: bool) {
        let project_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Length(1), // Blank
                Constraint::Length(3), // Audit, transitive, compatible
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

                // Open pull requests of the sweeps
                frame.render_widget(
                    Paragraph::new(
                        SweepKind::ALL
                            .into_iter()
                            .map(|kind| Self::sweep_line(backend, &self.project_id, kind))
                            .collect::<Vec<_>>(),
                    ),
                    project_chunks[2],
                );

                // Table of bumps, grouped by ecosystem
                let rows = Self::rows(backend, &self.project_id);

                let max_name_len = rows.iter().map(|r| r.bump.name.len()).max().unwrap_or(0);

                let column_widths = [
                    Constraint::Length(3),
                    Constraint::Length((max_name_len + 2) as u16),
                    Constraint::Length(15),
                    Constraint::Length(15),
                    Constraint::Length(15),
                    Constraint::Min(0),
                ];
                let column_count = column_widths.len() as u16;

                let mut table_rows: Vec<Row> = Vec::new();
                let mut selected_table_index: Option<usize> = None;
                let mut last_ecosystem: Option<&str> = None;

                for (data_index, r) in rows.iter().enumerate() {
                    if last_ecosystem != Some(r.ecosystem.as_str()) {
                        if last_ecosystem.is_some() {
                            table_rows.push(Row::new(Vec::<Cell>::new()));
                        }
                        table_rows.push(Row::new(vec![Cell::new(ecosystem_label(&r.ecosystem))
                            .style(Style::default().add_modifier(Modifier::BOLD))
                            .column_span(column_count)]));
                        last_ecosystem = Some(r.ecosystem.as_str());
                    }

                    if data_index == self.selected_bump_index {
                        selected_table_index = Some(table_rows.len());
                    }

                    let style = if focused && data_index == self.selected_bump_index {
                        Style::default().bg(Color::Blue).fg(Color::White)
                    } else {
                        Style::default().fg(Color::Gray)
                    };

                    let approved_checkbox = if r.bump.approved { "[x]" } else { "[ ]" };
                    let pr_url = r.bump.url.clone().unwrap_or_default();

                    table_rows.push(
                        Row::new(vec![
                            Cell::from(approved_checkbox),
                            Cell::from(r.bump.name.clone()),
                            Cell::from(r.current.clone()),
                            Cell::from(r.target.clone()),
                            Cell::from(r.head.clone()),
                            Cell::from(pr_url),
                        ])
                        .style(style),
                    );
                }

                self.bump_table_state.select(selected_table_index);

                let table = Table::new(table_rows, column_widths).header(
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

                frame.render_stateful_widget(table, project_chunks[4], &mut self.bump_table_state);
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
            HotkeyDescriptor {
                key: "M".to_string(),
                description: "Compatible".to_string(),
            },
        ]
    }
}
