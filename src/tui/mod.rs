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
use tokio::{
    sync::{broadcast::error::RecvError, mpsc},
    task::JoinHandle,
};

use crate::core::application::Handle;
use crate::core::database::pk;
use crate::core::message::Payload;
use crate::tui::app::Event;

mod app;
pub mod views;

/// Where the TUI gets its state from and sends its messages to.
pub enum Connection {
    /// A syzd server, reached over HTTP.
    Remote { url: String, token: String },

    /// An application running in this process.
    Local(Handle),
}

pub async fn run(connection: Connection) -> Result<()> {
    install_panic_hook();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Main loop
    let res = run_app(connection, &mut terminal).await;

    restore_terminal()?;

    if let Err(err) = &res {
        eprintln!("{:?}", err);
    }

    Ok(())
}

fn restore_terminal() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show)
}

/// Make panics leave the terminal usable, for the rest of the process.
///
/// A panic of the TUI itself unwinds past the code that restores the terminal, and would leave
/// it in raw mode on the alternate screen, so the hook restores it before the message is
/// printed. Other threads panic in tasks whose panics are caught, jobs for example, and the TUI
/// keeps running. Their message goes to the log if they log somewhere, instead of over the
/// screen.
fn install_panic_hook() {
    let tui_thread = std::thread::current().id();
    let previous = std::panic::take_hook();

    std::panic::set_hook(Box::new(move |info| {
        let logs = tracing::dispatcher::get_default(|dispatch| {
            !dispatch.is::<tracing::subscriber::NoSubscriber>()
        });

        if std::thread::current().id() == tui_thread {
            let _ = restore_terminal();
            previous(info);
        } else if logs {
            tracing::error!("{}", info);
        } else {
            previous(info);
        }
    }));
}

async fn run_app<B: Backend>(connection: Connection, terminal: &mut Terminal<B>) -> Result<()>
where
    <B as Backend>::Error: Send + Sync + 'static,
{
    let mut app = app::App::new();
    terminal.draw(|frame| app.draw(frame))?;

    let (tx, mut rx) = mpsc::channel(100);

    pump_term_events(tx.clone());

    let (payload_tx, payload_rx) = mpsc::channel(100);
    match connection {
        Connection::Remote { url, token } => pump_remote_events(tx.clone(), url, token, payload_rx),
        Connection::Local(handle) => pump_local_events(tx.clone(), handle, payload_rx),
    }

    let mut redraw_timer_handle: Option<JoinHandle<()>> = None;

    while let Some(event) = rx.recv().await {
        if let Event::Fatal(message) = event {
            anyhow::bail!(message);
        }

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

fn pump_remote_events(
    tx: mpsc::Sender<app::Event>,
    url: String,
    token: String,
    mut payload_rx: mpsc::Receiver<Payload>,
) {
    tokio::spawn(async move {
        let client = reqwest::Client::new();

        loop {
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
                            level: tracing::Level::ERROR,
                            message: format!("Failed to connect: {}. Retrying in 5s...", e),
                        }))
                        .await;
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };

            if !res.status().is_success() {
                let _ = tx
                    .send(Event::Core(crate::core::event::Event::Trace {
                        level: tracing::Level::ERROR,
                        message: format!("Server error: {}. Retrying in 5s...", res.status()),
                    }))
                    .await;
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
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
                        level: tracing::Level::ERROR,
                        message: format!("Failed to send Bootstrap: {}. Retrying in 5s...", e),
                    }))
                    .await;
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
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
                                        level: tracing::Level::ERROR,
                                        message: format!("Stream error: {}. Retrying in 5s...", e),
                                    }))
                                    .await;
                                break;
                            }
                            None => {
                                let _ = tx
                                    .send(Event::Core(crate::core::event::Event::Trace {
                                        level: tracing::Level::INFO,
                                        message: "Stream ended. Retrying in 5s...".into(),
                                    }))
                                    .await;
                                break;
                            }
                        }
                    }
                    res = payload_rx.recv() => {
                        match res {
                            Some(payload) => {
                                if let Err(e) = client
                                    .post(format!("{}/messages", url))
                                    .header("Authorization", format!("Bearer {}", token))
                                    .json(&payload)
                                    .send()
                                    .await
                                {
                                    let _ = tx.send(Event::Core(crate::core::event::Event::Trace {
                                        level: tracing::Level::ERROR,
                                        message: format!("Failed to send payload: {}", e),
                                    })).await;
                                }
                            }
                            None => return,
                        }
                    }
                }
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

