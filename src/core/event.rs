#[derive(Clone, serde::Serialize)]
#[serde(tag = "type")]
pub enum Event {
    Trace {
        #[serde(with = "level_serde")]
        level: tracing::Level,
        message: String,
    },

    Commit {
        ops: Vec<Op>,
    },
}

#[derive(Clone, serde::Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Op {
    Upsert {
        path: String,
        data: serde_json::Value,
    },
    Delete {
        path: String,
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
