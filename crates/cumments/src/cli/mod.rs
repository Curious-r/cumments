//! CLI subcommands for Cumments.

mod args;
mod error;
mod output;
mod pages;
mod projection;
mod registration;
mod rooms;
mod sites;
#[cfg(test)]
mod test_support;

pub use args::*;
pub use error::{CliError, CliErrorKind, CliResult};
pub use output::{print_json, print_list};
pub use pages::handle_pages_command;
pub use projection::handle_projection_repairs_command;
pub use registration::{handle_appservice_command, handle_generate_registration};
pub use rooms::{
    handle_quarantined_rooms_command, handle_rooms_command, handle_rooms_upgrade_command,
};
pub use sites::handle_sites_command;
