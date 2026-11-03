use std::pin::Pin;
use todo_proto::v1::system_service_server::SystemService;

use futures_util::{Stream, StreamExt as _};
use todo_proto::export::{
    FORMAT, FORMAT_VERSION, Header, header_line, read_header, read_record, record_line,
};
use todo_proto::v1::{
    self, BackupRequest, BackupResponse, ClientKind, ExportRequest, ExportResponse,
    GetConfigRequest, GetConfigResponse, GetServerInfoRequest, GetServerInfoResponse,
    ImportRequest, ImportResponse, ReloadConfigRequest, ReloadConfigResponse, ShutdownRequest,
    ShutdownResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Ctx;
use crate::config::ReloadError;
use crate::conn::ConnId;
use crate::error;

pub(crate) struct System(pub(crate) Ctx);

/// Lines an Export message carries.
const EXPORT_BATCH: usize = 500;

/// The most export text an Import takes, all lines together (1 GiB):
/// personal scale is far below it.
pub(crate) const MAX_IMPORT_BYTES: usize = 1 << 30;

type ExportStream = Pin<Box<dyn Stream<Item = Result<ExportResponse, Status>> + Send>>;

#[tonic::async_trait]
impl SystemService for System {
    type ExportStream = ExportStream;

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

    async fn backup(
        &self,
        _request: Request<BackupRequest>,
    ) -> Result<Response<BackupResponse>, Status> {
        let backup = &self.0.config.current().backup;
        let retain = backup.enabled.then_some(backup.retain_days);
        let run = crate::backup::run_once(&self.0.store, &self.0.backups, retain)
            .await
            .map_err(error::store)?;
        Ok(Response::new(BackupResponse {
            path: run.path.display().to_string(),
            bytes: run.bytes,
            removed: run
                .removed
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        }))
    }

    async fn export(
        &self,
        request: Request<ExportRequest>,
    ) -> Result<Response<ExportStream>, Status> {
        let history = !request.into_inner().no_history;
        let export = self.0.store.export(history).await.map_err(error::store)?;
        let header = Header {
            format: FORMAT.to_owned(),
            format_version: FORMAT_VERSION,
            schema_version: export.schema_version,
            exported_at: Some(todo_proto::convert::timestamp_to_proto(
                jiff::Timestamp::now(),
            )),
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            revision: export.revision,
            history,
        };
        let mut lines = vec![header_line(&header)];
        lines.extend(export.rows.iter().map(record_line));
        let batches: Vec<Result<ExportResponse, Status>> = lines
            .chunks(EXPORT_BATCH)
            .map(|chunk| {
                Ok(ExportResponse {
                    lines: chunk.to_vec(),
                })
            })
            .collect();
        Ok(Response::new(Box::pin(futures_util::stream::iter(batches))))
    }

    async fn import(
        &self,
        request: Request<tonic::Streaming<ImportRequest>>,
    ) -> Result<Response<ImportResponse>, Status> {
        let mut stream = request.into_inner();
        let mut header = None;
        let mut rows = Vec::new();
        // The line each record came from, for an error the store names by record.
        let mut lines_of = Vec::new();
        let mut bytes = 0usize;
        let mut line_no = 0usize;
        while let Some(message) = stream.next().await {
            for line in message?.lines {
                line_no += 1;
                bytes += line.len();
                if bytes > MAX_IMPORT_BYTES {
                    return Err(Status::resource_exhausted(
                        "the export is larger than 1 GiB",
                    ));
                }
                if line.trim().is_empty() {
                    continue;
                }
                let at = |e: todo_proto::export::LineError| {
                    error::bad_request(&format!("line {line_no}"), e.0)
                };
                if header.is_none() {
                    header = Some(read_header(&line).map_err(at)?);
                } else {
                    rows.push(read_record(&line).map_err(at)?);
                    lines_of.push(line_no);
                }
            }
        }
        let header = header.ok_or_else(|| error::bad_request("line 1", "an empty file"))?;
        let out = self
            .0
            .store
            .import(header.revision, rows)
            .await
            .map_err(|e| match e {
                todo_store::StoreError::Import { record, error } => {
                    let line = lines_of.get(record).copied().unwrap_or(0);
                    error::bad_request(&format!("line {line}"), error)
                }
                e => error::store(e),
            })?;
        Ok(Response::new(ImportResponse {
            items: out.items,
            records: out.records,
            revision: out.revision,
        }))
    }
}
