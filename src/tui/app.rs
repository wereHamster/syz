use crate::core::event::Op;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
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

pub enum View {
    /// Overview showing all projects.
    Overview,

    /// Project view showing the details of a specific project.
    Project(String, usize),
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
            view: View::Overview,
            dirty: false,
        }
    }

    pub fn update(&mut self, event: Event) {
        match event {
            Event::Term(event) => match event {
                crossterm::event::Event::Key(key) => match key.code {
                    KeyCode::Char('q') => {
                        self.lifecycle = Lifecycle::Exiting;
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
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Create a centered layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);

        let content_area = chunks[1];

        let centered_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Percentage(20),
                Constraint::Percentage(40),
            ])
            .split(content_area);

        let vertical_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(40),
                Constraint::Percentage(20),
                Constraint::Percentage(40),
            ])
            .split(centered_chunks[1]);

        let final_area = vertical_chunks[1];

        let block = Block::default().borders(Borders::ALL).title(" syzctl ");

        let paragraph = Paragraph::new("syzctl").block(block);

        frame.render_widget(paragraph, final_area);
    }
}
