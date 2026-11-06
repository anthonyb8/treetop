//! `treetop serve`: treetop for a phone. A small HTTP server on 127.0.0.1
//! hands out the web app built from `web/`, embedded in the binary, and a
//! JSON API over the same state the screen shows: each workspace's pool is
//! an `App` kept current by the screen's own refresh clocks, so the phone
//! sees the rows, order and marks the list does.
//!
//! It shares itself on start, with the `share` command `p` uses, `tether
//! share` by default, which gives it HTTPS on the tailnet and hands over who
//! is asking in `Tailscale-User-Login`. It answers only the login `[mobile] owner` names,
//! and anything without one gets nothing, so the tailnet's other devices and
//! people are turned away. A process on this machine can still reach the
//! port and claim any login: those run as the owner already.

mod actions;
mod answer;
mod chat;
mod inbox;
mod install;
mod issues;
mod pool;
mod push;
mod tree;

pub use pool::title;

use std::collections::HashMap;
use std::io::{IsTerminal as _, Read};
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
use crate::phone;
use crate::pool as treehouse;
use crate::refresh::{Refresher, Update};
use crate::ship;
use crate::sidecar::Shelf;
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
    /// Lists the pool again now, after an action changed it.
    kick: Mutex<std::sync::mpsc::Sender<()>>,
    /// Where pushes go, when the server can send them.
    push: Option<Arc<push::Push>>,
    /// What pushes are judged against: the pool as last seen, once it has
    /// been read in full, and what of it has been read so far.
    seen: Mutex<Watch>,
    /// The merged tickets last looked at, and what they left ready.
    ready: Mutex<(Merged, Vec<String>)>,
    /// The other workspaces' agents and PRs, read slowly for the inbox from
    /// the first time the phone asks for it.
    away: Mutex<Option<Away>>,
    pub app: Mutex<App>,
}

/// The other workspaces' reading, and whether a round of it has come in.
struct Away {
    reader: crate::inbox::Reader<HashMap<PathBuf, Vec<treehouse::Tree>>>,
    is_read: bool,
}

impl Pool {
    /// The pool of `checkout`, set up as the screen sets one up, with its
    /// clocks started and their updates applied as they come.
    fn spawn(checkout: PathBuf, push: Option<Arc<push::Push>>) -> Arc<Pool> {
        let mut app = App::new(None);
        app.checkout = checkout.clone();
        app.user = crate::branch::user();
        app.ship_plugins = ship::plugins(&checkout).0;
        app.base = diff::base(&checkout);
        app.pools = crate::inbox::other_pools(&checkout);
        if let Some(mut cached) = cache::read(&checkout) {
            cached.insert(0, treehouse::main_tree(&checkout));
            app.set_cached(cached);
        }
        let refresher = Refresher::spawn(checkout.clone(), app.trees.clone());
        let pool = Arc::new(Pool {
            name: workspaces::name(&checkout),
            checkout,
            kick: Mutex::new(refresher.kicker()),
            push,
            seen: Mutex::new(Watch::default()),
            ready: Mutex::default(),
            away: Mutex::new(None),
            app: Mutex::new(app),
        });
        thread::spawn({
            let pool = Arc::clone(&pool);
            move || {
                let mut clock = crate::snapshot::Clock::default();
                while let Ok(update) = refresher.updates.recv() {
                    pool.apply(update);
                    pool.snapshot(&mut clock);
                }
            }
        });
        pool
    }

    /// Writes what `treetop status` prints for this workspace, once the pool
    /// has been read in full and at most as often as `clock` allows.
    fn snapshot(&self, clock: &mut crate::snapshot::Clock) {
        let app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
        let has_reports = self
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .has_reports;
        if !app.is_agents_read || !has_reports || !clock.is_due() {
            return;
        }
        let ready = self
            .ready
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone();
        let status = crate::cli::status_json(
            &app.trees,
            &app.reports,
            &ready,
            std::time::SystemTime::now(),
        );
        drop(app);
        crate::snapshot::write(&self.checkout, &status);
    }

    /// Lists the pool again now rather than at the next trigger.
    pub fn refresh(&self) {
        let _ = self
            .kick
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(());
    }

