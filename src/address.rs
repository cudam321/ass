//! What the address input does with the text a person types.
//!
//! [`resolve`] turns it into one URL to load, or refuses it:
//!
//! - Text that starts with `http:` or `https:` is parsed as it is.
//! - Text that reads as a host (`example.com`, `localhost:3000`, `127.0.0.1`,
//!   `[::1]`), with an optional port and path, is an address.
//! - Any other text with a scheme (`javascript:`, `file:`, `data:`, `ass:`) is
//!   refused. The address input opens web pages and nothing else.
//! - Everything else is a search, sent to [`SEARCH_URL`].
//!
//! Every address is in the canonical form the `url` crate gives it (the WHATWG
//! URL Standard).
//!
//! Typed text can hold a password. Nothing here logs it, and a refusal's
//! message never repeats it. Text that reads as an address with a user name or
//! password is refused, not searched: a search would send it to the search
//! engine. That covers text with something before an `@` ahead of any path,
//! whether or not it parses. Text with a space in it is a search without that
//! check (the test `rule_order_surprises` shows it).
//!
//! [`web_address`] is the same check for a URL a web page supplied, such as
//! the target of a Cmd+clicked link.

use url::{Host, Url, form_urlencoded};

/// Where a search goes. A placeholder: the default search engine is not chosen yet.
pub const SEARCH_URL: &str = "https://www.google.com/search?q=";

/// What typed text resolves to. Both hold the full URL to load.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// The text was an address, in its canonical form.
    Address(String),
    /// The text was a search: [`SEARCH_URL`] followed by the text, encoded.
    Search(String),
}

impl Resolved {
    /// The URL to load.
    pub fn url(&self) -> &str {
        match self {
            Self::Address(url) | Self::Search(url) => url,
        }
    }

    /// Which way the text went, as the shell page names it.
    pub fn went(&self) -> &'static str {
        match self {
            Self::Address(_) => "address",
            Self::Search(_) => "search",
        }
    }
}

/// Why typed text was neither opened nor searched.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Nothing but whitespace.
    Empty,
    /// A control character inside the text. A URL parser drops tabs and
    /// newlines without a word, so what loaded would not be what was typed.
    ControlCharacter,
    /// An address with a user name or password.
    Credentials,
    /// A scheme other than `http` or `https`.
    Scheme,
    /// Starts with `http:` or `https:` but is not a valid web address.
    Invalid,
}

impl Refused {
    /// Fixed text to show the person who typed: a short fragment the shell
    /// page shows with its first letter capitalised. It never contains what
    /// they typed, which may hold a password.
    pub fn message(&self) -> &'static str {
        match self {
            Self::Empty => "type an address or a search",
            Self::ControlCharacter => "that text has a control character in it",
            Self::Credentials => "that address has a user name or password in it",
            Self::Scheme => "only http and https addresses open here",
            Self::Invalid => "that is not a valid web address",
        }
    }
}

/// Resolves what was typed in the address input. The rules, in order:
///
/// 1. Leading and trailing whitespace is trimmed. Nothing left is refused.
/// 2. A control character left anywhere is refused.
/// 3. Text that starts with `http:` or `https:` is parsed as it is.
/// 4. Text with whitespace inside is a search.
/// 5. Text that reads as a host, with an optional port and path, is an
///    address: `localhost`, a domain with a dot, four dotted groups of digits,
///    or a bracketed IPv6 address. With a user name or password, refused.
/// 6. Text with something before an `@` ahead of any path is refused, even
///    when it does not parse.
/// 7. Other text with a scheme is refused. The rest is a search.
pub fn resolve(input: &str) -> Result<Resolved, Refused> {
    let text = input.trim();
    if text.is_empty() {
        return Err(Refused::Empty);
    }
    if text.chars().any(char::is_control) {
        return Err(Refused::ControlCharacter);
    }
    let scheme = typed_scheme(text);
    if scheme.is_some_and(|s| s.eq_ignore_ascii_case("http") || s.eq_ignore_ascii_case("https")) {
        return typed_web_address(text);
    }
    if text.chars().any(char::is_whitespace) {
        return Ok(search(text));
    }
    // The host check comes before the scheme check on purpose: to a URL
    // parser, `localhost:8080` and `example.com:8080` start with a scheme.
    if let Some(address) = host_address(text)? {
        return Ok(Resolved::Address(address));
    }
    if has_typed_user(text) {
        return Err(Refused::Credentials);
    }
    if let Some(scheme) = scheme {
        // `example.com:65536`: a host whose port the parser refused.
        let host = scheme.contains('.') || scheme.eq_ignore_ascii_case("localhost");
        return Err(if host {
            Refused::Invalid
        } else {
            Refused::Scheme
        });
    }
    Ok(search(text))
}

