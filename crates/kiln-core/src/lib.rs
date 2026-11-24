//! Domain model for Kiln. No UI and no I/O: everything here is plain data
//! so the privacy policy can be tested without a window.

pub mod policy;
pub mod store;

pub use store::{Message, Project, Role, Status, Store, Thread};