    /// Notes how far `port` is shared, None for not at all, as a tap has just
    /// changed it, so the next page shows it before the shares are read again.
    pub fn set_share(&self, port: u16, share: Option<crate::phone::Shared>) {
        let mut app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
        match share {
            Some(share) => app.shares.insert(port, share),
            None => app.shares.remove(&port),
        };
    }

    /// A tree of this pool, or its main checkout, listens on `port`.
    pub fn serves(&self, port: u16) -> bool {
        let app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
        app.trees.iter().any(|t| t.ports().contains(&port))
    }

    /// Gives `app` the other workspaces' agents and PRs as last read,
    /// starting their reading the first time, and says whether any round
    /// of it has come in yet.
    fn read_away(&self, app: &mut App) -> bool {
        let mut away = self.away.lock().unwrap_or_else(PoisonError::into_inner);
        let away = away.get_or_insert_with(|| Away {
            reader: crate::inbox::spawn_reader(self.checkout.clone()),
            is_read: false,
        });
        while let Ok(live) = away.reader.rounds.try_recv() {
            app.away_live = live;
            away.is_read = true;
        }
        away.is_read
    }

    /// The pool's agents and its reports have been read, so what `app`
    /// says of them is how they stand rather than not known yet.
    fn is_read(&self, app: &App) -> bool {
        app.is_agents_read
            && self
                .seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .has_reports
    }

    /// The tree called `name` as the pool last read it.
    pub fn tree(&self, name: &str) -> Option<crate::pool::Tree> {
        let app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
        app.trees.iter().find(|t| t.name == name).cloned()
    }

    /// The tickets merges have left ready. The screen learns them when it
    /// follows a merge up, which a server does not do. Reading them takes
    /// several calls to the ticket system, so they are read again only when
    /// what has merged changes, not every time the list refreshes.
    fn ready(&self) -> Vec<String> {
        let trees = {
            let app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
            app.trees.clone()
        };
        let merged: Merged = crate::cli::merged_tickets(&trees)
            .into_iter()
            .map(|(id, source)| (id.to_string(), source.map(str::to_string)))
            .collect();
        let mut ready = self.ready.lock().unwrap_or_else(PoisonError::into_inner);
        if ready.0 != merged {
            *ready = (
                merged,
                crate::cli::ready_after_merges(&self.checkout, &trees),
            );
        }
        ready.1.clone()
    }

    fn apply(&self, update: Update) {
        let mut app = self.app.lock().unwrap_or_else(PoisonError::into_inner);
        let mut watch = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        match &update {
            Update::Reports(_) => watch.has_reports = true,
            Update::Ship(_) => watch.has_ship = true,
            _ => {}
        }
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
                // Another workspace leases trees as its agents start, so its
                // cache is read again to place their reports in the inbox.
                app.pools = crate::inbox::other_pools(&self.checkout);
                app.apply_reports(reports);
            }
            Update::Sidecars(sidecars) => app.apply_sidecars(sidecars),
            Update::Ship(answer) => {
                app.apply_ship(answer);
            }
            Update::Usage(usage) => app.usage = usage,
            Update::Runs(runs) => app.runs = runs.unwrap_or_default(),
            Update::Base(standing) => app.apply_base(standing),
            Update::Release(tag) => app.apply_release(tag),
            Update::CatchUpHookFailed(why) => app.message = Some(why),
            Update::Agents(agents) => app.apply_agents(&agents),
            Update::Shares(shares) => app.apply_shares(shares),
        }
        // Pushes are judged once the pool has been read in full: agents,
        // reports and PRs. Until then a change is only the reading catching
        // up, and pushing it would buzz the phone with the whole pool.
        let Some(push) = &self.push else {
            return;
        };
        let is_read = app.is_agents_read
            && watch.has_reports
            && (watch.has_ship || watch.started.elapsed() >= SHIP_WAIT);
        if !is_read {
            return;
        }
        let now = push::Seen::of(&app);
        let notices = watch
            .last
            .as_ref()
            .map(|last| last.notices(&now, &app, &self.name))
            .unwrap_or_default();
        watch.last = Some(now);
        if !notices.is_empty() {
            let push = Arc::clone(push);
            thread::spawn(move || {
                for notice in &notices {
                    push.send(notice);
                }
            });
        }
    }
}

