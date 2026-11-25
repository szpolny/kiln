//! Icons compiled into the binary, so nothing is loaded from disk or the network.
use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

pub const CLAUDE: &str = "icons/claude.svg";

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            CLAUDE => Some(Cow::Borrowed(include_bytes!("../assets/icons/claude.svg"))),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(vec![CLAUDE.into()])
    }
}
