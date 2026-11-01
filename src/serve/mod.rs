//! `treetop serve`: treetop for a phone. A small HTTP server on 127.0.0.1
//! hands out the web app built from `web/`, embedded in the binary, and a
//! JSON API over the same state the screen shows: each workspace's pool is
//! an `App` kept current by the screen's own refresh clocks, so the phone
//! sees the rows, order and marks the list does.
//!
//! It is reached through `tether share` or `tailscale serve`, which give it
//! HTTPS on the tailnet and hand over who is asking in
//! `Tailscale-User-Login`. It answers only the login `[mobile] owner` names,
//! and anything without one gets nothing, so the tailnet's other devices and
//! people are turned away. A process on this machine can still reach the
//! port and claim any login: those run as the owner already.

mod chat;
mod pool;
mod terminal;
mod tree;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::app::App;
use crate::cache;
use crate::config;
use crate::diff;
use crate::pool as treehouse;
use crate::refresh::{Refresher, Update};
use crate::ship;
use crate::workspaces;

mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

/// The port it listens on when the config names none.
const PORT: u16 = 7777;

/// Requests handled at once. A live view holds one while it is open, so
/// there are enough for a phone with a few screens open and a second
/// device besides.
const THREADS: usize = 16;

/// The header `tailscale serve` names the asking person in.
const LOGIN: &str = "Tailscale-User-Login";

/// One workspace's pool, kept current by its own refresh clocks from the
/// first time it is asked for until the server stops.
pub struct Pool {
    pub name: String,
    pub checkout: PathBuf,
    pub app: Mutex<App>,
}

impl Pool {
    /// The pool of `checkout`, set up as the screen sets one up, with its
    /// clocks started and their updates applied as they come.
    fn spawn(checkout: PathBuf) -> Arc<Pool> {
        let mut app = App::new(None);
        app.checkout = checkout.clone();
        app.user = crate::branch::user();
        app.ship_plugins = ship::plugins(&checkout).0;
        app.base = diff::base(&checkout);
        if let Some(mut cached) = cache::read(&checkout) {
            cached.insert(0, treehouse::main_tree(&checkout));
            app.set_cached(cached);
        }
        let refresher = Refresher::spawn(checkout.clone(), app.trees.clone());
        let pool = Arc::new(Pool {
            name: workspaces::name(&checkout),
            checkout,
            app: Mutex::new(app),
        });
        thread::spawn({
            let pool = Arc::clone(&pool);
            move || {
                while let Ok(update) = refresher.updates.recv() {
                    pool.apply(update);
                }
            }
        });
        pool
    }

    fn apply(&self, update: Update) {
        let mut app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
        match update {
            Update::ListingStarted => app.start_listing(),
            Update::Listing(Ok(listing)) => app.set_trees(listing),
            Update::Listing(Err(err)) => {
                app.is_listing = false;
                app.error = Some(format!("{err:#}"));
            }
            Update::Processes(processes) => app.apply_processes(&processes),
            Update::Work(work) => app.apply_work(&work),
            Update::Reports(reports) => {
                app.apply_reports(reports);
            }
            Update::Sidecars(sidecars) => app.apply_sidecars(sidecars),
            Update::Ship(answer) => {
                app.apply_ship(answer);
            }
            Update::Usage(usage) => app.usage = usage,
            Update::Agents(agents) => app.apply_agents(&agents),
        }
    }
}

/// Everything a request is answered from.
pub struct State {
    /// The Tailscale login answered; every other is turned away.
    owner: String,
    /// The workspace shown when a request names none: the one treetop would
    /// open here.
    default: PathBuf,
    /// Each workspace's pool, by checkout, started the first time it is
    /// asked for.
    pools: Mutex<HashMap<PathBuf, Arc<Pool>>>,
    /// The agents' terminals open for the phone.
    terminals: terminal::Terminals,
}

impl State {
    pub fn new(owner: String, default: PathBuf) -> Self {
        State {
            owner,
            default,
            pools: Mutex::new(HashMap::new()),
            terminals: terminal::Terminals::default(),
        }
    }

