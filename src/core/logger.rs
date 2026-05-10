use tokio::sync::broadcast;
use tracing::Subscriber;
use tracing_subscriber::Layer;

use crate::core::event::Event;

pub struct CoreLogLayer {
    tx: broadcast::Sender<Event>,
}

impl CoreLogLayer {
    pub fn new(tx: broadcast::Sender<Event>) -> Self {
        Self { tx }
    }
}

impl<S: Subscriber> Layer<S> for CoreLogLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = StringVisitor::new();
        event.record(&mut visitor);

        let trace_event = Event::Trace {
            level: *event.metadata().level(),
            message: visitor.message,
        };

        // We ignore send errors because there might not be any subscribers yet
        let _ = self.tx.send(trace_event);
    }
}

struct StringVisitor {
    message: String,
}

impl StringVisitor {
    fn new() -> Self {
        Self {
            message: String::new(),
        }
    }
}

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        }
    }
}