/// The tickets of the merged trees, each with its issue source.
type Merged = Vec<(String, Option<String>)>;

/// How long pushes wait on the PRs' first reading, for a repository whose
/// ship plugins cannot say.
const SHIP_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// How much of the pool has been read, and how it last looked.
struct Watch {
    started: std::time::Instant,
    has_reports: bool,
    has_ship: bool,
    last: Option<push::Seen>,
}

impl Default for Watch {
    fn default() -> Watch {
        Watch {
            started: std::time::Instant::now(),
            has_reports: false,
            has_ship: false,
            last: None,
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
    /// Where pushes go; None when the server could not set them up.
    push: Option<Arc<push::Push>>,
    /// The command a port is shared with, as `p` runs it.
    share: Vec<String>,
    /// The command a port is put on the internet with, as `P` runs it.
    publish: Vec<String>,
    /// The command a port is taken off again with, as `x` runs it.
    unpublish: Vec<String>,
}

impl State {
    pub fn new(owner: String, default: PathBuf) -> Self {
        State {
            owner,
            default,
            pools: Mutex::new(HashMap::new()),
            push: None,
            share: Vec::new(),
            publish: Vec::new(),
            unpublish: Vec::new(),
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
                .or_insert_with(|| Pool::spawn(checkout, self.push.clone())),
        )
    }
}

/// What `treetop serve` was asked to do.
#[derive(Debug, PartialEq, Eq)]
enum Mode {
    /// Serve here, on this `--port` or the config's.
    Serve(Option<u16>),
    /// Serve as a service from now on, likewise.
    Install(Option<u16>),
}

const USAGE: &str = "usage: treetop serve [--port N] [--install] | treetop serve --uninstall";

fn parse(args: &[String]) -> Result<Mode> {
    let mut port = None;
    let mut is_install = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let number = args.next().ok_or_else(|| anyhow!(USAGE))?;
                port = Some(number.parse().context("--port takes a number")?);
            }
            "--install" => is_install = true,
            _ => bail!(USAGE),
        }
    }
    Ok(if is_install {
        Mode::Install(port)
    } else {
        Mode::Serve(port)
    })
}

/// `treetop serve --uninstall`: needs no workspace, since the service may
/// outlast every one.
pub fn uninstall() -> Result<()> {
    install::uninstall()
}

/// Restarts the service on the binary now on disk, after an update. False
/// when it is not installed.
pub fn restart_service() -> Result<bool> {
    install::restart_if_installed()
}

/// The login the config says to answer, without which there is nothing to
/// serve.
fn owner(config: &config::Config) -> Result<String> {
    match config.mobile.owner.as_deref() {
        Some(owner) if !owner.is_empty() => Ok(owner.to_string()),
        _ => bail!(
            "treetop serve answers only its owner: set your Tailscale login as `owner` \
             under [mobile] in {}",
            config::path().map_or_else(|| "the config".into(), |p| p.display().to_string())
        ),
    }
}

/// Shares `port` and says where the phone reaches it, with a QR code to scan
/// when someone is at a terminal to scan it; a service's goes to its journal.
fn share(command: &[String], port: u16) {
    match phone::share(command, port) {
        Ok(address) => {
            let code = std::io::stderr()
                .is_terminal()
                .then(|| phone::Code::new(&address));
            eprintln!("treetop serve: on your tailnet at {address}");
            match code {
                Some(Ok(code)) => eprintln!("{}", code.text()),
                Some(Err(why)) => eprintln!("treetop serve: {why}"),
                None => {}
            }
        }
        Err(why) => {
            eprintln!("treetop serve: not shared, so only on 127.0.0.1:{port}: {why}");
        }
    }
}

