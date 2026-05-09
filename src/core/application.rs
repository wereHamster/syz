use anyhow::Result;
use tokio::sync::{broadcast, mpsc};

use super::event::Event;
use super::message::Message;

pub struct Application {
    handle: Handle,

    mailbox: mpsc::Receiver<Message>,
}

#[derive(Clone)]
pub struct Handle {
    /// Clients send messages to this mailbox, which are then processed sequentially by
    /// the application.
    mailbox: mpsc::Sender<Message>,

    /// All events that the application generated are broadcast to this channel.
    events: broadcast::Sender<Event>,
}

impl Application {
    pub async fn new() -> Result<Self> {
        let (mailbox_tx, mailbox_rx) = mpsc::channel(100);
        let (events, _) = broadcast::channel(1000);

        Ok(Self {
            handle: Handle {
                mailbox: mailbox_tx,
                events,
            },

            mailbox: mailbox_rx,
        })
    }

    pub fn start(self) -> Handle {
        let handle = self.handle.clone();

        tokio::spawn(
            async move {
                tracing::info!("Event loop active");

                if let Err(e) = self.run().await {
                    tracing::error!("Application loop exited with error: {}", e);
                }
            }
        );

        handle
    }

    async fn run(mut self) -> Result<()> {
        while let Some(msg) = self.mailbox.recv().await {
            match msg {
                _ => todo!(),
            }
        }

        Ok(())
    }
}
