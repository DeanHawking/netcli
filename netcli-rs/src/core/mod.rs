//! NetCLI core. This layer implements parsing, completion, session state,
//! execution, rendering, and safe configuration transactions. The application
//! injects an immutable command registry; domain modules are not dependencies.
pub mod cli;
pub mod ctl;
pub mod exec;
pub mod json;
pub mod render;
pub mod session;
pub mod terminal;
pub mod tokenize;
