//! Signing in to SereChat with OAuth 2.1: the authorization code flow with
//! PKCE for native apps (RFC 8252), and rotating refresh tokens.
//!
//! [`SignIn`] listens on a free loopback port, the app opens its consent
//! page in the system browser, and the browser comes back to
//! `http://127.0.0.1:{port}/callback` with a code that is traded for tokens.
//! A signed-in [`Client`] refreshes its one-hour access token in one place, a
//! lock all its clones share, so two refreshes never race for the refresh
//! token, which works once.

use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use ring::rand::SecureRandom as _;
use serde::Deserialize;

use crate::client::{BASE_URL, Client, read_json};
use crate::error::{Error, Result};

/// OpenRP's pre-registered public client: no secret, loopback redirects.
const CLIENT_ID: &str = "openrp";
/// What OpenRP asks to do: talk to language models (its model list is public).
const SCOPE: &str = "chat";
/// The audience tokens are issued for (RFC 8707): the REST API at [`BASE_URL`].
const RESOURCE: &str = "https://serechat.com/v1";
/// Refreshes this long before the access token expires.
const MARGIN: Duration = Duration::from_secs(60);
/// How long the browser may take to come back.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// How often the listener looks for the browser, and at `cancel`.
const POLL: Duration = Duration::from_millis(100);
/// Largest request head read from the browser.
const MAX_REQUEST: usize = 16 << 10;
/// What the browser shows once it is back.
const PAGE: &str = "<!doctype html><meta charset=utf-8><meta name=color-scheme content=\"light dark\"><title>OpenRP</title>\
    <body style=\"font:16px system-ui,sans-serif;text-align:center;margin-top:30vh\"><p>You can close this tab and return to OpenRP.</p>";

/// Keeps each new refresh token; `None` once signed out.
type Save = Box<dyn Fn(Option<&str>) + Send + Sync>;

/// Tokens from the token endpoint.
#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: String,
    expires_in: u64,
}

/// A SereChat sign-in, shared by every clone of its [`Client`].
pub(crate) struct OAuth {
    grant: Mutex<Grant>,
    save: Save,
}

/// The tokens in use.
struct Grant {
    /// Empty once signed out.
    refresh: String,
    /// Empty until the first refresh.
    access: String,
    /// When `access` stops working.
    expires: Instant,
}

impl From<Tokens> for Grant {
    fn from(tokens: Tokens) -> Self {
        Self { refresh: tokens.refresh_token, access: tokens.access_token, expires: Instant::now() + Duration::from_secs(tokens.expires_in) }
    }
}

impl OAuth {
    /// The access token, refreshed first when it is about to expire or is
    /// `rejected`: the one the server just turned down, unless another
    /// request refreshed it since.
    pub(crate) fn access(&self, client: &Client, rejected: Option<&str>) -> Result<String> {
        let mut grant = self.grant.lock().unwrap_or_else(PoisonError::into_inner);
        if grant.refresh.is_empty() {
            return Err(Error::Api { status: 401, code: Some("invalid_grant".into()), message: "Signed out of SereChat.".into(), retry_after: None });
        }
        if rejected == Some(grant.access.as_str()) || grant.expires < Instant::now() + MARGIN {
            let form = [("grant_type", "refresh_token"), ("refresh_token", grant.refresh.as_str()), ("client_id", CLIENT_ID)];
            let tokens: Tokens = read_json(client.post_form("/oauth/token", &form)?)?;
            // The old refresh token no longer works: the new one is kept first.
            (self.save)(Some(&tokens.refresh_token));
            *grant = tokens.into();
        }
        Ok(grant.access.clone())
    }

    /// Forgets the tokens and tells `save`, returning the refresh token.
    fn forget(&self) -> String {
        let mut grant = self.grant.lock().unwrap_or_else(PoisonError::into_inner);
        grant.access.clear();
        // Under the lock, so no refresh saves a token after this.
        (self.save)(None);
        std::mem::take(&mut grant.refresh)
    }
}