/// Rule 6: an `@` with something before it, ahead of any path, query or
/// fragment. A lone leading `@` (`@username`) is a search.
fn has_typed_user(text: &str) -> bool {
    let authority = text.split(['/', '?', '#', '\\']).next().unwrap_or("");
    authority.rfind('@').is_some_and(|at| at > 0)
}

/// For a URL a web page supplied (a Cmd+clicked link): Some(canonical) only for an absolute
/// http or https URL with a host and no user or password; None otherwise.
pub fn web_address(url: &str) -> Option<String> {
    let url = Url::parse(url).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    (web && url.has_host() && !has_credentials(&url)).then(|| url.into())
}

/// Rule 3: the scheme was typed, so the text is parsed as it is.
fn typed_web_address(text: &str) -> Result<Resolved, Refused> {
    let url = Url::parse(text).map_err(|_| Refused::Invalid)?;
    if has_credentials(&url) {
        return Err(Refused::Credentials);
    }
    if !url.has_host() {
        return Err(Refused::Invalid);
    }
    Ok(Resolved::Address(url.into()))
}

/// Rule 5: reads `text` the way it would be read after `https://`. The
/// decision rests on the host the parser found, not on the text, because the
/// parser decides what is loaded. `Ok(None)` when it is not a host.
fn host_address(text: &str) -> Result<Option<String>, Refused> {
    let Ok(url) = Url::parse(&format!("https://{text}")) else {
        return Ok(None);
    };
    // Checked before the host: text that parses with a user name or password
    // is refused whatever its host, so it is never searched.
    if has_credentials(&url) {
        return Err(Refused::Credentials);
    }
    let scheme = match url.host() {
        Some(Host::Domain("localhost")) => "http",
        Some(Host::Domain(domain)) if domain.contains('.') => "https",
        Some(Host::Ipv4(_)) if is_dotted_quad(text) => "http",
        Some(Host::Ipv6(_)) => "http",
        _ => return Ok(None),
    };
    // Parsed again with the scheme it loads with, so that only that scheme's
    // default port is dropped: `localhost:443` stays `http://localhost:443/`.
    let url = Url::parse(&format!("{scheme}://{text}")).map_err(|_| Refused::Invalid)?;
    Ok(Some(url.into()))
}

/// True when the host in `text` is written as four dot-separated groups of 1
/// to 3 ASCII digits. The parser also reads `1.5`, `3.14`, `2130706433` and
/// `0x7f.0.0.1` as IPv4 addresses; typed, those are searches.
fn is_dotted_quad(text: &str) -> bool {
    let host = text.split([':', '/', '?', '#', '\\']).next().unwrap_or("");
    host.split('.').count() == 4
        && host
            .split('.')
            .all(|part| (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()))
}

/// The scheme at the start of `text`, if it has one: a letter, then letters,
/// digits, `+`, `-` or `.`, then `:`.
fn typed_scheme(text: &str) -> Option<&str> {
    let (scheme, _) = text.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    let valid = first.is_ascii_alphabetic()
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    valid.then_some(scheme)
}

