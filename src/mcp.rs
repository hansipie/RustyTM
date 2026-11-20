//! MCP (Model Context Protocol) server: gives an AI assistant access to RustyTM's data and actions.
//!
//! Two transports share one JSON-RPC handler:
//! * Streamable HTTP (`POST /mcp`, JSON responses, no SSE stream) on 127.0.0.1, started from the
//!   Settings page or with `--mcp-http`. Every request needs `Authorization: Bearer <token>`.
//! * stdio (`--mcp`): newline-delimited JSON-RPC on stdin / stdout, for clients that launch RustyTM.
//!
//! The transport threads never touch the app: a `tools/call` becomes a [`Job`] sent to the UI thread,
//! which runs it between frames (see `App::mcp_poll`). Data that is collected in the background
//! (services, connections...) answers [`Outcome::Pending`] until it is ready; the transport then
//! retries, so the UI thread is never blocked.

use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 7419;
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const MAX_BODY: usize = 1 << 20;
const MAX_HEADERS: usize = 16 << 10;
/// Authenticated requests running at once.
const MAX_CONNECTIONS: usize = 16;
/// Connections that have not shown the token yet, and how long they get to send their headers.
const MAX_UNAUTHED: usize = 8;
const HEADER_DEADLINE: Duration = Duration::from_secs(2);
/// Used once those 8 are taken: a client on this machine sends its headers in well under this.
const MAX_OVERFLOW: usize = 32;
const OVERFLOW_DEADLINE: Duration = Duration::from_millis(250);
/// A client must send its whole request within this time, however slowly the bytes arrive.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);
/// How often and how long a pending tool call is retried (background data still loading).
const RETRY_EVERY: Duration = Duration::from_millis(250);
const RETRY_FOR: u32 = 120;

/// A tool call handed to the UI thread.
pub struct Job {
    pub tool: String,
    pub args: Value,
    /// false when this is a retry of a call that answered `Pending`
    pub first: bool,
    pub reply: Sender<Outcome>,
}

pub enum Outcome {
    Done(Result<Value, String>),
    /// The data is still being collected: ask again shortly.
    Pending,
    /// Slow work (a command, a file to parse) the transport thread runs itself, off the UI thread.
    Run(Box<dyn FnOnce() -> Result<Value, String> + Send>),
}

pub struct Handle {
    pub rx: Receiver<Job>,
    /// HTTP: the address listened on (None for stdio)
    pub addr: Option<SocketAddr>,
    pub port: u16,
    token: Arc<Mutex<String>>,
    /// stdio: set when stdin is closed (the client went away)
    pub closed: Arc<AtomicBool>,
    allow_actions: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

impl Handle {
    pub fn set_allow_actions(&self, v: bool) {
        self.allow_actions.store(v, Ordering::Relaxed);
    }

    /// Clients must send this token from now on (HTTP).
    pub fn set_token(&self, t: &str) {
        if let Ok(mut x) = self.token.lock() {
            *x = t.to_string();
        }
    }

    pub fn token(&self) -> String {
        self.token.lock().map(|t| t.clone()).unwrap_or_default()
    }

    pub fn url(&self) -> Option<String> {
        self.addr.map(|a| format!("http://{a}/mcp"))
    }