impl Client {
    /// A client signed in to SereChat with a saved refresh token, which its
    /// first request trades for an access token. `save` keeps every new
    /// refresh token (`None` once signed out).
    #[must_use]
    pub fn signed_in(refresh_token: String, save: impl Fn(Option<&str>) + Send + Sync + 'static) -> Self {
        let grant = Grant { refresh: refresh_token, access: String::new(), expires: Instant::now() };
        Self::new().with_grant(grant, Box::new(save))
    }

    fn with_grant(mut self, grant: Grant, save: Save) -> Self {
        self.oauth = Some(Arc::new(OAuth { grant: Mutex::new(grant), save }));
        self
    }

    /// Signs out of SereChat: forgets the refresh token in this client and
    /// all its clones, tells `save`, and revokes the grant.
    ///
    /// # Errors
    /// The revocation could not be sent; the token is forgotten anyway.
    pub fn sign_out(&self) -> Result<()> {
        match self.oauth.as_ref().map(|oauth| oauth.forget()) {
            Some(refresh) if !refresh.is_empty() => self.post_form("/oauth/revoke", &[("token", &refresh), ("client_id", CLIENT_ID)]).map(drop),
            _ => Ok(()),
        }
    }
}

/// A browser sign-in under way: a loopback listener waiting for SereChat's
/// consent page to send the browser back with a code.
pub struct SignIn {
    listener: TcpListener,
    redirect_uri: String,
    verifier: String,
    state: String,
    url: String,
}

impl SignIn {
    /// Listens on a free loopback port and makes the consent page's address.
    ///
    /// # Errors
    /// No port could be opened, or the system has no randomness to offer.
    pub fn start() -> Result<Self> {
        // An address, not `localhost`: the redirect must not depend on name resolution.
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let redirect_uri = format!("http://127.0.0.1:{}/callback", listener.local_addr()?.port());
        let (verifier, state) = (random(32)?, random(16)?);
        let challenge = base64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());
        let mut url = format!("{BASE_URL}/oauth/authorize?response_type=code");
        let query = [
            ("client_id", CLIENT_ID),
            ("redirect_uri", &redirect_uri),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("state", &state),
            ("scope", SCOPE),
            ("resource", RESOURCE),
        ];
        for (key, value) in query {
            let _ = write!(url, "&{key}={}", encode(value));
        }
        Ok(Self { listener, redirect_uri, verifier, state, url })
    }

    /// The consent page, for the system browser.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Waits for the browser to come back, checks its answer and trades the
    /// code for tokens, kept with `save` (see [`Client::signed_in`]). Gives
    /// up when `cancel` is raised or after a quarter of an hour.
    ///
    /// # Errors
    /// [`Error::SignIn`] when the user declined, it timed out or was
    /// cancelled, or the answer was wrong; or the token request failed.
    pub fn finish(self, cancel: &AtomicBool, save: impl Fn(Option<&str>) + Send + Sync + 'static) -> Result<Client> {
        let code = self.wait(cancel)?;
        let client = Client::new();
        let form = [
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &self.redirect_uri),
            ("client_id", CLIENT_ID),
            ("code_verifier", &self.verifier),
            ("resource", RESOURCE),
        ];
        let tokens: Tokens = read_json(client.post_form("/oauth/token", &form)?)?;
        save(Some(&tokens.refresh_token));
        Ok(client.with_grant(tokens.into(), Box::new(save)))
    }

    /// The code the browser brought back.
    fn wait(&self, cancel: &AtomicBool) -> Result<String> {
        self.listener.set_nonblocking(true)?;
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        let (answers, answered) = mpsc::channel();
        loop {
            match self.listener.accept() {
                // Each connection is read on its own thread: browsers open
                // spare ones that never send anything.
                Ok((stream, _)) => {
                    let answers = answers.clone();
                    let _ = std::thread::Builder::new().name("openrp-sign-in".into()).spawn(move || {
                        if let Some(query) = answer(stream) {
                            let _ = answers.send(query);
                        }
                    });
                }
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted | ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset) => {
                    std::thread::sleep(POLL);
                }
                Err(e) => return Err(e.into()),
            }
            while let Ok(query) = answered.try_recv() {
                if let Some(result) = self.check(&query) {
                    return result;
                }
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::SignIn("Sign-in was cancelled.".into()));
            }
            if Instant::now() > deadline {
                return Err(Error::SignIn("The browser took too long to come back. Please start again.".into()));
            }
        }
    }

    /// What the browser's `query` says: `None` when it is not the answer to
    /// this sign-in (another `state`), so the wait goes on.
    fn check(&self, query: &str) -> Option<Result<String>> {
        let param = |key: &str| query_param(query, key);
        if param("state").as_deref() != Some(self.state.as_str()) {
            return None;
        }
        // RFC 9207: an answer naming another issuer is a mix-up attack.
        if param("iss").as_deref() != Some(BASE_URL) {
            return Some(Err(Error::SignIn("The browser's answer did not come from SereChat.".into())));
        }
        Some(match (param("code"), param("error")) {
            (_, Some(error)) if error == "access_denied" => Err(Error::SignIn("Sign-in was declined in the browser.".into())),
            (_, Some(error)) => Err(Error::SignIn(param("error_description").unwrap_or(error))),
            (Some(code), None) if !code.is_empty() => Ok(code),
            _ => Err(Error::SignIn("SereChat sent the browser back without a code.".into())),
        })
    }
}