/// `treetop serve --install`, from the checkout treetop would open in
/// `start`: the service, then the phone's way in.
fn install_service(
    config: &config::Config,
    asked: Option<u16>,
    port: u16,
    checkout: PathBuf,
) -> Result<()> {
    // A service with no owner would only fail, over and over, out of sight.
    owner(config)?;
    let unit = install::Unit {
        binary: std::env::current_exe().context("finding this binary")?,
        workspace: checkout,
        port: asked,
        path: std::env::var("PATH").unwrap_or_default(),
    };
    install::install(&unit, port)?;
    println!("{}", install::lingering());
    share(&config.share, port);
    Ok(())
}

/// `treetop serve [--port N] [--install]`, from the checkout treetop would
/// open in `start`. Serving runs until it is stopped.
pub fn run(args: &[String], checkout: PathBuf) -> Result<()> {
    let config = config::load().map_err(|err| anyhow!(err))?;
    let mode = parse(args)?;
    let (Mode::Serve(chosen) | Mode::Install(chosen)) = mode;
    let port = chosen.or(config.mobile.port).unwrap_or(PORT);
    if let Mode::Install(_) = mode {
        return install_service(&config, chosen, port, checkout);
    }
    let owner = owner(&config)?;
    let server = Server::http(("127.0.0.1", port))
        .map_err(|err| anyhow!("listening on 127.0.0.1:{port}: {err}"))?;
    workspaces::record(&checkout);
    let mut state = State::new(owner.clone(), checkout);
    let push_dir = config::path().and_then(|p| p.parent().map(|dir| dir.join("push")));
    match push_dir.map(|dir| push::Push::open(dir, &owner)) {
        Some(Ok(push)) => state.push = Some(Arc::new(push)),
        Some(Err(why)) => eprintln!("treetop serve: no pushes: {why}"),
        None => eprintln!("treetop serve: no pushes: no config directory"),
    }
    state.share.clone_from(&config.share);
    state.publish.clone_from(&config.publish);
    state.unpublish.clone_from(&config.unpublish);
    let state = Arc::new(state);
    // The default pool starts listing now, so the first page is not the
    // one that waits for it.
    state.pool(None);
    eprintln!(
        "treetop serve: http://127.0.0.1:{port}, for {}",
        state.owner
    );
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
    // Once it listens, since `tether share` can take its 30 seconds and the
    // server answers 127.0.0.1 meanwhile, whatever it comes to.
    thread::spawn(move || share(&config.share, port));
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
        (Method::Get, "/api/push/key") => match &state.push {
            Some(push) => Reply::json(200, &json!({ "key": push.public_key() })),
            None => Reply::error(404, "this server cannot send pushes"),
        },
        (Method::Post, "/api/push/subscribe" | "/api/push/unsubscribe") => {
            if !is_same_site(asked) {
                return Reply::error(403, "only treetop's own page can act");
            }
            let Some(push) = &state.push else {
                return Reply::error(404, "this server cannot send pushes");
            };
            let done = if asked.path.ends_with("unsubscribe") {
                #[derive(serde::Deserialize)]
                struct Gone {
                    endpoint: String,
                }
                serde_json::from_slice::<Gone>(asked.body)
                    .map_err(|_| "unsubscribe takes {\"endpoint\": \"...\"}".to_string())
                    .and_then(|gone| push.unsubscribe(&gone.endpoint))
            } else {
                serde_json::from_slice::<push::Subscription>(asked.body)
                    .map_err(|_| "subscribe takes the browser's subscription".to_string())
                    .and_then(|subscription| push.subscribe(subscription))
            };
            match done {
                Ok(()) => Reply::json(200, &json!({})),
                Err(why) => Reply::error(400, &why),
            }
        }
        (Method::Post, "/api/dispatch") => {
            if !is_same_site(asked) {
                return Reply::error(403, "only treetop's own page can act");
            }
            let pool = state.pool(asked.query.get("workspace").map(String::as_str));
            actions::dispatch(&pool, asked.body)
        }
        (Method::Post, path) if path.starts_with("/api/ports/") => {
            if !is_same_site(asked) {
                return Reply::error(403, "only treetop's own page can act");
            }
            let call = path["/api/ports/".len()..]
                .split_once('/')
                .and_then(|(port, call)| Some((port.parse::<u16>().ok()?, call)));
            let pool = || state.pool(asked.query.get("workspace").map(String::as_str));
            match call {
                Some((port, "share")) => actions::share_port(&pool(), &state.share, port),
                Some((port, "publish")) => actions::publish_port(&pool(), &state.publish, port),
                Some((port, "unshare")) => actions::unshare_port(&pool(), &state.unpublish, port),
                _ => Reply::error(404, "no such call"),
            }
        }
        (method, path) if path == "/api/issues" || path.starts_with("/api/issues/") => {
            if *method != Method::Get && !is_same_site(asked) {
                return Reply::error(403, "only treetop's own page can act");
            }
            let pool = state.pool(asked.query.get("workspace").map(String::as_str));
            let rest = path["/api/issues".len()..].trim_start_matches('/');
            issues::route(&pool, &decode(rest), asked)
        }
        (method, path) if path == "/api/inbox" || path.starts_with("/api/inbox/") => {
            if *method != Method::Get && !is_same_site(asked) {
                return Reply::error(403, "only treetop's own page can act");
            }
            let pool = state.pool(asked.query.get("workspace").map(String::as_str));
            inbox::route(
                &pool,
                method,
                path["/api/inbox".len()..].trim_start_matches('/'),
                asked,
            )
        }
        (method, path) if path.starts_with("/api/trees/") => {
            let pool = state.pool(asked.query.get("workspace").map(String::as_str));
            tree_route(&pool, method, &path["/api/trees/".len()..], asked)
        }
        (_, path) if path.starts_with("/api/") => Reply::error(404, "no such call"),
        (Method::Get | Method::Head, path) => asset(path),
        _ => Reply::error(405, "only GET here"),
    }
}

