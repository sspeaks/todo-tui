//! Where the CLI's RPCs go (ADR-0008): the local daemon over its Unix
//! socket, or a daemon elsewhere through its remote listener, as gRPC-Web
//! over HTTPS with the bearer token `todo remote pair` printed there. Each
//! command takes a [`Conn`] and builds its service clients on it alike.

use std::time::Duration;

use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use todo_daemon::config::{ClientRemote, Config, remote_url};
use todo_daemon::paths::Paths;
use todo_sync::credential::{SystemSecrets, resolve};
use tonic::body::Body;
use tonic::codegen::http::{Request, Response, Uri};
use tonic::transport::Channel;
use tower::util::BoxCloneSyncService;
use tower::{BoxError, ServiceBuilder, ServiceExt as _};

/// One daemon connection, local or remote: the service the generated
/// clients run on (`FooServiceClient::new(conn)`), as a Channel is.
#[derive(Clone)]
pub(crate) struct Conn {
    svc: Svc,
    /// The daemon is elsewhere: no spawning, restarting or stopping it here.
    pub(crate) remote: bool,
}

impl tower::Service<Request<Body>> for Conn {
    type Response = Response<Body>;
    type Error = BoxError;
    type Future = <Svc as tower::Service<Request<Body>>>::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), BoxError>> {
        self.svc.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        self.svc.call(req)
    }
}

type Svc = BoxCloneSyncService<Request<Body>, Response<Body>, BoxError>;

impl Conn {
    pub(crate) fn local(channel: Channel) -> Conn {
        Conn {
            svc: BoxCloneSyncService::new(channel.map_err(BoxError::from)),
            remote: false,
        }
    }

    /// The remote listener at `url` (an origin), each call carrying `token`.
    pub(crate) fn remote(url: &str, token: String) -> anyhow::Result<Conn> {
        let origin: Uri = url.parse()?;
        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .try_with_platform_verifier()?
            .https_or_http()
            .enable_http1()
            .build();
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(30))
            .build::<_, tonic_web::GrpcWebCall<Body>>(https);
        let auth: tonic::codegen::http::HeaderValue = format!("Bearer {token}")
            .parse()
            .map_err(|_| anyhow::anyhow!("the token is not plain text"))?;
        let svc = ServiceBuilder::new()
            .map_request(move |mut req: Request<Body>| {
                // The generated clients send a path only (as with_origin
                // does): the listener's scheme and host go in front.
                let mut parts = req.uri().clone().into_parts();
                parts.scheme = origin.scheme().cloned();
                parts.authority = origin.authority().cloned();
                if let Ok(uri) = Uri::from_parts(parts) {
                    *req.uri_mut() = uri;
                }
                req.headers_mut().insert("authorization", auth.clone());
                req
            })
            .map_response(|res: Response<_>| res.map(Body::new))
            .layer(tonic_web::GrpcWebClientLayer::new())
            .service(client)
            .map_err(BoxError::from);
        Ok(Conn {
            svc: BoxCloneSyncService::new(svc),
            remote: true,
        })
    }
}

/// The daemon elsewhere this machine's clients use, if any, said for a
/// message and without reading its token: `TODO_REMOTE`, else `[client]
/// remote`, or a `[client]` table that is broken (so nothing here falls
/// back to a daemon of its own).
pub(crate) fn elsewhere(paths: &Paths) -> Option<String> {
    match std::env::var("TODO_REMOTE") {
        Ok(url) if !url.trim().is_empty() => Some(format!("the daemon at {}", url.trim())),
        _ => match client_config(paths) {
            Ok(client) => client.map(|c| format!("the daemon at {}", c.url)),
            Err(why) => Some(format!(
                "the daemon [client] names in {}, which is broken: {why}",
                paths.config.display()
            )),
        },
    }
}

/// `[client]` in the config: `None` when there is none, an error saying why
/// when there is one that names no daemon (a missing credential, a typo),
/// or when the file is not TOML and has a `[client]` line.
fn client_config(paths: &Paths) -> Result<Option<ClientRemote>, String> {
    match Config::load(&paths.config) {
        Ok(Config {
            client: Some(client),
            ..
        }) => Ok(Some(client)),
        Ok(config) => config.client_error.map_or(Ok(None), Err),
        Err(e) => {
            let text = std::fs::read_to_string(&paths.config).unwrap_or_default();
            if text.lines().any(|l| l.trim_start().starts_with("[client]")) {
                Err(e.message)
            } else {
                Ok(None)
            }
        }
    }
}

/// The daemon elsewhere this machine's clients use, if any (ADR-0008):
/// `TODO_REMOTE` (with `TODO_REMOTE_TOKEN`) over `[client] remote` in the
/// config. `None`: the local daemon.
pub(crate) fn remote_of(paths: &Paths) -> anyhow::Result<Option<(String, String)>> {
    if let Ok(url) = std::env::var("TODO_REMOTE")
        && !url.trim().is_empty()
    {
        let url = remote_url(&url).map_err(|why| anyhow::anyhow!("TODO_REMOTE {why}"))?;
        let token = std::env::var("TODO_REMOTE_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("TODO_REMOTE is set without TODO_REMOTE_TOKEN"))?;
        return Ok(Some((url, token.trim().to_owned())));
    }
    let Some(ClientRemote { url, credential }) = client_config(paths)
        .map_err(|why| anyhow::anyhow!("{} [client]: {why}", paths.config.display()))?
    else {
        return Ok(None);
    };
    let token = resolve(&credential, &SystemSecrets)
        .map_err(|e| anyhow::anyhow!("[client] credential {credential}: {e}"))?;
    Ok(Some((url, token.expose().to_owned())))
}
