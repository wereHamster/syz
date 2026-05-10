#[derive(Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum Event {
    Trace {
        #[serde(with = "level_serde")]
        level: tracing::Level,
        message: String,
    },
}

mod level_serde {
    use serde::Serializer;
    use tracing::Level;

    pub fn serialize<S>(level: &Level, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(level.as_str())
    }
}
