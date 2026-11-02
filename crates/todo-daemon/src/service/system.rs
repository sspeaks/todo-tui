use todo_proto::v1::system_service_server::SystemService;
use todo_proto::v1::{
    self, ClientKind, GetConfigRequest, GetConfigResponse, GetServerInfoRequest,
    GetServerInfoResponse, ReloadConfigRequest, ReloadConfigResponse, ShutdownRequest,
    ShutdownResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Ctx;
use crate::config::ReloadError;
use crate::conn::ConnId;
use crate::error;

pub(crate) struct System(pub(crate) Ctx);

#[tonic::async_trait]
impl SystemService for System {
    async fn get_server_info(
        &self,
        request: Request<GetServerInfoRequest>,
    ) -> Result<Response<GetServerInfoResponse>, Status> {
        let conn = request.extensions().get::<ConnId>().copied();
        let req = request.into_inner();
        let kind = ClientKind::try_from(req.client_kind).unwrap_or(ClientKind::Unspecified);
        if let Some(conn) = conn {
            self.0.clients.set_kind(conn, kind);
        }
        tracing::debug!(?kind, client_version = %req.client_version, "client hello");
        Ok(Response::new(GetServerInfoResponse {
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            api_minor: todo_proto::API_MINOR,
            capabilities: Vec::new(),
        }))
    }

    async fn shutdown(
        &self,
        request: Request<ShutdownRequest>,
    ) -> Result<Response<ShutdownResponse>, Status> {
        let conn = request.extensions().get::<ConnId>().copied();
        if request.get_ref().restart {
            let others = self.0.clients.other_interactive(conn);
            if others > 0 {
                return Err(error::error_info(
                    Code::FailedPrecondition,
                    "INTERACTIVE_CLIENTS_CONNECTED",
                    format!("restart required; close the other {others} interactive session(s)"),
                    [("interactive_clients", others.to_string())],
                ));
            }
        }
        tracing::info!(restart = request.get_ref().restart, "shutdown requested");
        // Graceful: the server stops accepting and lets this response finish.
        self.0.shutdown.trigger();
        Ok(Response::new(ShutdownResponse {}))
    }

    async fn get_config(
        &self,
        _request: Request<GetConfigRequest>,
    ) -> Result<Response<GetConfigResponse>, Status> {
        // The revision first: a change recorded after it brings a config at
        // least as new as this one.
        let revision = self.0.store.current_revision();
        Ok(Response::new(GetConfigResponse {
            config: Some(v1::ConfigSnapshot::from(&*self.0.config.current())),
            revision,
        }))
    }

    async fn reload_config(
        &self,
        _request: Request<ReloadConfigRequest>,
    ) -> Result<Response<ReloadConfigResponse>, Status> {
        let (config, changed) = crate::config::reload(&self.0.config, &self.0.store)
            .await
            .map_err(|e| match e {
                ReloadError::File(e) => error::bad_request("config", e.to_string()),
                ReloadError::Store(e) => error::store(e),
            })?;
        Ok(Response::new(ReloadConfigResponse {
            config: Some(v1::ConfigSnapshot::from(&*config)),
            changed,
        }))
    }
}