    /// The `claude mcp add` command line for this server.
    pub fn claude_command(&self) -> Option<String> {
        self.url().map(|u| claude_command(&u, &self.token()))
    }
}

pub fn claude_command(url: &str, token: &str) -> String {
    format!("claude mcp add --transport http rustytm {url} --header \"Authorization: Bearer {token}\"")
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A random 128-bit token in hex.
pub fn new_token() -> String {
    let mut b = [0u8; 16];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).is_ok();
    if !ok {
        // no /dev/urandom: mix time and pid (weak, but never empty)
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) ^ ((std::process::id() as u128) << 64);
        b = t.to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ───────────── HTTP transport ─────────────

/// Listens on 127.0.0.1:`port`. Fails when the port is taken.
pub fn serve_http(port: u16, token: String, allow_actions: bool) -> Result<Handle, String> {
    let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("cannot listen on 127.0.0.1:{port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let allow = Arc::new(AtomicBool::new(allow_actions));
    let token = Arc::new(Mutex::new(token));
    let h = Handle { rx, addr: Some(addr), port: addr.port(), token: token.clone(), closed: Arc::new(AtomicBool::new(false)), allow_actions: allow.clone(), stop: stop.clone() };
    std::thread::spawn(move || {
        let unauthed = Arc::new(AtomicUsize::new(0));
        let ctx = Arc::new(Conn { tx, allow, token, stop: stop.clone(), authed: Arc::new(AtomicUsize::new(0)) });
        let overflow = Arc::new(AtomicUsize::new(0));
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((s, _)) => {
                    // connections without the token yet go to a small pool with a short header deadline: the
                    // client with the token gets out of it within one round trip
                    // when it is full, an overflow pool with a much shorter deadline still lets a client
                    // that sends its headers at once (as real clients do) through
                    let taken = Slot::take(&unauthed, MAX_UNAUTHED).map(|s| (s, HEADER_DEADLINE)).or_else(|| Slot::take(&overflow, MAX_OVERFLOW).map(|s| (s, OVERFLOW_DEADLINE)));
                    let Some((slot, head_deadline)) = taken else {
                        let mut s = s;
                        let _ = respond(&mut s, 503, "Service Unavailable", &[], b"");
                        continue;
                    };
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        let _ = handle_conn(s, slot, head_deadline, &ctx);
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    });
    Ok(h)
}

/// One place in a connection pool, given back when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(pool: &Arc<AtomicUsize>, max: usize) -> Option<Slot> {
        pool.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < max).then_some(n + 1)).ok()?;
        Some(Slot(pool.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    /// body bytes received with the headers
    rest: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Reads up to `max` bytes, waiting at most until `deadline`: a client trickling bytes cannot
/// hold its connection past it.
fn read_until(s: &mut TcpStream, buf: &mut [u8], start: Instant, deadline: Duration) -> Option<usize> {
    let left = deadline.checked_sub(start.elapsed()).filter(|d| !d.is_zero())?;
    s.set_read_timeout(Some(left)).ok()?;
    match s.read(buf) {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// The request line and headers, received within `deadline`.
fn read_head(s: &mut TcpStream, start: Instant, deadline: Duration) -> Result<Option<Request>, (u16, &'static str)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
        if buf.len() > MAX_HEADERS {
            return Err((431, "Request Header Fields Too Large"));
        }
        match read_until(s, &mut chunk, start, deadline) {
            Some(n) => buf.extend_from_slice(&chunk[..n]),
            None => return Ok(None),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let path = first.next().unwrap_or("").split('?').next().unwrap_or("").to_string();
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
    Ok(Some(Request { method, path, headers, rest: buf[head_end + 4..].to_vec() }))
}

/// The body, received within REQUEST_DEADLINE of the connection's start.
fn read_body(s: &mut TcpStream, req: &Request, start: Instant) -> Result<Option<Vec<u8>>, (u16, &'static str)> {
    if req.header("transfer-encoding").is_some() {
        return Err((411, "Length Required"));
    }
    let len: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return Err((413, "Payload Too Large"));
    }
    let mut body = req.rest.clone();
    let mut chunk = [0u8; 4096];
    while body.len() < len {
        match read_until(s, &mut chunk, start, REQUEST_DEADLINE) {
            Some(n) => body.extend_from_slice(&chunk[..n]),
            None => return Ok(None),
        }
    }
    body.truncate(len);
    Ok(Some(body))
}

fn respond(s: &mut TcpStream, code: u16, reason: &str, extra: &[(&str, &str)], body: &[u8]) -> std::io::Result<()> {
    let mut h = format!("HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n", body.len());
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    s.write_all(h.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

/// Only pages served from this machine may call the server (DNS rebinding protection).
fn origin_ok(origin: Option<&str>) -> bool {
    let Some(o) = origin else { return true };
    let rest = o.strip_prefix("http://").or_else(|| o.strip_prefix("https://")).unwrap_or("");
    let host = if rest.starts_with('[') { rest.split(']').next().map(|h| format!("{h}]")).unwrap_or_default() } else { rest.split([':', '/']).next().unwrap_or("").to_string() };
    matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// Constant-time comparison of the bearer token.
fn token_ok(auth: Option<&str>, token: &str) -> bool {
    let Some(given) = auth.and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("bearer "))) else { return false };
    let (a, b) = (given.trim().as_bytes(), token.as_bytes());
    !token.is_empty() && a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// What every connection thread shares.
struct Conn {
    tx: Sender<Job>,
    allow: Arc<AtomicBool>,
    token: Arc<Mutex<String>>,
    stop: Arc<AtomicBool>,
    /// authenticated requests in flight (MAX_CONNECTIONS)
    authed: Arc<AtomicUsize>,
}

/// `unauthed`: this connection's place in the small pool of connections that have not shown the
/// token yet. It is given up once the headers are checked; an authenticated request then takes a
/// place in `authed` (MAX_CONNECTIONS) for as long as it runs.
fn handle_conn(mut s: TcpStream, unauthed: Slot, head_deadline: Duration, ctx: &Conn) -> std::io::Result<()> {
    let (tx, allow, token, stop, authed) = (&ctx.tx, &*ctx.allow, &*ctx.token, &*ctx.stop, &ctx.authed);
    let start = Instant::now();
    s.set_nonblocking(false)?;
    s.set_write_timeout(Some(Duration::from_secs(10)))?;
    let head = read_head(&mut s, start, head_deadline);
    let req = match head {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(()),
        Err((code, reason)) => return respond(&mut s, code, reason, &[], b""),
    };
    if !origin_ok(req.header("origin")) {
        return respond(&mut s, 403, "Forbidden", &[], b"origin not allowed");
    }
    if req.path != "/mcp" && req.path != "/" {
        return respond(&mut s, 404, "Not Found", &[], b"");
    }
    let token = token.lock().map(|t| t.clone()).unwrap_or_default();
    if !token_ok(req.header("authorization"), &token) {
        return respond(&mut s, 401, "Unauthorized", &[("WWW-Authenticate", "Bearer")], b"missing or wrong bearer token");
    }
    drop(unauthed);
    let Some(_slot) = Slot::take(authed, MAX_CONNECTIONS) else {
        return respond(&mut s, 503, "Service Unavailable", &[], b"too many requests in flight");
    };
    match req.method.as_str() {
        "POST" => {}
        _ => return respond(&mut s, 405, "Method Not Allowed", &[("Allow", "POST")], b""),
    }
    if stop.load(Ordering::Relaxed) {
        return respond(&mut s, 503, "Service Unavailable", &[], b"");
    }
    let body = match read_body(&mut s, &req, start) {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(()),
        Err((code, reason)) => return respond(&mut s, code, reason, &[], b""),
    };
    let reply = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => handle_message(&v, tx, allow),
        Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
    };
    match reply {
        Some(v) => respond(&mut s, 200, "OK", &[("Content-Type", "application/json")], v.to_string().as_bytes()),
        None => respond(&mut s, 202, "Accepted", &[], b""),
    }
}


// ───────────── stdio transport ─────────────

/// Reads JSON-RPC messages from stdin, one per line, and writes the replies to stdout.
pub fn serve_stdio(allow_actions: bool) -> Handle {
    let (tx, rx) = channel();
    let closed = Arc::new(AtomicBool::new(false));
    let allow = Arc::new(AtomicBool::new(allow_actions));
    let h = Handle { rx, addr: None, port: 0, token: Arc::new(Mutex::new(String::new())), closed: closed.clone(), allow_actions: allow.clone(), stop: Arc::new(AtomicBool::new(false)) };
    std::thread::spawn(move || {
        let out = Arc::new(Mutex::new(std::io::stdout()));
        let busy = Arc::new(AtomicUsize::new(0));
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            let (tx, allow, out, busy) = (tx.clone(), allow.clone(), out.clone(), busy.clone());
            busy.fetch_add(1, Ordering::SeqCst);
            // one thread per message: a slow tool call does not hold up the others
            std::thread::spawn(move || {
                let reply = match serde_json::from_str::<Value>(&line) {
                    Ok(v) => handle_message(&v, &tx, &allow),
                    Err(e) => Some(rpc_error(Value::Null, -32700, &format!("parse error: {e}"))),
                };
                if let Some(r) = reply {
                    if let Ok(mut o) = out.lock() {
                        let _ = writeln!(o, "{r}");
                        let _ = o.flush();
                    }
                }
                busy.fetch_sub(1, Ordering::SeqCst);
            });
        }
        // let calls in flight answer before the app exits
        for _ in 0..200 {
            if busy.load(Ordering::SeqCst) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        closed.store(true, Ordering::SeqCst);
    });
    h
}

// ───────────── JSON-RPC / MCP ─────────────

fn rpc_error(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// Handles one JSON-RPC message (or a batch); None when nothing is to be sent back.
pub fn handle_message(msg: &Value, tx: &Sender<Job>, allow: &AtomicBool) -> Option<Value> {
    if let Some(batch) = msg.as_array() {
        let out: Vec<Value> = batch.iter().filter_map(|m| handle_one(m, tx, allow)).collect();
        return if out.is_empty() { None } else { Some(Value::Array(out)) };
    }
    handle_one(msg, tx, allow)
}

fn handle_one(msg: &Value, tx: &Sender<Job>, allow: &AtomicBool) -> Option<Value> {
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // a response from the client (we never send requests) or garbage
        return msg.get("id").filter(|_| msg.get("result").is_none() && msg.get("error").is_none()).map(|id| rpc_error(id.clone(), -32600, "invalid request"));
    };
    // notifications get no answer
    let id = msg.get("id")?.clone();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let v = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[0]);
            rpc_ok(
                id,
                json!({
                    "protocolVersion": v,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "rustytm", "title": "RustyTM", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                }),
            )
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => {
            let actions = allow.load(Ordering::Relaxed);
            let tools: Vec<Value> = TOOLS.iter().filter(|t| actions || !t.action).map(Tool::to_json).collect();
            rpc_ok(id, json!({ "tools": tools }))
        }
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else { return Some(rpc_error(id, -32602, "missing tool name")) };
            let Some(tool) = TOOLS.iter().find(|t| t.name == name) else { return Some(rpc_error(id, -32602, &format!("unknown tool: {name}"))) };
            let args = params.get("arguments").cloned().filter(|a| a.is_object()).unwrap_or_else(|| json!({}));
            let res = if tool.action && !allow.load(Ordering::Relaxed) {
                Err("Actions are turned off. Turn on \"MCP server: allow actions\" in RustyTM's Settings page to use this tool.".to_string())
            } else {
                call_tool(tx, name, args)
            };
            rpc_ok(
                id,
                match res {
                    Ok(v) => json!({"content": [{"type": "text", "text": v.to_string()}], "isError": false}),
                    Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
                },
            )
        }
        _ => rpc_error(id, -32601, &format!("method not found: {method}")),
    })
}

/// Runs a tool on the UI thread, retrying while its data is being collected.
fn call_tool(tx: &Sender<Job>, name: &str, args: Value) -> Result<Value, String> {
    for attempt in 0..RETRY_FOR {
        let (rtx, rrx) = channel();
        tx.send(Job { tool: name.to_string(), args: args.clone(), first: attempt == 0, reply: rtx }).map_err(|_| "RustyTM is shutting down".to_string())?;
        match rrx.recv_timeout(Duration::from_secs(30)) {
            Ok(Outcome::Done(r)) => return r,
            Ok(Outcome::Pending) => std::thread::sleep(RETRY_EVERY),
            Ok(Outcome::Run(f)) => return f(),
            Err(RecvTimeoutError::Timeout) => return Err("RustyTM did not answer in time".into()),
            Err(RecvTimeoutError::Disconnected) => return Err("RustyTM is shutting down".into()),
        }
    }
    Err("The data is still being collected; call the tool again in a few seconds.".into())
}

const INSTRUCTIONS: &str = "RustyTM is a Linux task manager. Read tools return the latest sample taken by the running RustyTM \
(it samples every refresh interval, 1 s by default): sizes are in bytes, rates in bytes per second, CPU in percent (100% = all \
cores unless per_core_cpu is set), temperatures in °C. Action tools (signals, priorities, services, benchmarks, recordings...) \
are only listed when the user allowed actions in RustyTM's settings; they act immediately, without the confirmation the \
TUI asks for, so confirm destructive ones with the user first.";

// ───────────── tool catalogue ─────────────

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON object of the input properties (`{}` for none)
    pub props: &'static str,
    pub required: &'static [&'static str],
    /// changes something: only available when actions are allowed
    pub action: bool,
    pub destructive: bool,
}

impl Tool {
    fn to_json(&self) -> Value {
        let props: Value = serde_json::from_str(self.props).unwrap_or_else(|_| json!({}));
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": {"type": "object", "properties": props, "required": self.required, "additionalProperties": false},
            "annotations": {"readOnlyHint": !self.action, "destructiveHint": self.destructive, "openWorldHint": false},
        })
    }
}

