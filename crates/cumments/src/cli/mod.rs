//! CLI subcommands for Cumments.

mod args;
mod output;
mod projection;
mod registration;
mod rooms;
mod sites;
#[cfg(test)]
mod test_support;

pub use args::{
    Commands, ProjectionArgs, ProjectionCommand, RoomsCommand, SitesCommand, handle_completions,
};
pub use output::print_json;
pub use projection::handle_projection_command;
pub use registration::{handle_appservice_command, handle_generate_registration};
pub use rooms::{handle_rooms_command, handle_rooms_upgrade_command};
pub use sites::handle_sites_command;
