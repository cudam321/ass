//! The internal `ass://` scheme, which serves the browser's own pages.
//!
//! CEF requires a custom scheme to be registered identically in every
//! process, so the browser and the helper both call [`register`] from
//! `App::on_register_custom_schemes`. Serving the scheme, and deciding who may
//! use it, happens in the browser process only (`src/shell.rs`).

use cef::*;

/// The scheme of the browser's own pages.
pub const SCHEME: &str = "ass";
/// The host of the shell UI.
pub const SHELL_HOST: &str = "shell";
/// The shell page. It is the only URL the shell view may show.
pub const SHELL_URL: &str = "ass://shell/";

/// Registers the `ass` scheme with these properties:
///
/// - `STANDARD`: URLs parse like `http` URLs, so `ass://shell` is a real origin.
/// - `SECURE`: treated like `https` (secure context, no mixed content).
/// - `DISPLAY_ISOLATED`: pages of other schemes cannot link to it, frame it or
///   embed it.
/// - `FETCH_ENABLED`: its pages may call `fetch()` on their own origin.
///
/// Deliberately left out: `CORS_ENABLED` (no other origin may send it
/// requests), `CSP_BYPASSING` and `LOCAL`.
pub fn register(registrar: Option<&mut SchemeRegistrar>) {
    let Some(registrar) = registrar else {
        return;
    };
    let options = SchemeOptions::STANDARD.get_raw()
        | SchemeOptions::SECURE.get_raw()
        | SchemeOptions::DISPLAY_ISOLATED.get_raw()
        | SchemeOptions::FETCH_ENABLED.get_raw();
    let registered = registrar.add_custom_scheme(Some(&CefString::from(SCHEME)), options as i32);
    assert_eq!(registered, 1, "cannot register the {SCHEME} scheme");
}