    /// The pool of the workspace named `name`, else the default one.
    fn pool(&self, name: Option<&str>) -> Arc<Pool> {
        let checkout = name
            .and_then(|name| {
                workspaces::list()
                    .into_iter()
                    .find(|w| w.name() == name)
                    .map(|w| w.checkout)
            })
            .unwrap_or_else(|| self.default.clone());
        let mut pools = self.pools.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            pools
                .entry(checkout.clone())
                .or_insert_with(|| Pool::spawn(checkout)),
        )
    }
}

/// `treetop serve [--port N]`, from the checkout treetop would open in
/// `start`. Runs until it is stopped.
pub fn run(args: &[String], checkout: PathBuf) -> Result<()> {
    let config = config::load().map_err(|err| anyhow!(err))?;
    let port = match args {
        [] => config.mobile.port.unwrap_or(PORT),
        [flag, port] if flag == "--port" => port.parse().context("--port takes a number")?,
        _ => bail!("usage: treetop serve [--port N]"),
    };
    let Some(owner) = config.mobile.owner.filter(|o| !o.is_empty()) else {
        bail!(
            "treetop serve answers only its owner: set your Tailscale login as `owner` \
             under [mobile] in {}",
            config::path().map_or_else(|| "the config".into(), |p| p.display().to_string())
        );
    };
    let server = Server::http(("127.0.0.1", port))
        .map_err(|err| anyhow!("listening on 127.0.0.1:{port}: {err}"))?;
    workspaces::record(&checkout);
    let state = Arc::new(State::new(owner, checkout));
    // The default pool starts listing now, so the first page is not the
    // one that waits for it.
    state.pool(None);
    eprintln!(
        "treetop serve: http://127.0.0.1:{port}, for {} through `tether share {port}` \
         or `tailscale serve`",
        state.owner
    );
    thread::spawn({
        let state = Arc::clone(&state);
        move || loop {
            thread::sleep(std::time::Duration::from_secs(30));
            state.terminals.sweep();
        }
    });
    let server = Arc::new(server);
    let workers: Vec<_> = (0..THREADS)
        .map(|_| {
            let (server, state) = (Arc::clone(&server), Arc::clone(&state));
            thread::spawn(move || {
                while let Ok(request) = server.recv() {
                    handle(&state, request);
                }
            })
        })
        .collect();
    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

/// The largest request body read, far more than a line to an agent needs.
const BODY_LIMIT: u64 = 64 * 1024;

/// What a request is answered with, before it becomes a tiny_http response.
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// A body written as it comes, for a live view, in place of `body`.
    pub stream: Option<Box<dyn Read + Send>>,
    /// How long a browser may keep it: None never, Some for a file whose
    /// name changes with its contents.
    pub immutable: bool,
}

impl Reply {
    fn json(status: u16, value: &Value) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            body: serde_json::to_vec(value).unwrap_or_default(),
            stream: None,
            immutable: false,
        }
    }

    fn events(stream: impl Read + Send + 'static) -> Reply {
        Reply {
            status: 200,
            content_type: "text/event-stream",
            body: Vec::new(),
            stream: Some(Box::new(stream)),
            immutable: false,
        }
    }

    fn error(status: u16, message: &str) -> Reply {
        Reply::json(status, &json!({ "error": message }))
    }
}

/// A request as the routes read it.
pub struct Asked<'a> {
    pub method: &'a Method,
    /// The path, without the query.
    pub path: &'a str,
    pub query: HashMap<String, String>,
    /// Who `tailscale serve` says is asking.
    pub login: Option<&'a str>,
    /// The page that sent it, which for anything that acts must be this
    /// server's own, so another site open on the phone cannot act through it.
    pub origin: Option<&'a str>,
    /// The address the request was sent to.
    pub host: Option<&'a str>,
    pub body: &'a [u8],
}

fn handle(state: &State, mut request: Request) {
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let login = header(&request, LOGIN);
    let origin = header(&request, "Origin");
    let host = header(&request, "Host");
    let mut body = Vec::new();
    let _ = request.as_reader().take(BODY_LIMIT).read_to_end(&mut body);
    let method = request.method().clone();
    let reply = route(
        state,
        &Asked {
            method: &method,
            path,
            query: parse_query(query),
            login: login.as_deref(),
            origin: origin.as_deref(),
            host: host.as_deref(),
            body: &body,
        },
    );
    if let Some(stream) = reply.stream {
        write_stream(request, reply.content_type, stream);
        return;
    }
    let mut response = Response::from_data(reply.body)
        .with_status_code(reply.status)
        .with_header(header_of("Content-Type", reply.content_type))
        .with_header(header_of(
            "Cache-Control",
            if reply.immutable {
                "public, max-age=31536000, immutable"
            } else {
                "no-cache"
            },
        ))
        .with_header(header_of("X-Content-Type-Options", "nosniff"));
    if reply.content_type.starts_with("text/html") {
        response = response.with_header(header_of("X-Frame-Options", "DENY"));
    }
    let _ = request.respond(response);
}

