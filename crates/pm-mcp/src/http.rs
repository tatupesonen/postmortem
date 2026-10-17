//! Streamable HTTP transport on `127.0.0.1` with a bearer token
//! (Autopsy's mode). [`HttpServer::start`] runs it on a background
//! thread with its own Tokio runtime so the GUI can start and stop it.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio_util::sync::CancellationToken;

use crate::handler::McpHandler;
use crate::server::McpServer;

/// Path of the MCP endpoint.
pub const ENDPOINT: &str = "/mcp";

type Body = BoxBody<Bytes, Infallible>;

/// A running HTTP server; stopped on [`HttpServer::stop`] or drop.
#[derive(Debug)]
pub struct HttpServer {
    addr: SocketAddr,
    token: String,
    token_file: Option<PathBuf>,
    cancel: CancellationToken,
    thread: Option<JoinHandle<()>>,
}

/// A fresh random bearer token (256 bits of UUID v4 randomness, URL-safe).
pub fn new_token() -> String {
    use base64::Engine as _;
    let mut b = Vec::with_capacity(32);
    b.extend_from_slice(uuid_bytes().as_slice());
    b.extend_from_slice(uuid_bytes().as_slice());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn uuid_bytes() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

/// Where the CLI writes the token by default: the user's postmortem
/// settings folder.
pub fn default_token_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("postmortem").join("mcp-token"))
}

/// Writes the token readable by the user only (`0600` on Unix).
pub fn write_token(path: &Path, token: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(token.as_bytes())
}

fn plain(status: StatusCode, text: &'static str) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from_static(text.as_bytes())).boxed());
    *r.status_mut() = status;
    if status == StatusCode::UNAUTHORIZED
        && let Ok(v) = "Bearer".parse()
    {
        r.headers_mut().insert(hyper::header::WWW_AUTHENTICATE, v);
    }
    r
}

/// Constant-time comparison of the `Authorization` header.
fn authorized(req: &Request<Incoming>, token: &str) -> bool {
    let Some(h) = req
        .headers()
        .get(hyper::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(given) = h.strip_prefix("Bearer ") else {
        return false;
    };
    let (a, b) = (given.trim().as_bytes(), token.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl HttpServer {
    /// Binds `127.0.0.1:port` (0: any free port) and serves `server` on a
    /// background thread. The token is written to `token_file` when given.
    pub fn start(
        server: Arc<McpServer>,
        port: u16,
        token_file: Option<&Path>,
    ) -> std::io::Result<Self> {
        let token = new_token();
        if let Some(p) = token_file {
            write_token(p, &token)?;
        }
        let std_listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
        std_listener.set_nonblocking(true)?;
        let addr = std_listener.local_addr()?;
        let cancel = CancellationToken::new();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let (c, tok) = (cancel.clone(), token.clone());
        let thread = std::thread::Builder::new()
            .name("mcp-http".into())
            .spawn(move || {
                rt.block_on(async move {
                    match tokio::net::TcpListener::from_std(std_listener) {
                        Ok(l) => serve(l, server, tok, c).await,
                        Err(e) => tracing::error!("MCP HTTP listener failed: {e}"),
                    }
                });
            })?;
        tracing::info!("MCP HTTP server on http://{addr}{ENDPOINT}");
        Ok(Self {
            addr,
            token,
            token_file: token_file.map(Path::to_path_buf),
            cancel,
            thread: Some(thread),
        })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>/mcp`.
    pub fn url(&self) -> String {
        format!("http://{}{ENDPOINT}", self.addr)
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// A client configuration snippet (Claude Desktop / Claude Code style)
    /// for the GUI's "copy" button.
    pub fn client_config_snippet(&self) -> String {
        serde_json::json!({
            "mcpServers": {
                "postmortem": {
                    "type": "http",
                    "url": self.url(),
                    "headers": {"Authorization": format!("Bearer {}", self.token)},
                }
            }
        })
        .to_string()
    }

    /// Stops serving, waits for the thread and deletes the token file.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Blocks until the server stops (it only stops through [`Self::stop`]).
    pub fn join(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Some(p) = self.token_file.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn serve(
    listener: tokio::net::TcpListener,
    server: Arc<McpServer>,
    token: String,
    cancel: CancellationToken,
) {
    let config = StreamableHttpServerConfig::default()
        .with_cancellation_token(cancel.child_token())
        .with_allowed_hosts(["localhost", "127.0.0.1", "::1"]);
    let service = StreamableHttpService::new(
        move || Ok(McpHandler::new(Arc::clone(&server))),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let token = Arc::new(token);
    loop {
        let accepted = tokio::select! {
            () = cancel.cancelled() => break,
            a = listener.accept() => a,
        };
        let (stream, _) = match accepted {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("MCP HTTP accept failed: {e}");
                continue;
            }
        };
        let (svc, token, cancel) = (service.clone(), Arc::clone(&token), cancel.clone());
        tokio::spawn(async move {
            let handler = hyper::service::service_fn(move |req: Request<Incoming>| {
                let mut svc = svc.clone();
                let token = Arc::clone(&token);
                async move {
                    if req.uri().path() != ENDPOINT {
                        return Ok::<_, Infallible>(plain(StatusCode::NOT_FOUND, "Not Found"));
                    }
                    if !authorized(&req, &token) {
                        return Ok(plain(StatusCode::UNAUTHORIZED, "Unauthorized"));
                    }
                    tower_service::Service::call(&mut svc, req).await
                }
            });
            let conn = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), handler);
            tokio::select! {
                r = conn => if let Err(e) = r { tracing::debug!("MCP HTTP connection: {e}") },
                () = cancel.cancelled() => {}
            }
        });
    }
}

/// `postmortem mcp --http`: serves until the process is killed.
#[allow(clippy::print_stderr, reason = "command-line diagnostics")]
pub fn run_blocking(server: Arc<McpServer>, token_file: Option<&Path>) -> Result<(), String> {
    let port = server.config().port;
    let path = token_file
        .map(Path::to_path_buf)
        .or_else(default_token_path);
    let http = HttpServer::start(server, port, path.as_deref())
        .map_err(|e| format!("cannot start HTTP server: {e}"))?;
    eprintln!("postmortem MCP server listening on {}", http.url());
    match &path {
        Some(p) => eprintln!("bearer token written to {}", p.display()),
        None => eprintln!("bearer token: {}", http.token()),
    }
    http.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_url_safe() {
        let (a, b) = (new_token(), new_token());
        assert_ne!(a, b);
        assert_eq!(a.len(), 43);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        );
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("tok");
        write_token(&p, "abc").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "abc");
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