/// A call on one tree: `<name>`, `<name>/chat` or `<name>/send`.
fn tree_route(pool: &Pool, method: &Method, rest: &str, asked: &Asked) -> Reply {
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
                .find_map(|job| chat::transcript_of(&crate::agents::claude::job_home(job)?, job));
            match transcript {
                Some(path) => Reply::events(chat::stream(path)),
                None => Reply::error(
                    404,
                    "no conversation treetop can read for this tree's agent",
                ),
            }
        }
        (Method::Get, "prompt") => {
            let Some(command) = App::agents_of(found).iter().find_map(|a| a.attach.clone()) else {
                return Reply::error(404, "this tree's agent has no chat to read a question from");
            };
            drop(app);
            let Some(hints) = crate::plugin::screen_of("claude") else {
                return Reply::error(500, "the Claude Code plugin says nothing of its screen");
            };
            match answer::read(&command, &hints) {
                Ok(ask) => Reply::json(200, &json!({ "ask": ask })),
                Err(why) => Reply::error(409, &why),
            }
        }
        (Method::Post, "answer") => {
            #[derive(serde::Deserialize)]
            struct Answer {
                action: String,
                #[serde(default)]
                text: Option<String>,
            }
            let Ok(answer) = serde_json::from_slice::<Answer>(asked.body) else {
                return Reply::error(
                    400,
                    "answer takes {\"action\": \"allow\" | \"deny\" | \"say\", \"text\": \"...\"}",
                );
            };
            let Some(command) = App::agents_of(found).iter().find_map(|a| a.attach.clone()) else {
                return Reply::error(404, "this tree's agent has no chat to answer in");
            };
            drop(app);
            let Some(hints) = crate::plugin::screen_of("claude") else {
                return Reply::error(500, "the Claude Code plugin says nothing of its screen");
            };
            let Some((press, says)) = answer_of(&answer.action) else {
                return Reply::error(400, &format!("no answer called {}", answer.action));
            };
            match answer::press(&command, &hints, press) {
                Err(why) => Reply::error(409, &why),
                // The prompt is put away; the answer goes in as a message.
                Ok(()) if says => {
                    let body = json!({ "text": answer.text.unwrap_or_default() }).to_string();
                    send(pool, &name, body.as_bytes())
                }
                Ok(()) => Reply::json(200, &json!({})),
            }
        }
        (Method::Post, "park") => {
            drop(app);
            actions::shelve(pool, &name, Shelf::Parked, asked.body)
        }
        (Method::Post, "backburner") => {
            drop(app);
            actions::shelve(pool, &name, Shelf::Backburner, asked.body)
        }
        (Method::Post, "resume") => {
            drop(app);
            actions::resume(pool, &name)
        }
        (Method::Post, "return") => {
            drop(app);
            actions::give_back(pool, &name)
        }
        (Method::Post, "send") => {
            drop(app);
            send(pool, &name, asked.body)
        }
        _ => Reply::error(404, "no such call"),
    }
}

