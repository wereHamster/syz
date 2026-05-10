pub mod events;
pub mod messages;
pub mod root;

#[derive(Clone)]
pub struct AppState {
    pub handle: crate::core::application::Handle,
    pub token: String,
}
