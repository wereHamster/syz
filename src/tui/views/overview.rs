use crate::core::database::Project;
use crate::tui::app::{Backend, Event, View, ViewAction, ViewType};
use crossterm::event::KeyCode;
use ratatui::{
    prelude::*,
    widgets::{Cell, Paragraph, Row, Table, TableState},
};

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
    }

    fn hotkeys(&self) -> Vec<(String, String)> {
        vec![]
    }
}