/// What an answer presses on the prompt, and whether words follow it as a
/// message: only Allow lets the prompt's own choice stand; words always put
/// the prompt away first, so they never approve what they argue against.
fn answer_of(action: &str) -> Option<(answer::Press, bool)> {
    match action {
        "allow" => Some((answer::Press::Allow, false)),
        "deny" => Some((answer::Press::Deny, false)),
        "say" => Some((answer::Press::Clear, true)),
        _ => None,
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

/// What `treetop send` wrote about why a line did not go: the reason it gave
/// for the agent, which the phone can act on, over its closing tally.
fn why_not_sent(stderr: &str) -> &str {
    let lines = || stderr.lines().rev().filter(|l| !l.trim().is_empty());
    lines()
        .find(|l| l.starts_with("not sent to "))
        .or_else(|| lines().next())
        .map_or("not sent", |l| l.trim_start_matches("Error: "))
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
            Reply::error(502, why_not_sent(&why))
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

    #[test]
    fn a_line_not_sent_says_why_rather_than_the_tally() {
        let stderr = "not sent to pickup in tree 3: a dialog is open in its chat; answer it there\n\
                      Error: sent to 0 of 1 agent(s)\n";
        assert_eq!(
            why_not_sent(stderr),
            "not sent to pickup in tree 3: a dialog is open in its chat; answer it there"
        );
        assert_eq!(why_not_sent("Error: no agent at 3\n"), "no agent at 3");
        assert_eq!(why_not_sent(""), "not sent");
    }

    #[test]
    fn serve_takes_a_port_and_install_in_either_order() {
        let words = |list: &[&str]| list.iter().map(ToString::to_string).collect::<Vec<_>>();
        let asked = |list: &[&str]| parse(&words(list)).ok();
        assert_eq!(asked(&[]), Some(Mode::Serve(None)));
        assert_eq!(asked(&["--port", "8123"]), Some(Mode::Serve(Some(8123))));
        assert_eq!(asked(&["--install"]), Some(Mode::Install(None)));
        assert_eq!(
            asked(&["--install", "--port", "8123"]),
            Some(Mode::Install(Some(8123)))
        );
        assert_eq!(
            asked(&["--port", "8123", "--install"]),
            Some(Mode::Install(Some(8123)))
        );
        assert_eq!(asked(&["--port"]), None);
        assert_eq!(asked(&["--port", "many"]), None);
        assert_eq!(asked(&["--instal"]), None);
    }

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
    fn another_site_cannot_start_an_agent() {
        let state = State::new("me".into(), PathBuf::from("/nowhere"));
        for origin in [None, Some("https://evil.example")] {
            let reply = route(
                &state,
                &Asked {
                    method: &Method::Post,
                    origin,
                    host: Some("server.tailnet.ts.net:7777"),
                    // A task the server turns down before any job runs, so
                    // that if the guard were gone this would still start
                    // nothing.
                    body: br#"{"task": ""}"#,
                    ..asked("/api/dispatch", Some("me"))
                },
            );
            assert_eq!(reply.status, 403, "{origin:?}");
        }
    }

    #[test]
    fn another_site_cannot_share_a_port_or_park_a_tree() {
        let state = State::new("me".into(), PathBuf::from("/nowhere"));
        for path in [
            "/api/ports/5173/share",
            "/api/ports/5173/publish",
            "/api/ports/5173/unshare",
            "/api/trees/17/park",
        ] {
            for origin in [None, Some("https://evil.example")] {
                let reply = route(
                    &state,
                    &Asked {
                        method: &Method::Post,
                        origin,
                        host: Some("server.tailnet.ts.net:7777"),
                        body: br#"{"parked": true}"#,
                        ..asked(path, Some("me"))
                    },
                );
                assert_eq!(reply.status, 403, "{path} {origin:?}");
            }
        }
    }

    #[test]
    fn only_a_port_a_tree_serves_goes_onto_the_tailnet() {
        let mark = std::env::temp_dir().join(format!("treetop-shared-{}", std::process::id()));
        let _ = std::fs::remove_file(&mark);
        let mut state = State::new("me".into(), PathBuf::from("/nowhere"));
        state.share = vec!["touch".into(), mark.display().to_string()];
        let reply = route(
            &state,
            &Asked {
                method: &Method::Post,
                origin: Some("https://server.tailnet.ts.net:7777"),
                host: Some("server.tailnet.ts.net:7777"),
                ..asked("/api/ports/22/share", Some("me"))
            },
        );
        assert_eq!(reply.status, 404);
        assert!(!mark.exists(), "the share command never ran");
    }

    #[test]
    fn another_site_cannot_change_or_pick_up_an_issue() {
        let state = State::new("me".into(), PathBuf::from("/nowhere"));
        let ask = |path: &str, origin: Option<&str>| {
            route(
                &state,
                &Asked {
                    method: &Method::Post,
                    // A source no config has, so that whatever gets past the
                    // guard stops there, having changed and started nothing.
                    query: HashMap::from([("source".into(), "no-such-source".into())]),
                    origin,
                    host: Some("server.tailnet.ts.net:7777"),
                    body: b"{}",
                    ..asked(path, Some("me"))
                },
            )
            .status
        };
        for path in [
            "/api/issues",
            "/api/issues/END-1",
            "/api/issues/END-1/comment",
            "/api/issues/END-1/pick",
        ] {
            for other in [None, Some("https://evil.example")] {
                assert_eq!(ask(path, other), 403, "{path} {other:?}");
            }
            // The same ask from the server's own page is heard.
            let own = Some("https://server.tailnet.ts.net:7777");
            assert_ne!(ask(path, own), 403, "{path}");
        }
    }

    #[test]
    fn another_site_cannot_subscribe_or_unsubscribe_a_device() {
        let dir = std::env::temp_dir().join(format!("treetop-serve-push-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut state = State::new("me".into(), PathBuf::from("/nowhere"));
        state.push = Some(Arc::new(push::Push::open(dir.clone(), "me").unwrap()));
        // Nothing here sends a push, and the endpoint is one no push service has.
        let device: &[u8] =
            br#"{"endpoint": "https://push.invalid/phone", "keys": {"p256dh": "k", "auth": "a"}}"#;
        let gone: &[u8] = br#"{"endpoint": "https://push.invalid/phone"}"#;
        let ask = |path: &str, origin: Option<&str>, body: &[u8]| {
            route(
                &state,
                &Asked {
                    method: &Method::Post,
                    origin,
                    host: Some("server.tailnet.ts.net:7777"),
                    body,
                    ..asked(path, Some("me"))
                },
            )
            .status
        };
        let is_kept = || {
            std::fs::read_to_string(dir.join("subscriptions.json"))
                .is_ok_and(|text| text.contains("push.invalid/phone"))
        };
        let own = Some("https://server.tailnet.ts.net:7777");
        for other in [None, Some("https://evil.example")] {
            assert_eq!(ask("/api/push/subscribe", other, device), 403, "{other:?}");
            assert!(!is_kept(), "{other:?}");
        }
        // The same ask from the server's own page is heard.
        assert_eq!(ask("/api/push/subscribe", own, device), 200);
        assert!(is_kept());
        for other in [None, Some("https://evil.example")] {
            assert_eq!(ask("/api/push/unsubscribe", other, gone), 403, "{other:?}");
            assert!(is_kept(), "{other:?}");
        }
        assert_eq!(ask("/api/push/unsubscribe", own, gone), 200);
        assert!(!is_kept());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_allow_lets_a_prompt_s_choice_stand() {
        use answer::Press::{Allow, Clear, Deny};
        assert_eq!(answer_of("allow"), Some((Allow, false)));
        assert_eq!(answer_of("deny"), Some((Deny, false)));
        assert_eq!(answer_of("say"), Some((Clear, true)));
        assert_eq!(answer_of("yes"), None);
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
