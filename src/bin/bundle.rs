//! Dev tool: builds the binaries and assembles `Ass.app`.
//!
//!     cargo run --bin bundle              # target/debug/app.noindex/Ass.app
//!     cargo run --bin bundle -- --release # target/release/app.noindex/Ass.app
//!
//! Spotlight does not list a folder whose name ends in `.noindex`, so a build never
//! shows up beside the app in use. `scripts/install.sh` copies a build to that place.
//!
//! The app is signed ad hoc from the inside out, so that macOS can verify the bundle. That needs
//! `/usr/bin/codesign` and `/usr/bin/plutil`, which are part of macOS.

use cef::build_util::mac::{BundleInfo, bundle};
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const APP_NAME: &str = "Ass";
const BUNDLE_ID: &str = "dev.ass.browser";
const HELPER_BIN: &str = "ass_helper";
const FRAMEWORK: &str = "Chromium Embedded Framework.framework";
/// Each helper app: what follows "Ass Helper" in its name, and what follows the app's bundle
/// identifier in its own, after CEF's convention.
const HELPERS: [(&str, &str); 5] = [
    ("", ".helper"),
    (" (Alerts)", ".helper.alerts"),
    (" (GPU)", ".helper.gpu"),
    (" (Plugin)", ".helper.plugin"),
    (" (Renderer)", ".helper.renderer"),
];

fn main() -> Result<(), Box<dyn Error>> {
    let release = env::args().any(|arg| arg == "--release");
    let profile = if release { "release" } else { "debug" };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = root.join("target").join(profile);

    let mut build = Command::new(env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    build
        .current_dir(&root)
        .args(["build", "--bin", "ass", "--bin", HELPER_BIN]);
    if release {
        build.arg("--release");
    }
    // `cargo run` exports this package's CARGO_PKG_* / CARGO_MANIFEST_* variables.
    // Some build scripts (ring) track them, so leaking them into the nested build
    // would recompile most dependencies on every run.
    for (name, _) in env::vars_os() {
        let name_str = name.to_string_lossy();
        if name_str.starts_with("CARGO_PKG_") || name_str.starts_with("CARGO_MANIFEST_") {
            build.env_remove(&name);
        }
    }
    if !build.status()?.success() {
        return Err("cargo build failed".into());
    }

    // The bundler names the app after its executable, so stage the binary as "Ass".
    let stage = target.join("bundle-stage");
    fs::create_dir_all(&stage)?;
    fs::copy(target.join("ass"), stage.join(APP_NAME))?;
    fs::copy(target.join(HELPER_BIN), stage.join(HELPER_BIN))?;

    let info = BundleInfo::new(
        APP_NAME,
        BUNDLE_ID,
        APP_NAME,
        "English",
        semver::Version::parse(env!("CARGO_PKG_VERSION"))?,
    );
    // Everything in `resources/mac/` is copied into the app; `Ass.icns` there is its icon.
    let resources = root.join("resources");
    let out = target.join("app.noindex");
    let app = bundle(&out, &stage, APP_NAME, HELPER_BIN, Some(resources), info)?;
    sign(&app)?;
    println!("{}", app.display());
    Ok(())
}

/// Signs the app ad hoc, from the inside out: the framework's libraries, the framework, each
/// helper, then the app. The binaries come with the linker's signature only, which binds no
/// `Info.plist` and seals no resources, and macOS does not accept such a bundle as verified
/// (`docs/ARCHITECTURE.md`, known issue 1).
fn sign(app: &Path) -> Result<(), Box<dyn Error>> {
    let frameworks = app.join("Contents/Frameworks");
    let framework = frameworks.join(FRAMEWORK);
    let mut libraries = Vec::new();
    for entry in fs::read_dir(framework.join("Libraries"))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "dylib")
        {
            libraries.push(path);
        }
    }
    libraries.sort();
    for library in &libraries {
        codesign(&["--force", "--sign", "-"], library)?;
    }
    codesign(&["--force", "--sign", "-"], &framework)?;
    for (name, identifier) in HELPERS {
        let helper = frameworks.join(format!("{APP_NAME} Helper{name}.app"));
        // The pinned bundler gives every helper the app's own identifier.
        let identifier = format!("{BUNDLE_ID}{identifier}");
        run(Command::new("/usr/bin/plutil")
            .args(["-replace", "CFBundleIdentifier", "-string", &identifier])
            .arg(helper.join("Contents/Info.plist")))?;
        codesign(&["--force", "--sign", "-"], &helper)?;
    }
    codesign(&["--force", "--sign", "-"], app)?;
    codesign(&["--verify", "--deep", "--strict"], app)
}

fn codesign(args: &[&str], path: &Path) -> Result<(), Box<dyn Error>> {
    run(Command::new("/usr/bin/codesign").args(args).arg(path))
}

/// Runs a command and shows what it printed only when it fails.
fn run(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    let program = command.get_program().to_string_lossy().into_owned();
    let printed = String::from_utf8_lossy(&output.stderr);
    Err(format!("{program} failed: {}", printed.trim()).into())
}
