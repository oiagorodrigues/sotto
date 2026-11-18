//! Shared contract between the Swift sidecar, the Rust core and the React webview.
//!
//! Wire format rules: JSON, `snake_case` fields, internally tagged enums with a `type` field,
//! unknown fields rejected on everything the core receives. Times are milliseconds since the
//! session clock origin (`Started::t0_host_ns`) unless the field name says otherwise.

pub mod api;
pub mod onboarding;
pub mod sidecar;
pub mod ui;

pub use sidecar::{SidecarCommand, SidecarEvent, Speaker};
pub use ui::{AppSnapshot, Intent};