/// Writes a live view straight to the connection, each piece as soon as it
/// comes: tiny_http's chunked body holds pieces back until it has 8 KiB,
/// which would keep a short message from the phone until more followed. The
/// body runs to the connection's close, as HTTP allows.
fn write_stream(request: Request, content_type: &str, mut stream: Box<dyn Read + Send>) {
    use std::io::Write;
    let mut socket = request.into_writer();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nCache-Control: no-cache\r\n\
         X-Accel-Buffering: no\r\nConnection: close\r\n\r\n"
    );
    if socket
        .write_all(head.as_bytes())
        .and_then(|()| socket.flush())
        .is_err()
    {
        return;
    }
    let mut buf = vec![0; 64 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                if socket
                    .write_all(&buf[..n])
                    .and_then(|()| socket.flush())
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

fn header(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

fn header_of(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("a valid header")
}

/// `a=1&b=two%20words` as a map, each value percent-decoded.
fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(high), Some(low)) => {
                    out.push(high << 4 | low);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(digit: u8) -> Option<u8> {
    (digit as char).to_digit(16).map(|d| d as u8)
}

/// Answers one request: the owner's API calls and the app's files, and
/// nothing at all for anyone else.
pub fn route(state: &State, asked: &Asked) -> Reply {
    if asked.login != Some(state.owner.as_str()) {
        return Reply::error(403, "this treetop answers only its owner");
    }
    match (asked.method, asked.path) {
        (Method::Get, "/api/workspaces") => Reply::json(200, &workspaces_json(state)),
        (Method::Get, "/api/pool") => {
            let pool = state.pool(asked.query.get("workspace").map(String::as_str));
            let app = pool.app.lock().unwrap_or_else(PoisonError::into_inner);
            Reply::json(200, &pool::pool_json(&app, &pool.name))
        }
        (method, path) if path.starts_with("/api/trees/") => {
            let pool = state.pool(asked.query.get("workspace").map(String::as_str));
            tree_route(state, &pool, method, &path["/api/trees/".len()..], asked)
        }
        (_, path) if path.starts_with("/api/") => Reply::error(404, "no such call"),
        (Method::Get | Method::Head, path) => asset(path),
        _ => Reply::error(405, "only GET here"),
    }
}

/// A call on one tree: `<name>`, `<name>/chat` or `<name>/send`.
fn tree_route(state: &State, pool: &Pool, method: &Method, rest: &str, asked: &Asked) -> Reply {
    let (name, call) = rest.split_once('/').unwrap_or((rest, ""));
    let name = decode(name);
    if *method != Method::Get && !is_same_site(asked) {
        return Reply::error(403, "only treetop's own page can act");
    }
    let app = pool.app.lock().unwrap_or_else(PoisonError::into_inner);
    let Some(found) = app.trees.iter().find(|t| t.name == name && !t.is_main) else {
        return Reply::error(404, &format!("no tree {name} in {}", pool.name));
    };
    match (method, call) {
        (Method::Get, "") => Reply::json(200, &tree::tree_json(&app, found, pool::now())),
        (Method::Get, "chat") => {
            let transcript = App::agents_of(found)
                .iter()
                .filter_map(|a| a.job.as_deref())
                .find_map(|job| chat::transcript_of(&crate::agents::claude::config_dir()?, job));
            match transcript {
                Some(path) => Reply::events(chat::stream(path)),
                None => Reply::error(
                    404,
                    "no conversation treetop can read for this tree's agent",
                ),
            }
        }
        (Method::Get, "terminal") => {
            let Some(agent) = App::agents_of(found)
                .into_iter()
                .find(|a| a.attach.is_some())
            else {
                return Reply::error(404, "this tree's agent has no terminal to open");
            };
            let size = |key: &str, default: u16, range: std::ops::RangeInclusive<u16>| {
                asked
                    .query
                    .get(key)
                    .and_then(|v| v.parse().ok())
                    .map_or(default, |v: u16| v.clamp(*range.start(), *range.end()))
            };
            let (cols, rows) = (
                size("cols", terminal::COLS, 30..=120),
                size("rows", terminal::ROWS, 12..=80),
            );
            let command = agent.attach.clone().unwrap_or_default();
            let (agent, tree) = (agent.name.clone(), found.name.clone());
            drop(app);
            match state
                .terminals
                .get((pool.checkout.clone(), command), rows, cols, &agent, &tree)
            {
                Ok(open) => Reply::events(open.stream()),
                Err(err) => Reply::error(502, &format!("{err:#}")),
            }
        }
        (Method::Post, "keys") => {
            #[derive(serde::Deserialize)]
            struct Keys {
                #[serde(default)]
                keys: Vec<String>,
                #[serde(default)]
                text: Option<String>,
            }
            let Ok(Keys { keys, text }) = serde_json::from_slice::<Keys>(asked.body) else {
                return Reply::error(400, "keys takes {\"keys\": [...], \"text\": \"...\"}");
            };
            let Some(command) = App::agents_of(found).iter().find_map(|a| a.attach.clone()) else {
                return Reply::error(404, "this tree's agent has no terminal");
            };
            let Some(open) = state.terminals.find(&(pool.checkout.clone(), command)) else {
                return Reply::error(409, "the agent's terminal is not open");
            };
            match open.write(&keys, text.as_deref()) {
                Ok(()) => Reply::json(200, &json!({})),
                Err(err) => Reply::error(400, &format!("{err:#}")),
            }
        }
        (Method::Post, "send") => {
            drop(app);
            send(pool, &name, asked.body)
        }
        _ => Reply::error(404, "no such call"),
    }
}

/// The request comes from this server's own page: its Origin names the
/// host it was sent to.
fn is_same_site(asked: &Asked) -> bool {
    match (asked.origin, asked.host) {
        (Some(origin), Some(host)) => origin
            .split_once("://")
            .is_some_and(|(_, rest)| rest.eq_ignore_ascii_case(host)),
        _ => false,
    }
}

/// Sends a line to the tree's agent the way `treetop send` does, by running
/// it: the chat is typed into in a terminal of its own, or a cold agent is
/// started again with the line.
fn send(pool: &Pool, tree: &str, body: &[u8]) -> Reply {
    #[derive(serde::Deserialize)]
    struct Line {
        text: String,
    }
    let Ok(Line { text }) = serde_json::from_slice::<Line>(body) else {
        return Reply::error(400, "send takes {\"text\": \"...\"}");
    };
    let text = text.trim();
    if text.is_empty() {
        return Reply::error(400, "nothing to send");
    }
    let output = std::env::current_exe().and_then(|treetop| {
        Command::new(treetop)
            .args(["send", tree, text])
            .current_dir(&pool.checkout)
            .output()
    });
    match output {
        Ok(out) if out.status.success() => Reply::json(
            200,
            &json!({ "said": String::from_utf8_lossy(&out.stdout).trim() }),
        ),
        Ok(out) => {
            let why = String::from_utf8_lossy(&out.stderr);
            let why = why
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("not sent");
            Reply::error(502, why.trim_start_matches("Error: "))
        }
        Err(err) => Reply::error(500, &format!("could not run treetop send: {err}")),
    }
}

fn workspaces_json(state: &State) -> Value {
    let default = workspaces::name(&state.default);
    let mut names: Vec<String> = workspaces::list().iter().map(|w| w.name()).collect();
    if !names.contains(&default) {
        names.insert(0, default.clone());
    }
    json!({ "default": default, "workspaces": names })
}

/// A file of the app, or the app's page for any path that names none, so
/// the page loads whatever screen the address was left on.
fn asset(path: &str) -> Reply {
    let name = path.trim_start_matches('/');
    let found = assets::ASSETS.iter().find(|(file, _)| *file == name);
    match found {
        Some((file, bytes)) => Reply {
            status: 200,
            content_type: content_type(file),
            body: bytes.to_vec(),
            stream: None,
            // Vite names what it builds under assets/ for its contents.
            immutable: file.starts_with("assets/"),
        },
        None if name.contains('.') => Reply::error(404, "no such file"),
        None => {
            let page = assets::ASSETS
                .iter()
                .find(|(file, _)| *file == "index.html")
                .map_or(&[][..], |(_, bytes)| bytes);
            Reply {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: page.to_vec(),
                stream: None,
                immutable: false,
            }
        }
    }
}

fn content_type(file: &str) -> &'static str {
    match Path::new(file).extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked<'a>(path: &'a str, login: Option<&'a str>) -> Asked<'a> {
        Asked {
            method: &Method::Get,
            path,
            query: HashMap::new(),
            login,
            origin: None,
            host: None,
            body: &[],
        }
    }

    #[test]
    fn only_the_owner_is_answered() {
        let state = State::new("me@example.com".into(), PathBuf::from("/nowhere"));
        for (login, path) in [
            (None, "/api/workspaces"),
            (Some("someone@else.com"), "/api/workspaces"),
            (Some(""), "/"),
            (Some("ME@example.com"), "/index.html"),
        ] {
            assert_eq!(
                route(&state, &asked(path, login)).status,
                403,
                "{login:?} {path}"
            );
        }
        assert_eq!(
            route(&state, &asked("/", Some("me@example.com"))).status,
            200
        );
    }

    #[test]
    fn any_screen_s_address_loads_the_app_and_a_missing_file_does_not() {
        let state = State::new("me".into(), PathBuf::from("/nowhere"));
        let me = Some("me");
        let page = route(&state, &asked("/trees/17", me));
        assert_eq!(
            (page.status, page.content_type),
            (200, "text/html; charset=utf-8")
        );
        let index = route(&state, &asked("/index.html", me));
        assert!(!index.body.is_empty(), "the app's page is embedded");
        assert_eq!(page.body, index.body);
        assert_eq!(route(&state, &asked("/", me)).body, index.body);
        assert_eq!(route(&state, &asked("/assets/gone.js", me)).status, 404);
        assert_eq!(route(&state, &asked("/api/nothing", me)).status, 404);
    }

    #[test]
    fn a_live_view_s_short_event_reaches_the_phone_without_waiting_for_more() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;
        use std::time::Duration;
        let server = Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        // A stream that sends one short event, then stays open with nothing
        // more to say, as a quiet chat does.
        struct Quiet(std::sync::mpsc::Receiver<Vec<u8>>);
        impl Read for Quiet {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                match self.0.recv() {
                    Ok(bytes) => {
                        buf[..bytes.len()].copy_from_slice(&bytes);
                        Ok(bytes.len())
                    }
                    Err(_) => Ok(0),
                }
            }
        }
        thread::spawn(move || {
            let request = server.recv().unwrap();
            write_stream(request, "text/event-stream", Box::new(Quiet(rx)));
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        client
            .write_all(b"GET /api/trees/1/chat HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        tx.send(b"event: message\ndata: {}\n\n".to_vec()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut lines = BufReader::new(client).lines();
        let mut seen = Vec::new();
        while let Some(Ok(line)) = lines.next() {
            let is_event = line == "event: message";
            seen.push(line);
            if is_event {
                break;
            }
        }
        assert_eq!(seen.first().map(String::as_str), Some("HTTP/1.1 200 OK"));
        assert_eq!(
            seen.last().map(String::as_str),
            Some("event: message"),
            "{seen:?}"
        );
        drop(tx);
    }

    #[test]
    fn only_this_server_s_own_page_can_act() {
        let asked = |origin: Option<&'static str>| Asked {
            origin,
            host: Some("server.tailnet.ts.net:7777"),
            ..asked("/api/trees/17/send", Some("me"))
        };
        assert!(is_same_site(&asked(Some(
            "https://server.tailnet.ts.net:7777"
        ))));
        for origin in [
            None,
            Some("https://evil.example"),
            Some("https://server.tailnet.ts.net:7778"),
            Some("https://server.tailnet.ts.net:7777.evil.example"),
            Some("null"),
        ] {
            assert!(!is_same_site(&asked(origin)), "{origin:?}");
        }
    }

    #[test]
    fn a_query_is_percent_decoded() {
        let query = parse_query("workspace=my%20app&x=a+b&bad=%zz&end=%4");
        assert_eq!(query["workspace"], "my app");
        assert_eq!(query["x"], "a b");
        assert_eq!(query["bad"], "%zz");
        assert_eq!(query["end"], "%4");
    }
}