/// Reads a browser's request and answers it: the query of a `GET
/// /callback`, or `None` for anything else.
fn answer(mut stream: TcpStream) -> Option<String> {
    // Accepted sockets inherit non-blocking mode on some systems.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    // The whole head is read before answering: closing a socket with unread
    // data resets it, and the browser would show an error instead.
    let mut head = Vec::new();
    let mut chunk = [0; 2048];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).ok().filter(|&n| n > 0)?;
        head.extend_from_slice(&chunk[..read]);
        if head.len() > MAX_REQUEST {
            return None;
        }
    }
    let line = head.split(|&b| b == b'\r').next()?;
    let target = std::str::from_utf8(line).ok()?.strip_prefix("GET ")?.split(' ').next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let found = path == "/callback";
    let (status, body) = if found { ("200 OK", PAGE) } else { ("404 Not Found", "Not found") };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    found.then(|| query.to_owned())
}

/// `bytes` random bytes from the system, base64url-encoded.
fn random(bytes: usize) -> Result<String> {
    let mut buffer = vec![0; bytes];
    ring::rand::SystemRandom::new().fill(&mut buffer).map_err(|_| Error::SignIn("The system offered no randomness to sign in with.".into()))?;
    Ok(base64url(&buffer))
}

/// Base64url without padding (RFC 4648 section 5), as PKCE wants.
fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().zip([16, 8, 0]).fold(0u32, |n, (&byte, shift)| n | (u32::from(byte) << shift));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// Percent-encodes `value` for a URL query: all but the unreserved characters.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The decoded value of `key` in a URL query (`+` is a space); `None` when
/// absent or not UTF-8. A `%` not followed by two hex digits stays as it is.
fn query_param(query: &str, key: &str) -> Option<String> {
    let value = query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (k == key).then_some(v.as_bytes())
    })?;
    let hex = |i: usize| {
        let digits = value.get(i..i + 2).filter(|d| d.iter().all(u8::is_ascii_hexdigit))?;
        u8::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    };
    let (mut out, mut i) = (Vec::with_capacity(value.len()), 0);
    while let Some(&byte) = value.get(i) {
        match (byte, hex(i + 1)) {
            (b'%', Some(decoded)) => {
                out.push(decoded);
                i += 3;
            }
            (b'+', _) => {
                out.push(b' ');
                i += 1;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc_7636() {
        let verifier = [
            116, 24, 223, 180, 151, 153, 224, 37, 79, 250, 96, 125, 216, 173, 187, 186, 22, 212, 37, 77, 105, 214, 191, 240, 91, 88, 5, 88, 83, 132, 141,
            121,
        ];
        let verifier = base64url(&verifier);
        assert_eq!(verifier, "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        let challenge = base64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref());
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!([base64url(b"f"), base64url(b"fo"), base64url(b"foo"), base64url(b"\xfb\xff")], ["Zg", "Zm8", "Zm9v", "-_8"]);
        assert_eq!(random(32).unwrap().len(), 43, "PKCE wants 43 to 128 characters");
    }

    #[test]
    fn queries_encode_and_decode() {
        assert_eq!(encode("http://127.0.0.1:5/callback"), "http%3A%2F%2F127.0.0.1%3A5%2Fcallback");
        assert_eq!(encode("chat media é"), "chat%20media%20%C3%A9");
        let query = "code=a%2Bb+c&iss=https%3A%2F%2Fserechat.com&bad=%zz%4&cut=%E2%82&flag";
        assert_eq!(query_param(query, "code").as_deref(), Some("a+b c"));
        assert_eq!(query_param(query, "iss").as_deref(), Some(BASE_URL));
        assert_eq!(query_param(query, "bad").as_deref(), Some("%zz%4"), "broken escapes stay");
        assert_eq!(query_param(query, "cut"), None, "not UTF-8");
        assert_eq!(query_param(query, "flag").as_deref(), Some(""));
        assert_eq!(query_param(query, "missing"), None);
        assert_eq!(query_param("x=%+1", "x").as_deref(), Some("% 1"), "a sign is not a hex digit");
    }

    #[test]
    fn answers_are_checked() {
        let sign_in = SignIn::start().unwrap();
        assert!(sign_in.url().contains("&redirect_uri=http%3A%2F%2F127.0.0.1%3A") && sign_in.url().contains("&scope=chat&resource="));
        let iss = "iss=https%3A%2F%2Fserechat.com";
        let state = format!("state={}", sign_in.state);
        assert!(sign_in.check(&format!("code=c&{iss}&state=other")).is_none(), "another sign-in's answer is ignored");
        assert!(sign_in.check(&format!("code=c&{iss}")).is_none());
        assert_eq!(sign_in.check(&format!("code=c&{iss}&{state}")).unwrap().unwrap(), "c");
        let fails = |query: String| sign_in.check(&query).unwrap().unwrap_err().to_string();
        assert_eq!(fails(format!("code=c&iss=https%3A%2F%2Fevil.com&{state}")), "The browser's answer did not come from SereChat.");
        assert_eq!(fails(format!("code=c&{state}")), "The browser's answer did not come from SereChat.");
        assert_eq!(fails(format!("error=access_denied&{iss}&{state}")), "Sign-in was declined in the browser.");
        assert_eq!(fails(format!("error=invalid_scope&error_description=Unknown+scope&{iss}&{state}")), "Unknown scope");
        assert_eq!(fails(format!("code=&{iss}&{state}")), "SereChat sent the browser back without a code.");
    }

    #[test]
    fn browser_comes_back() {
        let sign_in = SignIn::start().unwrap();
        let port = sign_in.listener.local_addr().unwrap().port();
        let state = sign_in.state.clone();
        let browser = std::thread::spawn(move || {
            // A spare connection that sends nothing must not hold things up.
            let _idle = TcpStream::connect(("127.0.0.1", port)).unwrap();
            let get = |path: &str| {
                let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
                write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").unwrap();
                let mut reply = String::new();
                stream.read_to_string(&mut reply).unwrap();
                reply
            };
            assert!(get("/favicon.ico").starts_with("HTTP/1.1 404"));
            get(&format!("/callback?code=the-code&state={state}&iss=https%3A%2F%2Fserechat.com"))
        });
        assert_eq!(sign_in.wait(&AtomicBool::new(false)).unwrap(), "the-code");
        assert!(browser.join().unwrap().contains("return to OpenRP"));
        assert_eq!(sign_in.wait(&AtomicBool::new(true)).unwrap_err().to_string(), "Sign-in was cancelled.");
    }

    #[test]
    fn signed_out_clients_send_nothing() {
        let saved = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&saved);
        let client = Client::signed_in("r".into(), move |token| log.lock().unwrap().push(token.map(str::to_owned)));
        let clone = client.clone();
        assert_eq!(client.oauth.as_ref().unwrap().forget(), "r");
        assert_eq!(*saved.lock().unwrap(), [None]);
        let error = clone.bearer(None).unwrap_err();
        assert!(error.is_unauthorized(), "{error}");
        assert!(client.sign_out().is_ok(), "nothing left to revoke");
    }
}
