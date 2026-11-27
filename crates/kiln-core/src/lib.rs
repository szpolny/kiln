//! Domain model for Kiln. No UI and no I/O: everything here is plain data
//! so the privacy policy can be tested without a window.

pub mod checkout;
pub mod policy;
pub mod settings;
pub mod store;
pub mod title;

pub use checkout::Checkout;
pub use settings::{Appearance, Fetch, FollowUp, Settings};
pub use store::{
    Agent, Attachment, AttachmentContent, Prompt, Effort, Message, Project, Role, Status, Store, Thread, NEW_THREAD_TITLE,
};
