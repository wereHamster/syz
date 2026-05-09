#[derive(Clone)]
pub enum Event {
    Trace {
        level: tracing::Level,
        message: String,
    },
}
