//! Tauri commands, grouped by area. Argument names are camelCase on the
//! JavaScript side (`hostId`, `maxBytes`); see `ui/lib/api.ts`.

mod dragout;
mod edit;
mod files;
mod forwards;
mod hosts;
mod search;
mod settings;
mod terminals;
mod transfers;

pub use dragout::*;
pub use edit::*;
pub use files::*;
pub use forwards::*;
pub use hosts::*;
pub use search::*;
pub use settings::*;
pub use terminals::*;
pub use transfers::*;