fn has_credentials(url: &Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

fn search(text: &str) -> Resolved {
    let mut url = String::from(SEARCH_URL);
    url.extend(form_urlencoded::byte_serialize(text.as_bytes()));
    Resolved::Search(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(url: &str) -> Result<Resolved, Refused> {
        Ok(Resolved::Address(url.to_owned()))
    }

    /// `query` is the expected encoded form of the typed text.
    fn searched(query: &str) -> Result<Resolved, Refused> {
        Ok(Resolved::Search(format!("{SEARCH_URL}{query}")))
    }

    fn check(cases: &[(&str, Result<Resolved, Refused>)]) {
        for (input, expected) in cases {
            assert_eq!(&resolve(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn http_and_https_are_parsed_as_typed() {
        check(&[
            ("http://example.com", address("http://example.com/")),
            (
                "https://example.com/a?b=c#d",
                address("https://example.com/a?b=c#d"),
            ),
            ("HTTPS://EXAMPLE.COM", address("https://example.com/")),
            (
                "Http://Example.com/Path",
                address("http://example.com/Path"),
            ),
            ("https:\\\\example.com", address("https://example.com/")),
            ("http:example.com", address("http://example.com/")),
            ("https:/example.com", address("https://example.com/")),
            ("https://127.0.0.1", address("https://127.0.0.1/")),
            ("http://[::1]:8080/x", address("http://[::1]:8080/x")),
            ("http://localhost:8080", address("http://localhost:8080/")),
            ("http://example.com:80", address("http://example.com/")),
            ("https://bücher.de", address("https://xn--bcher-kva.de/")),
            // With the scheme typed, the parser's reading of the host stands.
            ("http://2130706433", address("http://127.0.0.1/")),
            ("http://", Err(Refused::Invalid)),
            ("https:", Err(Refused::Invalid)),
            ("https://exa mple.com", Err(Refused::Invalid)),
            ("http://[::1", Err(Refused::Invalid)),
            ("http://256.0.0.1", Err(Refused::Invalid)),
        ]);
    }

    #[test]
    fn other_schemes_are_refused() {
        for input in [
            "javascript:alert(1)",
            "JavaScript:alert(document.cookie)",
            "javascript://example.com/%0Aalert(1)",
            "file:///etc/passwd",
            "about:blank",
            "data:text/html,x",
            "data:text/html;base64,PHNjcmlwdD4=",
            "ass://shell/",
            "view-source:https://a.com",
            "ftp://a.b",
            "ws://example.com",
            "chrome://settings",
            "blob:https://example.com/0",
            "mailto:",
            "mailto:someone",
            "tel:+15555550100",
            "C:\\Windows",
            "c:/Windows",
        ] {
            assert_eq!(resolve(input), Err(Refused::Scheme), "input {input:?}");
        }
    }

    #[test]
    fn credentials_are_refused() {
        for input in [
            "user:pass@example.com",
            "user@example.com",
            "http://u:p@example.com",
            "https://u@example.com",
            "HTTPS://U:P@EXAMPLE.COM",
            "https://:p@example.com",
            "user:pass@example.com:8080/path",
            "admin:hunter2@localhost:3000",
            "admin:hunter2@192.168.0.1",
            "root@[::1]:22",
            "user@example",
            "a@b@example.com",
            // Refused either way; the `@` makes it a password, not a scheme.
            "mailto:someone@example.com",
        ] {
            assert_eq!(resolve(input), Err(Refused::Credentials), "input {input:?}");
        }
        check(&[
            // An `@` after the host is part of the path, query or fragment.
            ("example.com/@user", address("https://example.com/@user")),
            (
                "example.com?to=a@b.com",
                address("https://example.com/?to=a@b.com"),
            ),
            ("example.com#a@b", address("https://example.com/#a@b")),
            // The parser drops an empty user and password; nothing is sent.
            ("https://:@example.com", address("https://example.com/")),
            ("@username", searched("%40username")),
        ]);
    }

    #[test]
    fn hosts() {
        check(&[
            ("example.com", address("https://example.com/")),
            ("www.example.com", address("https://www.example.com/")),
            (
                "a.b.c.example.co.uk",
                address("https://a.b.c.example.co.uk/"),
            ),
            ("docs.rs/url/2.5.9", address("https://docs.rs/url/2.5.9")),
            (
                "EXAMPLE.com/Path?Q=1#F",
                address("https://example.com/Path?Q=1#F"),
            ),
            ("example.com.", address("https://example.com./")),
            ("localhost", address("http://localhost/")),
            ("LOCALHOST", address("http://localhost/")),
            ("localhost/admin", address("http://localhost/admin")),
            ("bücher.de", address("https://xn--bcher-kva.de/")),
            ("BÜCHER.de", address("https://xn--bcher-kva.de/")),
            ("xn--bcher-kva.de", address("https://xn--bcher-kva.de/")),
            (
                "münchen.de/straße",
                address("https://xn--mnchen-3ya.de/stra%C3%9Fe"),
            ),
            // A single word is a search, whatever the parser makes of it.
            ("hello", searched("hello")),
            ("intranet", searched("intranet")),
            ("localhostx", searched("localhostx")),
            ("xn--bcher-kva", searched("xn--bcher-kva")),
            ("café", searched("caf%C3%A9")),
        ]);
    }

    #[test]
    fn ip_literals() {
        check(&[
            ("127.0.0.1", address("http://127.0.0.1/")),
            (
                "192.168.1.1:8080/admin",
                address("http://192.168.1.1:8080/admin"),
            ),
            ("10.0.0.1:80", address("http://10.0.0.1/")),
            ("0.0.0.0", address("http://0.0.0.0/")),
            ("127.0.0.1?x#y", address("http://127.0.0.1/?x#y")),
            ("[::1]", address("http://[::1]/")),
            ("[::1]:8080", address("http://[::1]:8080/")),
            ("[2001:DB8::1]/x", address("http://[2001:db8::1]/x")),
            ("[::ffff:192.168.0.1]", address("http://[::ffff:c0a8:1]/")),
            // Numbers the parser would read as IPv4 addresses.
            ("1.5", searched("1.5")),
            ("3.14", searched("3.14")),
            ("127.1", searched("127.1")),
            ("1.2.3", searched("1.2.3")),
            ("2130706433", searched("2130706433")),
            ("0x7f.0.0.1", searched("0x7f.0.0.1")),
            // Parts out of range: the parse fails.
            ("256.1.1.1", searched("256.1.1.1")),
            ("1.2.3.256", searched("1.2.3.256")),
            ("1234.1.1.1", searched("1234.1.1.1")),
            ("999.999.999.999", searched("999.999.999.999")),
            ("1.2.3.4.5", searched("1.2.3.4.5")),
            ("[::1", searched("%5B%3A%3A1")),
            ("::1", searched("%3A%3A1")),
        ]);
    }

    #[test]
    fn ports() {
        check(&[
            ("localhost:0", address("http://localhost:0/")),
            ("example.com:0", address("https://example.com:0/")),
            ("example.com:65535", address("https://example.com:65535/")),
            ("127.0.0.1:65535", address("http://127.0.0.1:65535/")),
            ("[::1]:65535", address("http://[::1]:65535/")),
            ("localhost:3000/x", address("http://localhost:3000/x")),
            ("localhost:80", address("http://localhost/")),
            ("localhost:443", address("http://localhost:443/")),
            ("example.com:443", address("https://example.com/")),
            ("example.com:80", address("https://example.com:80/")),
            ("https://example.com:0", address("https://example.com:0/")),
            ("http://example.com:65536", Err(Refused::Invalid)),
        ]);
    }

    /// Results that follow from the order of the rules and the parser, and
    /// that a person might not expect. Pinned here so a change of rule changes
    /// them in one place.
    #[test]
    fn rule_order_surprises() {
        check(&[
            // A bad port fails the host parse; the text before it is a host.
            ("example.com:65536", Err(Refused::Invalid)),
            ("localhost:65536", Err(Refused::Invalid)),
            // Without a letter in front, nothing looks like a scheme.
            ("127.0.0.1:65536", searched("127.0.0.1%3A65536")),
            ("[::1]:65536", searched("%5B%3A%3A1%5D%3A65536")),
            // A search operator with no space reads as a scheme.
            ("site:example.com", Err(Refused::Scheme)),
            ("mailto:a@b.com", Err(Refused::Credentials)),
            // After `https://`, leading slashes and a leading `@` are dropped.
            ("/foo.bar", address("https://foo.bar/")),
            ("//example.com", address("https://example.com/")),
            ("@example.com", address("https://example.com/")),
            // A leading zero is octal to the parser.
            ("010.0.0.1", address("http://8.0.0.1/")),
            ("08.0.0.1", searched("08.0.0.1")),
            ("1.2.3.4.", searched("1.2.3.4.")),
            // Any dot makes a domain, empty labels included.
            ("e.g.", address("https://e.g./")),
            ("etc.", address("https://etc./")),
            // Credentials do reach the search engine when a space comes first
            // (rule 4).
            (
                "user:pass@example.com x",
                searched("user%3Apass%40example.com+x"),
            ),
            // Text that does not parse is still refused (rule 6).
            ("u_1:pw@example.com:99999", Err(Refused::Credentials)),
            ("admin@router:99999/x", Err(Refused::Credentials)),
            ("a@", Err(Refused::Credentials)),
        ]);
    }

    #[test]
    fn spaces() {
        check(&[
            ("  example.com  ", address("https://example.com/")),
            (
                "\u{3000}example.com\u{3000}",
                address("https://example.com/"),
            ),
            ("hello world", searched("hello+world")),
            ("  rust  url  ", searched("rust++url")),
            ("example.com/a b", searched("example.com%2Fa+b")),
            ("a\u{a0}b", searched("a%C2%A0b")),
            (
                "http://example.com/a b",
                address("http://example.com/a%20b"),
            ),
            (
                "https://example.com/?q=a b",
                address("https://example.com/?q=a%20b"),
            ),
        ]);
    }

    #[test]
    fn empty() {
        for input in ["", " ", "\n\t\r ", "\u{3000}\u{85}"] {
            assert_eq!(resolve(input), Err(Refused::Empty), "input {input:?}");
        }
    }

    #[test]
    fn control_characters() {
        for input in [
            "\0",
            "example.com\0",
            "a\u{7}b",
            "\u{1b}[31mred",
            "example\u{7f}.com",
            "a\u{85}b",
            "a\u{9b}b",
            "a\tb",
            "example.com\nevil.com",
            "http://example.com/\r\nSet-Cookie: a=b",
        ] {
            assert_eq!(
                resolve(input),
                Err(Refused::ControlCharacter),
                "input {input:?}"
            );
        }
        // Leading and trailing whitespace, U+0085 included, is trimmed first.
        check(&[
            ("example.com\n", address("https://example.com/")),
            ("\r\nexample.com\r\n", address("https://example.com/")),
            ("\u{85}example.com\u{85}", address("https://example.com/")),
            ("\t localhost \t", address("http://localhost/")),
        ]);
    }

    #[test]
    fn long_input() {
        let words = ["lorem"; 2731];
        assert!(words.join(" ").len() >= 16 * 1024);
        assert_eq!(resolve(&words.join(" ")), searched(&words.join("+")));

        let word = "a".repeat(16 * 1024);
        assert_eq!(resolve(&word), searched(&word));

        // Past 1000 characters the parser refuses a non-ASCII host, so this
        // falls through to a search.
        let accents = "é".repeat(2000);
        assert_eq!(resolve(&accents), searched(&"%C3%A9".repeat(2000)));

        let path = "a".repeat(100_000);
        let url = format!("https://example.com/{path}");
        assert_eq!(resolve(&url), address(&url));
        assert_eq!(resolve(&format!("example.com/{path}")), address(&url));
    }

    #[test]
    fn search_encoding() {
        check(&[
            ("a+b&c=d", searched("a%2Bb%26c%3Dd")),
            ("café", searched("caf%C3%A9")),
            ("#hash", searched("%23hash")),
            ("#x", searched("%23x")),
            ("?q", searched("%3Fq")),
            ("/path", searched("%2Fpath")),
            ("🦀", searched("%F0%9F%A6%80")),
            ("rust 🦀", searched("rust+%F0%9F%A6%80")),
            ("100%", searched("100%25")),
            ("c++", searched("c%2B%2B")),
        ]);
    }

    /// Whatever the input, an address is an http or https URL and a search
    /// goes to the search engine, with no whitespace or control character in
    /// either.
    #[test]
    fn every_result_is_a_web_url() {
        let inputs = [
            "[",
            "]",
            "[]",
            "[:]",
            ":",
            "::",
            "@",
            "@@",
            ".",
            "..",
            "%",
            "%%",
            "\\",
            "\\\\server\\share",
            "a@",
            "user:pass@",
            "http",
            "https",
            "xn--",
            "xn--a",
            "a..b",
            "ß",
            "İstanbul.com",
            "\u{fffd}",
            "\u{10ffff}",
            "\u{feff}example.com",
            "\u{200b}",
            "example.com:",
            "localhost:",
            "[::1]:",
            "1.2.3.4:",
            "a:b:c",
            "https.com:443",
            "http.example.com",
            "javascript.com:1",
            "web+foo.bar:123",
            "12:30",
            "TODO:",
            "e.g",
            "..com",
            "-.com",
            "a b@c.d",
            "http://a b",
        ];
        for input in inputs {
            let url = match resolve(input) {
                Ok(Resolved::Address(url)) => {
                    assert!(
                        url.starts_with("http://") || url.starts_with("https://"),
                        "input {input:?} gave {url:?}"
                    );
                    url
                }
                Ok(Resolved::Search(url)) => {
                    assert!(url.starts_with(SEARCH_URL), "input {input:?} gave {url:?}");
                    url
                }
                Err(_) => continue,
            };
            assert!(
                !url.chars().any(|c| c.is_whitespace() || c.is_control()),
                "input {input:?} gave {url:?}"
            );
        }
    }

    #[test]
    fn refusals_never_repeat_the_input() {
        let secret = "hunter2-correct-horse";
        let cases = [
            (format!("admin:{secret}@example.com"), Refused::Credentials),
            (
                format!("https://admin:{secret}@example.com"),
                Refused::Credentials,
            ),
            (format!("javascript:{secret}"), Refused::Scheme),
            (format!("http://{secret}:99999"), Refused::Invalid),
            (format!("{secret}\u{7}"), Refused::ControlCharacter),
        ];
        for (input, expected) in cases {
            let result = resolve(&input);
            assert_eq!(result, Err(expected), "input {input:?}");
            if let Err(refused) = result {
                assert!(!refused.message().contains(secret));
            }
        }
        for refused in [
            Refused::Empty,
            Refused::ControlCharacter,
            Refused::Credentials,
            Refused::Scheme,
            Refused::Invalid,
        ] {
            assert!(!refused.message().is_empty());
            assert!(!refused.message().contains(secret));
        }
    }

    #[test]
    fn resolved_accessors() {
        let opened = Resolved::Address("https://example.com/".to_owned());
        assert_eq!(opened.url(), "https://example.com/");
        assert_eq!(opened.went(), "address");
        let looked_up = Resolved::Search(format!("{SEARCH_URL}x"));
        assert_eq!(looked_up.url(), format!("{SEARCH_URL}x"));
        assert_eq!(looked_up.went(), "search");
    }

    #[test]
    fn web_address_accepts_only_web_urls() {
        assert_eq!(
            web_address("https://example.com/a?b#c").as_deref(),
            Some("https://example.com/a?b#c")
        );
        assert_eq!(
            web_address("http://EXAMPLE.com").as_deref(),
            Some("http://example.com/")
        );
        assert_eq!(
            web_address("https://bücher.de").as_deref(),
            Some("https://xn--bcher-kva.de/")
        );
        assert_eq!(
            web_address("http://127.0.0.1:8080/x").as_deref(),
            Some("http://127.0.0.1:8080/x")
        );
        assert_eq!(
            web_address("http://[::1]/").as_deref(),
            Some("http://[::1]/")
        );
        for url in [
            "https://u:p@example.com",
            "https://u@example.com",
            "https://:p@example.com",
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "about:blank",
            "ftp://example.com",
            "ws://example.com",
            "blob:https://example.com/0",
            "ass://shell/",
            "chrome://settings",
            "/path",
            "path",
            "//example.com",
            "?q",
            "#x",
            "",
            "http://",
        ] {
            assert_eq!(web_address(url), None, "url {url:?}");
        }
    }
}
