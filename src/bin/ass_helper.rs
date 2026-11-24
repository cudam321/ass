//! Sub-process executable (renderer, GPU, utility). Runs inside the sandbox.

use cef::{args::Args, *};

wrap_app! {
    struct HelperApp;

    impl App {
        fn on_register_custom_schemes(&self, registrar: Option<&mut SchemeRegistrar>) {
            // Must match the browser process: see `ass::scheme`.
            ass::scheme::register(registrar);
        }
    }
}

fn main() {
    let args = Args::new();

    // The sandbox must be initialized before the framework is loaded.
    let _sandbox = {
        let mut sandbox = cef::sandbox::Sandbox::new();
        sandbox.initialize(args.as_main_args());
        sandbox
    };

    let _loader = {
        let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), true);
        assert!(loader.load(), "cannot load the CEF framework");
        loader
    };

    // Initialize the CEF API version.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    let mut app = HelperApp::new();
    execute_process(
        Some(args.as_main_args()),
        Some(&mut app),
        std::ptr::null_mut(),
    );
}
