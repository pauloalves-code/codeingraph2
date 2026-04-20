//! codeingraph2 shared library. The two binaries (`codeingraph2` and
//! `mcp_server`) both consume these modules.

pub mod config;
pub mod db;
pub mod indexer;
pub mod watcher;
pub mod obsidian;
pub mod claudemd;
pub mod impact;
pub mod web;
