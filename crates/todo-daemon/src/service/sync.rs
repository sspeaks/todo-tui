//! SyncService (design 5.11): the sync state, "sync now", and the outbox.

use todo_proto::convert::{EnumField as _, timestamp_to_proto};
use todo_proto::v1::sync_service_server::SyncService;
use todo_proto::v1::{
    self, AttachRequest, AttachResponse, GetSyncStatusRequest, GetSyncStatusResponse,
    ListOutboxRequest, ListOutboxResponse, PromoteRequest, PromoteResponse, SyncNowRequest,
    SyncNowResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Ctx;
use crate::error;

pub(crate) struct Sync(pub(crate) Ctx);

impl Sync {
    /// The instance promote and attach go through, pushing; and the Item.
    async fn target(
        &self,
        item_id: &str,
    ) -> Result<(todo_sync::Instance, todo_core::Item), Status> {
        let config = self.0.config.current();
        let Some(i) = config.instances.first().cloned() else {
            return Err(error::error_info(
                Code::FailedPrecondition,
                "NO_INSTANCE",
                "no [[instances]] are configured to sync",
                [],
            ));
        };
        if !i.push.enabled {
            return Err(error::error_info(
                Code::FailedPrecondition,
                "PUSH_OFF",
                format!(
                    "{} is read-only (push = false): nothing is written to Jira",
                    i.id
                ),
                [],
            ));
        }
        let id = item_id
            .parse::<todo_core::ItemId>()
            .map_err(|_| error::bad_request("item_id", "must be a ULID"))?;
        let got = self
            .0
            .store
            .get_item(id, jiff::Zoned::now().date())
            .await
            .map_err(error::store)?;
        if got.item.is_deleted() {
            return Err(error::store(todo_store::StoreError::NotFound(id)));
        }
        if let Some(link) = &got.item.remote_link {
            return Err(error::store(todo_store::StoreError::AlreadyLinked(
                link.key.clone(),
            )));
        }
        Ok((i, got.item))
    }

    /// Promote (design 5.7), under the link lock from the check that the
    /// Item is local to its link: a second promote of it, or a pass
    /// importing the new issue meanwhile, waits and then sees it linked.
    async fn promote_now(&self, req: PromoteRequest) -> Result<todo_store::LinkOutput, Status> {
        let _held = self.0.links.lock().await;
        let (i, item) = self.target(&req.item_id).await?;
        let adapter = self.adapter(&i).await?;
        let (project, default_type) = match (&*req.project, &i.promote) {
            ("", Some(p)) => (p.project.clone(), p.issue_type.clone()),
            ("", None) => {
                return Err(error::error_info(
                    Code::FailedPrecondition,
                    "NO_PROMOTE_TARGET",
                    "no project given, and no promote_default = { project = \"…\", issue_type = \"…\" } in [[instances]]",
                    [],
                ));
            }
            (p, target) => (
                p.to_owned(),
                target
                    .as_ref()
                    .map_or_else(|| "Task".to_owned(), |t| t.issue_type.clone()),
            ),
        };
        // A subtask of a linked parent is created under it, as a sub-task.
        let parent = match item.core.parent_id {
            Some(p) => self
                .0
                .store
                .get_item(p, jiff::Zoned::now().date())
                .await
                .ok()
                .and_then(|g| g.item.remote_link.map(|l| l.key)),
            None => None,
        };
        let issue_type = match (&*req.issue_type, &parent) {
            ("", Some(_)) => i.subtask_issue_type.clone(),
            ("", None) => default_type,
            (t, _) => t.to_owned(),
        };
        let local_due = i.local_fields.contains(&todo_core::ItemField::DueDate);
        let new = todo_jira::NewIssue {
            project,
            issue_type,
            summary: item.core.title.clone(),
            description: item.core.description.clone(),
            priority: Some(item.core.priority).filter(|p| *p != todo_core::Priority::None),
            labels: item.core.labels.clone(),
            due_date: item.core.due_date.filter(|_| !local_due),
            parent,
        };
        let sent = jiff::Timestamp::now();
        let issue = match adapter.create(&new).await {
            Ok(issue) => issue,
            Err(todo_jira::CreateError::MissingFields(fields)) => {
                let names = fields.keys().cloned().collect::<Vec<_>>().join(", ");
                let said = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {v}"))
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(error::error_info(
                    Code::FailedPrecondition,
                    "MISSING_FIELDS",
                    format!("Jira needs {names} to create it ({said})"),
                    [("fields", names)],
                ));
            }
            // The request may have made the issue before its answer was
            // lost (design 5.7 step 4): look before saying it failed.
            Err(todo_jira::CreateError::Unknown(e)) => {
                match adapter.find_created(&new, sent).await {
                    Ok(Some(found)) => found,
                    Ok(None) => return Err(jira_status(e)),
                    Err(_) => {
                        return Err(error::error_info(
                            Code::Unavailable,
                            "CREATE_UNKNOWN",
                            format!(
                                "Jira did not answer ({e}), and may have made it: look in {} before trying again",
                                new.project
                            ),
                            [],
                        ));
                    }
                }
            }
            // Made, and not read back: once more, else the person links it.
            Err(todo_jira::CreateError::Unread { key, error }) => match adapter.fetch(&key).await {
                Ok(todo_jira::Fetched::Found(issue)) => *issue,
                _ => {
                    return Err(error::error_info(
                        Code::FailedPrecondition,
                        "CREATED_UNREAD",
                        format!(
                            "Jira made {key} but it could not be read back ({error}): link it with L"
                        ),
                        [("key", key)],
                    ));
                }
            },
            Err(todo_jira::CreateError::Jira(e)) => return Err(jira_status(e)),
        };
        // As Jira reads back what was sent: the same, not a change of Jira's.
        let sent_description =
            todo_jira::wiki::to_markdown(&todo_jira::wiki::to_wiki(&item.core.description));
        self.link(&i, &item, issue, Some(sent_description)).await
    }

    /// Attach (design 3.3), under the link lock as promote is.
    async fn attach_now(&self, req: AttachRequest) -> Result<todo_store::LinkOutput, Status> {
        let key = req.key.trim().to_ascii_uppercase();
        // A project key: a letter, then letters, digits or underscores.
        let valid = key.split_once('-').is_some_and(|(p, n)| {
            p.starts_with(|c: char| c.is_ascii_alphabetic())
                && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
        });
        if !valid {
            return Err(error::bad_request(
                "key",
                "must be an issue key, e.g. PLAT-12",
            ));
        }
        let _held = self.0.links.lock().await;
        let (i, item) = self.target(&req.item_id).await?;
        let adapter = self.adapter(&i).await?;
        let issue = match adapter.fetch(&key).await.map_err(jira_status)? {
            todo_jira::Fetched::Found(issue) => *issue,
            _ => return Err(jira_status(todo_jira::JiraError::NotFound)),
        };
        self.link(&i, &item, issue, None).await
    }

    async fn adapter(&self, i: &todo_sync::Instance) -> Result<todo_jira::Adapter, Status> {
        crate::sync::adapter_for(i).await.map_err(jira_status)
    }

    async fn link(
        &self,
        i: &todo_sync::Instance,
        item: &todo_core::Item,
        issue: todo_core::remote::RemoteIssue,
        // Promote's: the description as Jira reads back what it was sent.
        sent_description: Option<String>,
    ) -> Result<todo_store::LinkOutput, Status> {
        let out = self
            .0
            .store
            .link(todo_store::LinkInput {
                item: item.id,
                instance_id: i.id.clone(),
                issue,
                promote: sent_description.is_some(),
                sent_description,
                now: jiff::Timestamp::now(),
                today: jiff::Zoned::now().date(),
            })
            .await
            .map_err(error::store)?;
        // The Item is tracked now: the next pass keeps it fresh.
        self.0.sync_now.poke();
        Ok(out)
    }
}

/// A Jira error as the caller reads it: offline is UNAVAILABLE (try again),
/// a refused token UNAUTHENTICATED, the rest a precondition with Jira's words.
pub(crate) fn jira_status(e: todo_jira::JiraError) -> Status {
    use todo_jira::JiraError as J;
    match e {
        J::Offline(_) | J::Server(..) | J::RateLimited(_) => {
            error::error_info(Code::Unavailable, "JIRA_UNAVAILABLE", e.to_string(), [])
        }
        J::Auth => error::error_info(Code::Unauthenticated, "JIRA_AUTH", e.to_string(), []),
        J::NotFound => error::error_info(
            Code::NotFound,
            "ISSUE_NOT_FOUND",
            "Jira has no such issue you can read",
            [],
        ),
        _ => error::error_info(Code::FailedPrecondition, "JIRA_REFUSED", e.to_string(), []),
    }
}

#[tonic::async_trait]
impl SyncService for Sync {
    async fn get_sync_status(
        &self,
        _request: Request<GetSyncStatusRequest>,
    ) -> Result<Response<GetSyncStatusResponse>, Status> {
        let instances = self.0.sync_status.borrow().values().cloned().collect();
        Ok(Response::new(GetSyncStatusResponse { instances }))
    }

    async fn sync_now(
        &self,
        request: Request<SyncNowRequest>,
    ) -> Result<Response<SyncNowResponse>, Status> {
        let req = request.into_inner();
        let config = self.0.config.current();
        let known = config
            .instances
            .iter()
            .any(|i| req.instance_id.is_empty() || i.id == req.instance_id);
        if !known {
            return Err(error::error_info(
                Code::FailedPrecondition,
                "NO_INSTANCE",
                if req.instance_id.is_empty() {
                    "no [[instances]] are configured to sync".to_owned()
                } else {
                    format!("no instance {:?} is configured", req.instance_id)
                },
                [],
            ));
        }
        let request = self.0.sync_now.poke();
        Ok(Response::new(SyncNowResponse { request }))
    }

    async fn list_outbox(
        &self,
        request: Request<ListOutboxRequest>,
    ) -> Result<Response<ListOutboxResponse>, Status> {
        let req = request.into_inner();
        let instance = Some(req.instance_id).filter(|id| !id.is_empty());
        let entries = self
            .0
            .store
            .outbox(instance)
            .await
            .map_err(error::store)?
            .into_iter()
            .map(|e| v1::OutboxEntry {
                instance_id: e.instance_id,
                item_id: e.item_id.to_string(),
                remote_key: e.key,
                title: e.title,
                field: e.field.to_i32(),
                new: Some((&e.new).into()),
                state: match e.state.as_str() {
                    "pending" => v1::OutboxState::Pending,
                    "in_flight" => v1::OutboxState::InFlight,
                    "failed" => v1::OutboxState::Failed,
                    _ => v1::OutboxState::Unspecified,
                } as i32,
                attempts: e.attempts,
                next_attempt_at: e.next_attempt_at.map(timestamp_to_proto),
                last_error: e.last_error.unwrap_or_default(),
                updated_at: Some(timestamp_to_proto(e.updated_at)),
            })
            .collect();
        Ok(Response::new(ListOutboxResponse { entries }))
    }

    async fn promote(
        &self,
        request: Request<PromoteRequest>,
    ) -> Result<Response<PromoteResponse>, Status> {
        // To the end even if the caller goes: an issue made is linked.
        let me = Sync(self.0.clone());
        let out = tokio::spawn(async move { me.promote_now(request.into_inner()).await })
            .await
            .map_err(|e| Status::internal(format!("promote: {e}")))??;
        Ok(Response::new(PromoteResponse {
            item: Some(v1::Item::from(out.item)),
            result: Some(out.result.into()),
            suggestion_id: out.suggestion.unwrap_or_default(),
        }))
    }

    async fn attach(
        &self,
        request: Request<AttachRequest>,
    ) -> Result<Response<AttachResponse>, Status> {
        let me = Sync(self.0.clone());
        let out = tokio::spawn(async move { me.attach_now(request.into_inner()).await })
            .await
            .map_err(|e| Status::internal(format!("attach: {e}")))??;
        Ok(Response::new(AttachResponse {
            item: Some(v1::Item::from(out.item)),
            result: Some(out.result.into()),
            suggestion_id: out.suggestion.unwrap_or_default(),
        }))
    }
}
