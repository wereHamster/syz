//! The sections of each ecosystem. Ecosystems share the building blocks in the parent module,
//! but what they put together is theirs to change.

pub mod cargo;
pub mod github_actions;
pub mod nix_flake;
mod nix_flake_health;
mod nix_flake_system;
pub mod npm;