const fn read(name: &'static str, description: &'static str, props: &'static str) -> Tool {
    Tool { name, description, props, required: &[], action: false, destructive: false }
}

macro_rules! filter {
    () => {
        r#""filter": {"type": "string", "description": "case-insensitive substring to match"}"#
    };
}

macro_rules! props {
    ($first:expr $(, $rest:expr)* $(,)?) => { concat!("{", $first, $(",", $rest,)* "}") };
}

pub const TOOLS: &[Tool] = &[
    read("get_overview", "Summary of the machine right now: host, uptime, load, CPU, memory, GPUs, disks, network, batteries, process counts and the top processes by CPU and memory, plus what RustyTM is doing (recording, benchmark, disk scan).", "{}"),
    read("get_cpu", "CPU model, topology, caches, governor, overall and per-logical-CPU usage (user+kernel), frequencies in MHz and the usage history.", "{}"),
    read("get_memory", "Memory and swap usage, cache, commit, pressure (PSI) and paging, with the usage history.", "{}"),
    read("get_disks", "Block devices (throughput, active time, response time, totals) and mounted filesystems (size and free space).", "{}"),
    read("get_network", "Network interfaces: rates, totals, link speed, addresses, Wi-Fi network name.", "{}"),
    read("get_gpus", "GPUs: utilization, memory, clocks, power, temperature, encoder/decoder; optionally the processes using each GPU (VRAM, engines, runtime).", r#"{"include_processes": {"type": "boolean", "description": "per-process GPU usage (default true; may take ~2 s the first time)"}}"#),
    read("get_history", "Recent history of the performance graphs (one value per sample, oldest first): cpu %, memory %, busiest disk %, network rx/tx bytes/s, GPU 0 %, package power W; optionally every device's own series.", r#"{"include_devices": {"type": "boolean", "description": "add per-disk, per-interface, per-GPU and per-sensor series"}}"#),
    Tool {
        name: "list_processes",
        description: "Running processes with CPU, memory, disk and GPU usage, sorted and filtered like the Processes page.",
        props: props!(
            r#""filter": {"type": "string", "description": "substring of name, user or command line, or an exact PID"}"#,
            r#""sort_by": {"type": "string", "enum": ["name", "pid", "cpu", "memory", "disk_read", "disk_write", "user", "status", "threads", "gpu", "vram"], "description": "default cpu"}"#,
            r#""descending": {"type": "boolean", "description": "default true"}"#,
            r#""limit": {"type": "integer", "description": "default 50, max 5000"}"#,
            r#""include_exited": {"type": "boolean", "description": "add processes that exited in the last 30 s"}"#,
        ),
        required: &[],
        action: false,
        destructive: false,
    },
    Tool { name: "get_process", description: "Everything about one process: usage, full details (status, limits, environment, threads, open files, maps), CPU affinity, children and GPU usage.", props: r#"{"pid": {"type": "integer"}}"#, required: &["pid"], action: false, destructive: false },
    read("get_sensors", "Sensor readings (temperatures in °C, fans, voltages, power, frequencies) and the sources that could not be read.", props!(r#""group": {"type": "string", "description": "only this group, e.g. Temperatures"}"#, filter!())),
    read("get_batteries", "Batteries (laptop and peripherals): status, level, energy, power, health, cycles, time estimate, and whether AC power is connected.", "{}"),
    read(
        "get_connections",
        "Network connections and listening sockets with their owning processes and exposure (Connections page).",
        props!(
            r#""scope": {"type": "string", "enum": ["all", "listening", "public"], "description": "default all; public = listening on all interfaces or a public address"}"#,
            r#""include_ipc": {"type": "boolean", "description": "include local IPC (unix) sockets"}"#,
            filter!(),
            r#""limit": {"type": "integer", "description": "default 300"}"#,
            r#""refresh": {"type": "boolean", "description": "collect again even if recent"}"#,
        ),
    ),
    read(
        "get_kernel_modules",
        "Kernel modules / drivers (Drivers page). Give a name for one module's full details and unload hint.",
        props!(r#""name": {"type": "string"}"#, filter!(), r#""loaded_only": {"type": "boolean"}"#, r#""limit": {"type": "integer", "description": "default 300"}"#, r#""refresh": {"type": "boolean"}"#),
    ),
    read("get_installed_apps", "Installed applications (desktop entries, flatpak, snap; optionally packages without a desktop entry).", props!(filter!(), r#""include_packages": {"type": "boolean"}"#, r#""limit": {"type": "integer", "description": "default 300"}"#, r#""refresh": {"type": "boolean"}"#)),
    read("get_disk_health", "SMART health of each disk: verdict, temperature, power-on hours, wear, attributes (needs smartctl; often root).", r#"{"refresh": {"type": "boolean"}}"#),
    read("get_startup_apps", "Applications started at login (XDG autostart) and whether each is enabled.", props!(filter!(), r#""refresh": {"type": "boolean"}"#)),
    read("get_services", "systemd services: active state, sub state, enabled state, main PID.", props!(filter!(), r#""state": {"type": "string", "description": "only this active state, e.g. active, failed, inactive"}"#, r#""limit": {"type": "integer", "description": "default 500"}"#, r#""refresh": {"type": "boolean"}"#)),
    read("get_users", "Users with running processes: login sessions, process count, CPU and memory.", r#"{"refresh": {"type": "boolean"}}"#),
    read("get_system_info", "System Info page: OS, kernel, firmware, CPU, memory modules, disks, network, GPUs, as sections of name/value pairs.", r#"{"refresh": {"type": "boolean"}}"#),
    read("get_app_history", "Cumulative CPU time and disk I/O per executable name since the history was last cleared.", props!(filter!(), r#""limit": {"type": "integer", "description": "default 50"}"#)),
    read("get_disk_scan", "State and result of the Disk Space scanner: totals, categories, largest extensions, files and folders; and the mounted filesystems.", r#"{"limit": {"type": "integer", "description": "entries per list, default 30"}}"#),
    read("get_benchmarks", "Benchmark state (running test, progress, current result) and the history of runs with scores.", "{}"),
    read("get_recorder", "Flight Recorder: current recording, saved traces and the replay position.", "{}"),
    Tool { name: "get_trace", description: "Summary and (downsampled) samples of a saved Flight Recorder trace (a file listed by get_recorder; only that folder is readable).", props: r#"{"path": {"type": "string", "description": "file name or path in the traces folder"}, "max_samples": {"type": "integer", "description": "default 120"}}"#, required: &["path"], action: false, destructive: false },
    read("get_settings", "RustyTM settings and the MCP server status.", "{}"),
    // ── actions ──
    Tool {
        name: "signal_process",
        description: "Send a signal to a process (optionally with its descendants): TERM to end it, KILL to force it, STOP/CONT to suspend/resume, or HUP INT QUIT USR1 USR2 or a number.",
        props: r#"{"pid": {"type": "integer"}, "signal": {"type": "string", "description": "default TERM"}, "include_descendants": {"type": "boolean"}}"#,
        required: &["pid"],
        action: true,
        destructive: true,
    },
    Tool { name: "set_process_priority", description: "Set a process's nice value (-20 highest .. 19 lowest; below 0 needs privileges).", props: r#"{"pid": {"type": "integer"}, "nice": {"type": "integer"}}"#, required: &["pid", "nice"], action: true, destructive: false },
    Tool { name: "set_process_affinity", description: "Set the CPUs a process (all its threads) may run on, as a list like 0-3,6.", props: r#"{"pid": {"type": "integer"}, "cpus": {"type": "string"}}"#, required: &["pid", "cpus"], action: true, destructive: false },
    Tool { name: "write_process_report", description: "Write a sample report of all processes to a file and return its path.", props: "{}", required: &[], action: true, destructive: false },
    Tool { name: "run_program", description: "Run a command line with sh -c (like 'Run new task'); output is discarded.", props: r#"{"command": {"type": "string"}}"#, required: &["command"], action: true, destructive: true },
    Tool {
        name: "service_action",
        description: "Start, stop, restart, enable or disable a systemd service (may ask for authentication).",
        props: r#"{"unit": {"type": "string"}, "action": {"type": "string", "enum": ["start", "stop", "restart", "enable", "disable"]}}"#,
        required: &["unit", "action"],
        action: true,
        destructive: true,
    },
    Tool { name: "set_startup_app_enabled", description: "Enable or disable an application started at login.", props: r#"{"name": {"type": "string", "description": "name or .desktop file path"}, "enabled": {"type": "boolean"}}"#, required: &["name", "enabled"], action: true, destructive: false },
    Tool { name: "lock_session", description: "Lock a login session (loginctl).", props: r#"{"session_id": {"type": "string"}}"#, required: &["session_id"], action: true, destructive: false },
    Tool { name: "terminate_session", description: "Sign off a login session: closes everything running in it.", props: r#"{"session_id": {"type": "string"}}"#, required: &["session_id"], action: true, destructive: true },
    Tool { name: "uninstall_app", description: "Uninstall an installed application (flatpak, snap, dnf, rpm-ostree, apt, rpm); may ask for authentication. Runs in the background.", props: r#"{"name": {"type": "string"}}"#, required: &["name"], action: true, destructive: true },
    Tool { name: "start_disk_scan", description: "Measure the space used under a folder (Disk Space page); read the result with get_disk_scan.", props: r#"{"path": {"type": "string", "description": "default /"}}"#, required: &[], action: true, destructive: false },
    Tool { name: "pause_disk_scan", description: "Pause or resume the running disk scan.", props: r#"{"paused": {"type": "boolean"}}"#, required: &["paused"], action: true, destructive: false },
    Tool { name: "stop_disk_scan", description: "Stop the running disk scan (keeps what was measured).", props: "{}", required: &[], action: true, destructive: false },
    Tool {
        name: "start_benchmark",
        description: "Start a benchmark (CPU, disk, GPU, internet or all); follow it with get_benchmarks.",
        props: r#"{"kind": {"type": "string", "enum": ["cpu", "disk", "gpu", "internet", "all"]}, "folder": {"type": "string", "description": "folder for the disk test (default: the Benchmarks page folder)"}}"#,
        required: &["kind"],
        action: true,
        destructive: false,
    },
    Tool { name: "cancel_benchmark", description: "Cancel the running benchmark.", props: "{}", required: &[], action: true, destructive: false },
    Tool { name: "clear_benchmark_history", description: "Delete every recorded benchmark run. There is no undo.", props: "{}", required: &[], action: true, destructive: true },
    Tool { name: "start_recording", description: "Start a Flight Recorder recording (one sample per refresh).", props: "{}", required: &[], action: true, destructive: false },
    Tool { name: "stop_recording", description: "Stop the current recording (keeps it until saved or discarded).", props: "{}", required: &[], action: true, destructive: false },
    Tool { name: "save_recording", description: "Save the current recording as a .rustytmtrace file and return its path.", props: "{}", required: &[], action: true, destructive: false },
    Tool { name: "discard_recording", description: "Close and discard the current recording.", props: "{}", required: &[], action: true, destructive: true },
    Tool { name: "export_system_info", description: "Export the System Info page to a text file and return its path.", props: "{}", required: &[], action: true, destructive: false },
    Tool { name: "clear_app_history", description: "Clear the App history (cumulative CPU and disk per app).", props: "{}", required: &[], action: true, destructive: true },
    Tool {
        name: "change_setting",
        description: "Change a RustyTM setting: fahrenheit, show_exited, smooth_cpu, process_tree, per_core_cpu, combined_gpus, combined_disks, combined_network, net_bits, status_bar, sidebar, light_theme, confirm_stop_scan (booleans), refresh_ms (250-10000), bench_dir (folder). MCP settings cannot be changed.",
        props: r#"{"name": {"type": "string"}, "value": {}}"#,
        required: &["name", "value"],
        action: true,
        destructive: false,
    },
    Tool { name: "show_page", description: "Switch the RustyTM screen to a page (summary, processes, performance, connections, thermals, battery, power, disk-space, disk-health, apps, app-history, startup, services, drivers, users, system-info, recorder, benchmarks, settings).", props: r#"{"page": {"type": "string"}}"#, required: &["page"], action: true, destructive: false },
];

pub fn is_action(name: &str) -> bool {
    TOOLS.iter().any(|t| t.name == name && t.action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_schemas_are_valid_json() {
        for t in TOOLS {
            let v: Value = serde_json::from_str(t.props).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert!(v.is_object(), "{}", t.name);
            for r in t.required {
                assert!(v.get(r).is_some(), "{}: required {r} not in properties", t.name);
            }
        }
        let mut names: Vec<_> = TOOLS.iter().map(|t| t.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len());
    }

    #[test]
    fn origin_and_token_checks() {
        assert!(origin_ok(None));
        assert!(origin_ok(Some("http://localhost:3000")));
        assert!(origin_ok(Some("http://127.0.0.1")));
        assert!(origin_ok(Some("http://[::1]:8080")));
        assert!(!origin_ok(Some("http://evil.example")));
        assert!(!origin_ok(Some("http://localhost.evil.example")));
        assert!(!origin_ok(Some("null")));
        assert!(token_ok(Some("Bearer abc"), "abc"));
        assert!(!token_ok(Some("Bearer abd"), "abc"));
        assert!(!token_ok(None, "abc"));
        assert!(!token_ok(Some("Bearer "), ""));
        assert_eq!(new_token().len(), 32);
    }

    #[test]
    fn protocol_without_app() {
        let (tx, rx) = channel::<Job>();
        let allow = AtomicBool::new(false);
        let init = handle_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}}), &tx, &allow).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle_message(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), &tx, &allow).is_none());
        let list = handle_message(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}), &tx, &allow).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"list_processes") && !names.contains(&"signal_process"));
        // actions are refused while not allowed, without reaching the app
        let r = handle_message(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "signal_process", "arguments": {"pid": 1}}}), &tx, &allow).unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(rx.try_recv().is_err());
        let r = handle_message(&json!({"jsonrpc": "2.0", "id": 4, "method": "nope"}), &tx, &allow).unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }
}
