# Ass

A web browser for macOS. Chromium draws the pages. A small Rust program around it is the browser itself: the window, the tabs, the address field and the profile.

## Status

Early. It runs and it loads the web, and nobody has used it for a full day yet. Do not move your banking into it.

We build and test it on one machine, an Apple-silicon Mac on macOS 27.2, and on nothing else so far.

## What works

- Tabs in a sidebar, up to 500. A popup opens as a tab.
- An address field that takes an address or a search. Back and forward work, and so does reload.
- Cookies survive a restart.
- Every helper process runs inside the macOS sandbox: renderer, GPU, network, storage. The smoke test checks each one.

## What does not

- MP4 video. The Chromium inside has no H.264 and no AAC, so most video sites will not play. VP9 and AV1 do.
- The app cancels every download. It refuses camera and microphone requests, and any other permission request, without showing a prompt.
- A page's pop-up dialogs (`alert`, `confirm`, `prompt`) are answered without being shown, and a site that asks for a browser password gets its own 401 page. Showing those dialogs crashed the app when their tab closed, so for now there are none.
- No passkeys or security keys. Extensions load only from the command line and have no buttons. Nothing checks a page against a phishing or malware list.
- After a quit the tabs are gone. They have no icons either.
- Only a new CEF build moves the Chromium inside forward. On 2026-11-23 it was one Chrome security update behind.
- The bundler signs each build ad hoc, so after every build macOS asks again for the keychain item that holds the cookie key. Nobody has run the code that handles that prompt against a real keychain.
- Chromium's own command-line switches still work, `--no-sandbox` and `--remote-debugging-port` among them, because we have no policy for them yet.
- Under heavy load a quit can take 12 seconds and end with exit code 2.

That is the short version. `docs/ARCHITECTURE.md` lists 26 known issues, each with its cause where we know it.

## Build

You need Rust (`rust-toolchain.toml` pins the version we build with, 1.96.0) and two build tools, `cmake` and `ninja`. Xcode is not needed.

```
cargo run --bin bundle                # target/debug/app.noindex/Ass.app
cargo run --bin bundle -- --release   # target/release/app.noindex/Ass.app
```

The first build downloads CEF, the packaged Chromium, into `.cef/`: about 470 MB. Nothing of ours pins that download yet. It is checked against a hash that comes from the same server (known issue 13).

To try the app without touching a real profile or your keychain, give it a new, empty profile folder. The folder must exist before the app starts:

```
P="$(mktemp -d)" && ASS_PROFILE_DIR="$P" target/debug/app.noindex/Ass.app/Contents/MacOS/Ass --use-mock-keychain
```

`--use-mock-keychain` encrypts that profile's cookies with a fixed test key, so do not sign in to accounts you care about. Remove the folder afterwards with `rm -rf "$P"`.

## Test

```
cargo fmt --check && cargo clippy --bins -- -D warnings && cargo test
scripts/smoke.sh
```

`scripts/smoke.sh` runs the app in an invisible window, off your screen and your keyboard, and makes 22 checks. Two longer tests work the same way:

```
scripts/quit-stall-test.sh
scripts/leftover-test.sh
```

The tab test and the shell channel test are not published yet (known issue 15 in `docs/ARCHITECTURE.md`).

All three need Python 3 and `swiftc` from Apple's Command Line Tools.

## Where things are

`docs/ARCHITECTURE.md` is the technical record: the processes, a map of every file, each decision with its reason, and what was verified by running as opposed to only read. For the look, read `docs/DESIGN.md`, or open `design/shell-prototype.html`, which is the design as one working page.

## Security

Report a security problem in private, through "Report a vulnerability" on this repository's Security tab. Please do not open a public issue for one.

## License

BSD 3-Clause, see `LICENSE`. Chromium and CEF come under their own licenses.
