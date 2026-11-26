//! Domain model for Kiln. No UI and no I/O: everything here is plain data
//! so the privacy policy can be tested without a window.

pub mod persist;
pub mod policy;
pub mod settings;
pub mod store;

pub use settings::{Appearance, Settings};
pub use store::{
    Agent, Effort, Message, Project, Role, Status, Store, Thread, NEW_THREAD_TITLE,
};
