use anyhow::Result;
use crossterm::{
    event::EventStream,
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use futures::TryStreamExt;
use ratatui::{backend::CrosstermBackend, prelude::*};
use std::{io, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::tui::app::Event;

mod app;

pub struct Options {
    pub url: String,
    pub token: String,
}

pub async fn run(options: Options) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Main loop
    let res = run_app(options, &mut terminal).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(err) = &res {
        eprintln!("{:?}", err);
    }

    Ok(())
}

async fn run_app<B: Backend>(options: Options, terminal: &mut Terminal<B>) -> Result<()> {
    let mut app = app::App::new();
    terminal.draw(|frame| app.draw(frame))?;

    let (tx, mut rx) = mpsc::channel(100);

    pump_term_events(tx.clone());

    let (payload_tx, payload_rx) = mpsc::channel(100);
    pump_core_events(
        tx.clone(),
        options.url.clone(),
        options.token.clone(),
        payload_rx,
    );

    let mut redraw_timer_handle: Option<JoinHandle<()>> = None;

    while let Some(event) = rx.recv().await {
        let effects = app.update(event);

        for effect in effects {
            match effect {
                app::Effect::SendPayload(payload) => {
                    let _ = payload_tx.try_send(payload);
                }
            }
        }

        if app.lifecycle == app::Lifecycle::Exiting {
            break;
        }

        if app.dirty {
            terminal.draw(|frame| app.draw(frame))?;
            app.dirty = false;

            if let Some(handle) = redraw_timer_handle.take() {
                handle.abort();
            }
        } else {
            if redraw_timer_handle.is_none() {
                let tx = tx.clone();
                redraw_timer_handle = Some(tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs_f64(1.0 / 60.0)).await;
                    let _ = tx.send(Event::Draw).await;
                }));
            }
        }
    }

    Ok(())
}

fn pump_term_events(tx: mpsc::Sender<app::Event>) {
    tokio::spawn(async move {
        let mut reader = EventStream::new();
        while let Ok(Some(ev)) = reader.try_next().await {
            if tx.send(crate::tui::app::Event::Term(ev)).await.is_err() {
                break;
            }
        }
    });
}

fn pump_core_events(
    tx: mpsc::Sender<app::Event>,
    url: String,
    token: String,
    mut payload_rx: mpsc::Receiver<crate::core::message::Payload>,
) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();

        let _ = tx
            .send(Event::Core(crate::core::event::Event::Trace {
                level: tracing::Level::INFO,
                message: "Connecting to event stream...".into(),
            }))
            .await;

        let res = match client
            .get(format!("{}/events", url))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let _ = tx
                    .send(Event::Core(crate::core::event::Event::Trace {
                        level: tracing::Level::INFO,
                        message: format!("Failed to connect: {}", e),
                    }))
                    .await;
                return;
            }
        };

        if !res.status().is_success() {
            let _ = tx
                .send(Event::Core(crate::core::event::Event::Trace {
                    level: tracing::Level::INFO,
                    message: format!("Server error: {}", res.status()),
                }))
                .await;
            return;
        }

        let _ = tx
            .send(Event::Core(crate::core::event::Event::Trace {
                level: tracing::Level::INFO,
                message: "Streaming events...".into(),
            }))
            .await;

        let payload = serde_json::json!({ "type": "Bootstrap" });
        if let Err(e) = client
            .post(format!("{}/messages", url))
            .header("Authorization", format!("Bearer {}", token))
            .json(&payload)
            .send()
            .await
        {
            let _ = tx
                .send(Event::Core(crate::core::event::Event::Trace {
                    level: tracing::Level::INFO,
                    message: format!("Failed to send Bootstrap: {}", e),
                }))
                .await;
        } else {
            let _ = tx
                .send(Event::Core(crate::core::event::Event::Trace {
                    level: tracing::Level::INFO,
                    message: "Sent Bootstrap message".into(),
                }))
                .await;
        }

        let mut stream = res.bytes_stream();
        let mut buffer = Vec::new();

        loop {
            tokio::select! {
                res = stream.next() => {
                    match res {
                        Some(Ok(bytes)) => {
                            buffer.extend_from_slice(&bytes);
                            while let Some(pos) = buffer.windows(2).position(|w| w == b"\n\n") {
                                let event_bytes = buffer.drain(..pos + 2).collect::<Vec<_>>();
                                let text = String::from_utf8_lossy(&event_bytes);
                                for line in text.lines() {
                                    if let Some(data) = line.strip_prefix("data: ") {
                                        if let Ok(ev) =
                                            serde_json::from_str::<crate::core::event::Event>(data)
                                        {
                                            let _ = tx.send(Event::Core(ev)).await;
                                        } else {
                                            let _ = tx
                                                .send(Event::Core(crate::core::event::Event::Trace {
                                                    level: tracing::Level::INFO,
                                                    message: data.to_string(),
                                                }))
                                                .await;
                                        }
                                    }
                                }
                            }
                        }
                        Some(Err(e)) => {
                            let _ = tx
                                .send(Event::Core(crate::core::event::Event::Trace {
                                    level: tracing::Level::INFO,
                                    message: format!("Stream error: {}", e),
                                }))
                                .await;
                            break;
                        }
                        None => break,
                    }
                }
                Some(payload) = payload_rx.recv() => {
                    let _ = client
                        .post(format!("{}/messages", url))
                        .header("Authorization", format!("Bearer {}", token))
                        .json(&payload)
                        .send()
                        .await;
                }
            }
        }
    });
}