fn pump_local_events(
    tx: mpsc::Sender<app::Event>,
    handle: Handle,
    mut payload_rx: mpsc::Receiver<Payload>,
) {
    tokio::spawn(async move {
        // Subscribe before asking for the state, so that no change in between is missed.
        let mut events = handle.subscribe();
        if let Err(e) = handle.send(pk(), Payload::Bootstrap).await {
            let _ = tx
                .send(Event::Fatal(format!("Failed to send Bootstrap: {:#}", e)))
                .await;
            return;
        }

        // The last error the application logged, which likely explains why it stopped.
        let mut last_error = None;

        loop {
            tokio::select! {
                // Events first, so that the error logged by a stopping loop is seen.
                biased;

                res = events.recv() => {
                    match res {
                        Ok(ev) => {
                            if let crate::core::event::Event::Trace {
                                level: tracing::Level::ERROR,
                                message,
                            } = &ev
                            {
                                last_error = Some(message.clone());
                            }
                            if tx.send(Event::Core(ev)).await.is_err() {
                                return;
                            }
                        }
                        // The TUI fell behind and missed changes, ask for the whole state again.
                        Err(RecvError::Lagged(missed)) => {
                            let _ = tx
                                .send(Event::Core(crate::core::event::Event::Trace {
                                    level: tracing::Level::WARN,
                                    message: format!("Missed {} events, reloading the state", missed),
                                }))
                                .await;
                            if let Err(e) = handle.send(pk(), Payload::Bootstrap).await {
                                let _ = tx
                                    .send(Event::Core(crate::core::event::Event::Trace {
                                        level: tracing::Level::ERROR,
                                        message: format!("Failed to send Bootstrap: {:#}", e),
                                    }))
                                    .await;
                            }
                        }
                        Err(RecvError::Closed) => {
                            let _ = tx
                                .send(Event::Core(crate::core::event::Event::Trace {
                                    level: tracing::Level::ERROR,
                                    message: "The application stopped broadcasting events".into(),
                                }))
                                .await;
                            return;
                        }
                    }
                }
                res = payload_rx.recv() => {
                    match res {
                        Some(payload) => {
                            if let Err(e) = handle.send(pk(), payload).await {
                                let _ = tx.send(Event::Core(crate::core::event::Event::Trace {
                                    level: tracing::Level::ERROR,
                                    message: format!("Failed to send payload: {:#}", e),
                                })).await;
                            }
                        }
                        None => return,
                    }
                }
                // Its state would go stale from here on, so the TUI can't go on.
                () = handle.closed() => {
                    let message = match last_error {
                        Some(error) => format!("The application loop stopped, the last error it logged: {}", error),
                        None => "The application loop stopped".to_string(),
                    };
                    let _ = tx.send(Event::Fatal(message)).await;
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::database::Database;
    use crate::core::event::Event as CoreEvent;
    use crate::core::message::Message;
    use crate::core::store::Store;

    /// The local pump, on a handle without a main loop.
    struct Fixture {
        _dir: tempfile::TempDir,
        handle: Handle,

        /// What the pump sends to the application. Dropping it stops the "loop".
        mailbox: mpsc::Receiver<Message>,

        /// What the pump sends to the TUI.
        events: mpsc::Receiver<Event>,

        /// Kept open, so that only the stop of the loop ends the pump.
        _payloads: mpsc::Sender<Payload>,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::new(Database::open(dir.path()).await.unwrap());
            let (handle, mailbox) = Handle::for_tests(store);

            let (tx, events) = mpsc::channel(100);
            let (payloads, payload_rx) = mpsc::channel(100);
            pump_local_events(tx, handle.clone(), payload_rx);

            Self {
                _dir: dir,
                handle,
                mailbox,
                events,
                _payloads: payloads,
            }
        }
    }

    #[tokio::test]
    async fn asks_for_the_state_first() {
        let mut f = Fixture::new().await;

        let message = f.mailbox.recv().await.unwrap();
        assert!(matches!(message.payload, Payload::Bootstrap));
    }

    #[tokio::test]
    async fn fails_with_the_last_error_once_the_loop_stops() {
        let mut f = Fixture::new().await;
        f.mailbox.recv().await.unwrap();

        f.handle
            .broadcast(CoreEvent::Trace {
                level: tracing::Level::ERROR,
                message: "boom".into(),
            })
            .unwrap();
        drop(f.mailbox);

        assert!(matches!(
            f.events.recv().await,
            Some(Event::Core(CoreEvent::Trace { .. }))
        ));
        match f.events.recv().await {
            Some(Event::Fatal(message)) => assert!(message.ends_with(": boom"), "{}", message),
            _ => panic!("expected the TUI to fail"),
        }
    }

    #[tokio::test]
    async fn fails_if_the_loop_is_gone_from_the_start() {
        let mut f = Fixture::new().await;
        drop(f.mailbox);

        assert!(matches!(f.events.recv().await, Some(Event::Fatal(_))));
    }
}
