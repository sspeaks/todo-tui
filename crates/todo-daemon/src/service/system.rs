use todo_proto::v1::system_service_server::SystemService;
use todo_proto::v1::{
    ClientKind, GetServerInfoRequest, GetServerInfoResponse, ShutdownRequest, ShutdownResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Ctx;
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
}
