//! Sharing through Nyx Share: the Share service opens its Share Sheet for the
//! open document. With unsaved changes, a copy of the current state is shared
//! (through a private file removed at once), so the original isn't touched.

use nyx_share_client::{Client, Content, ShareOptions};
use std::os::unix::fs::DirBuilderExt;

pub fn document() {
    let Some(doc) = crate::doc::current() else { return };
    let content = if doc.dirty() {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("nyx-pdf-share");
        let copy = dir.join(doc.file_name());
        let made = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(anyhow::Error::from)
            .and_then(|()| doc.write_copy(&copy))
            .map_err(|e| e.to_string())
            .and_then(|()| {
                let mut content = Content::new();
                content.file(&copy).map_err(|e| e.to_string())?;
                Ok(content)
            });
        let _ = std::fs::remove_file(&copy);
        made
    } else {
        let mut content = Content::new();
        content.file(doc.path()).map(|_| ()).map_err(|e| e.to_string()).map(|()| content)
    };
    let content = match content {
        Ok(c) => c,
        Err(e) => return crate::window::toast(&format!("Couldn't share: {e}")),
    };
    crate::cmd::background(
        move || {
            let options = ShareOptions { app_name: Some("PDF".into()), ..Default::default() };
            Client::connect().and_then(|c| c.share(&content, &options)).map(|_| ()).map_err(|e| e.to_string())
        },
        |result| {
            if let Err(e) = result {
                crate::window::toast(&format!("Couldn't share: {e}"));
            }
        },
    );
}
