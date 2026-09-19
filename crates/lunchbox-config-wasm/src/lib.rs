//! Comment-preserving config document model for the web config editor.
//!
//! See `README.md` for why the editor mutates a `toml_edit` document instead of
//! regenerating the file from a model.
//!
//! The model itself ([`doc::ConfigDoc`]) is plain Rust and is tested natively.
//! This file adds the browser binding, which is a thin wrapper: everything
//! crossing into JavaScript is a JSON string, so there is no schema to keep in
//! sync at the boundary and no `serde-wasm-bindgen` dependency.

pub mod doc;
pub mod path;
pub mod report;
pub mod windows;

pub use doc::{ConfigDoc, DocError, Patch};
pub use report::Report;

/// What build of the editor this is.
///
/// A struct rather than an ad-hoc JSON object so it has a generated TypeScript
/// mirror like everything else crossing this boundary.
#[derive(Debug, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Versions {
    /// The `config_version` this build validates against. A file declaring a
    /// different one comes back as a `version` report rather than being
    /// edited on a schema this build does not know.
    pub config_version: u32,
    /// The Lunchbox release this was built from.
    ///
    /// Worth showing because the standalone editor is deployed on its own
    /// subdomain, decoupled from any device: a stale cached bundle is
    /// indistinguishable from a current one until something disagrees with the
    /// daemon, and then the first question is which build was open.
    pub crate_version: String,
}

/// Schema version this build understands, and the crate version it came from.
pub fn versions_json() -> String {
    let versions = Versions {
        config_version: lunchbox_config::CURRENT_CONFIG_VERSION,
        crate_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    serde_json::to_string(&versions).expect("versions serialize")
}

#[cfg(target_arch = "wasm32")]
mod bindings {
    use super::*;
    use wasm_bindgen::prelude::*;

    /// Browser handle for a config document.
    #[wasm_bindgen(js_name = ConfigDoc)]
    pub struct JsConfigDoc {
        inner: ConfigDoc,
    }

    #[wasm_bindgen(js_class = ConfigDoc)]
    impl JsConfigDoc {
        /// Parse an existing file. Rejects if it is not valid TOML — the caller
        /// should surface that as a "this file could not be opened" error
        /// rather than starting from blank and silently discarding it.
        #[wasm_bindgen(js_name = open)]
        pub fn open(src: &str) -> Result<JsConfigDoc, JsError> {
            ConfigDoc::open(src)
                .map(|inner| JsConfigDoc { inner })
                .map_err(|e| JsError::new(&e.to_string()))
        }

        /// A minimal valid document, for "new config".
        #[wasm_bindgen(js_name = blank)]
        pub fn blank() -> JsConfigDoc {
            JsConfigDoc {
                inner: ConfigDoc::blank(),
            }
        }

        /// The live document text. This is exactly what would be saved.
        #[wasm_bindgen(js_name = text)]
        pub fn text(&self) -> String {
            self.inner.text()
        }

        /// `RawConfig` as a JSON string, for rendering. Errors when the
        /// document no longer fits the schema's shape, in which case the UI
        /// keeps showing the last good projection alongside the error.
        #[wasm_bindgen(js_name = view)]
        pub fn view(&self) -> Result<String, JsError> {
            self.inner.view().map_err(|e| JsError::new(&e.to_string()))
        }

        /// Validation report as a JSON string. Never fails; a document that
        /// will not parse comes back as a `syntax` report.
        #[wasm_bindgen(js_name = validate)]
        pub fn validate(&self) -> String {
            self.inner.validate()
        }

        /// Apply one patch, given as a JSON string. Returns whether the
        /// document changed — a no-op `set` costs no undo step.
        #[wasm_bindgen(js_name = apply)]
        pub fn apply(
            &mut self,
            patch_json: &str,
            coalesce_key: Option<String>,
        ) -> Result<bool, JsError> {
            let patch: Patch =
                serde_json::from_str(patch_json).map_err(|e| JsError::new(&e.to_string()))?;
            self.inner
                .apply(&patch, coalesce_key.as_deref())
                .map_err(|e| JsError::new(&e.to_string()))
        }

        /// Ends the current coalescing gesture. Called on pointer-up so the
        /// next drag starts its own undo step.
        #[wasm_bindgen(js_name = endGesture)]
        pub fn end_gesture(&mut self) {
            self.inner.end_gesture();
        }

        /// Replace the whole document, as when the raw TOML pane is edited.
        #[wasm_bindgen(js_name = replaceText)]
        pub fn replace_text(&mut self, src: &str) -> Result<bool, JsError> {
            self.inner
                .replace_text(src)
                .map_err(|e| JsError::new(&e.to_string()))
        }

        #[wasm_bindgen(js_name = undo)]
        pub fn undo(&mut self) -> bool {
            self.inner.undo()
        }

        #[wasm_bindgen(js_name = redo)]
        pub fn redo(&mut self) -> bool {
            self.inner.redo()
        }

        #[wasm_bindgen(js_name = canUndo)]
        pub fn can_undo(&self) -> bool {
            self.inner.can_undo()
        }

        #[wasm_bindgen(js_name = canRedo)]
        pub fn can_redo(&self) -> bool {
            self.inner.can_redo()
        }

        /// Per-day availability spans for one activity: its own windows, its
        /// group's, and the intersection the engine actually enforces.
        #[wasm_bindgen(js_name = availabilityForEntry)]
        pub fn availability_for_entry(&self, entry_id: &str) -> Result<String, JsError> {
            self.availability(entry_id, true)
        }

        /// The same, for a category's own windows.
        #[wasm_bindgen(js_name = availabilityForGroup)]
        pub fn availability_for_group(&self, group_id: &str) -> Result<String, JsError> {
            self.availability(group_id, false)
        }

        fn availability(&self, id: &str, is_entry: bool) -> Result<String, JsError> {
            let raw: lunchbox_config::RawConfig =
                toml::from_str(&self.inner.text()).map_err(|e| JsError::new(&e.to_string()))?;
            let view = if is_entry {
                windows::view_for_entry(&raw, id)
            } else {
                windows::view_for_group(&raw, id)
            }
            .ok_or_else(|| JsError::new(&format!("no such id: {id}")))?;
            serde_json::to_string(&view).map_err(|e| JsError::new(&e.to_string()))
        }
    }

    /// Schema and crate versions this build understands.
    #[wasm_bindgen(js_name = versions)]
    pub fn versions() -> String {
        versions_json()
    }
}
