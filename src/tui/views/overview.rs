use crate::core::database::{Bump, Project};
use crate::tui::app::{Backend, Event, HotkeyDescriptor, View, ViewAction, ViewType};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    text::{Line, Span},
    widgets::{Cell, Paragraph, Row, Table, TableState},
};
use std::collections::HashMap;

pub struct OverviewView {
    pub selected_index: usize,
    pub table_state: TableState,
}

impl OverviewView {
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            selected_index: 0,
            table_state,
        }
    }
}

impl View for OverviewView {
    fn update(&mut self, event: &Event, backend: &Backend) -> Vec<ViewAction> {
        if let Event::Term(crossterm::event::Event::Key(key)) = event {
            match key.code {
                KeyCode::Up => {
                    let projects_count = backend
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
                }
                KeyCode::Down => {
                    let projects_count = backend
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
                }
                KeyCode::Right => {
                    let mut projects: Vec<Project> = backend
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
                        return vec![ViewAction::SwitchView(ViewType::Project(
                            project.id.clone(),
                        ))];
                    }
                }
                _ => {}
            }
        }
        vec![]
    }

    fn draw(&mut self, frame: &mut Frame, backend: &Backend, area: Rect) {
        let overview_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // Header
                Constraint::Length(1), // Blank
                Constraint::Min(0),    // Table
            ])
            .split(area);

        // Header
        frame.render_widget(
            Paragraph::new("Projects").style(Style::default().add_modifier(Modifier::BOLD)),
            overview_chunks[0],
        );

        // Blank
        frame.render_widget(Paragraph::new(""), overview_chunks[1]);

        // Table
        let mut projects: Vec<Project> = backend
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

        let mut bumps_by_project: HashMap<String, (usize, usize, usize)> = HashMap::new();
        for (path, value) in backend.db.iter() {
            if path.starts_with("bump/") {
                if let Ok(bump) = serde_json::from_value::<Bump>(value.clone()) {
                    let stats = bumps_by_project
                        .entry(bump.project_id.clone())
                        .or_insert((0, 0, 0));
                    stats.0 += 1;
                    if bump.approved {
                        stats.1 += 1;
                    }
                    if bump.url.is_some() {
                        stats.2 += 1;
                    }
                }
            }
        }

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

                let (total_bumps, approved_bumps, pr_bumps) =
                    bumps_by_project.get(&p.id).cloned().unwrap_or((0, 0, 0));

                let mut bumps_spans = Vec::new();

                let add_bump_spans = |val: usize, spans: &mut Vec<Span>| {
                    if val == 0 {
                        spans.push(Span::styled("   ", Style::default().fg(Color::DarkGray)));
                    } else {
                        let s = val.to_string();
                        let padding_len = 3usize.saturating_sub(s.len());

                        if padding_len > 0 {
                            spans.push(Span::styled(
                                " ".repeat(padding_len),
                                Style::default().fg(Color::DarkGray),
                            ));
                        }

                        let val_style = Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD);
                        spans.push(Span::styled(s, val_style));
                    }
                };

                add_bump_spans(total_bumps, &mut bumps_spans);
                bumps_spans.push(Span::styled(
                    " | ".to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
                add_bump_spans(approved_bumps, &mut bumps_spans);
                bumps_spans.push(Span::styled(
                    " | ".to_string(),
                    Style::default().fg(Color::DarkGray),
                ));
                add_bump_spans(pr_bumps, &mut bumps_spans);

                let bumps_line = Line::from(bumps_spans);

                Row::new(vec![
                    Cell::from(p.platform.clone()),
                    Cell::from(p.repository.clone()),
                    Cell::from(bumps_line),
                ])
                .style(style)
            })
            .collect();

        let table = Table::new(
            table_rows,
            [
                Constraint::Length(platform_width as u16),
                Constraint::Min(0),
                Constraint::Length(20),
            ],
        )
        .header(
            Row::new(vec!["Platform", "Repository", "Bumps"])
                .style(Style::default().add_modifier(Modifier::BOLD)),
        );

        frame.render_stateful_widget(table, overview_chunks[2], &mut self.table_state);
    }

    fn hotkeys(&self) -> Vec<HotkeyDescriptor> {
        vec![]
    }
}
