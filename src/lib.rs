//! Backblaze B2 buckets for the Flea file manager.
//!
//! The `flea-b2` binary is the plugin and its transfer worker. The bucket manager in `manager/`
//! is a separate binary that edits the bucket list and the keyring through [`manage`].
pub mod config;
pub mod error;
pub mod manage;
pub mod plugin;

mod b2;
mod creds;
mod jobs;
mod launch;
mod listing;
mod notify;
mod ops;
mod paths;
mod util;
mod worker;
