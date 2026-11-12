//! AssistantService (design 6, ADR-0010): refines asked for, and their jobs.

use jiff::Timestamp;
use todo_proto::convert::timestamp_to_proto;
use todo_proto::v1::assistant_service_server::AssistantService;
use todo_proto::v1::{
    self, GetJobRequest, GetJobResponse, ListJobsRequest, ListJobsResponse, RefineRequest,
    RefineResponse,
};
use todo_store::assistant::{Job, JobState};
use tonic::{Request, Response, Status};

use super::Ctx;
use crate::error;

/// Items one refine may name.
const MAX_ITEMS: usize = 20;

pub(crate) struct Assistants(pub(crate) Ctx);

impl Assistants {
    /// The model `[assistant]` asks for, or the refusal when it is off.
    fn model(&self) -> Result<String, Status> {
        match &self.0.config.current().assistant {
            Some(a) => Ok(a.model.clone()),
            None => Err(error::error_info(
                tonic::Code::FailedPrecondition,
                "ASSISTANT_OFF",
                "the assistant is off: set [assistant] provider = \"claude_code\" in the config",
                [],
            )),
        }
    }

    async fn job_proto(&self, job: Job) -> Result<v1::AssistantJob, Status> {
        // A failed job may have refined the Items before the one that failed.
        let suggestion_ids = if matches!(job.state, JobState::Done | JobState::Failed) {
            self.0
                .store
                .job_suggestions(job.id.clone())
                .await
                .map_err(error::store)?
        } else {
            Vec::new()
        };
        Ok(v1::AssistantJob {
            id: job.id,
            item_ids: job.item_ids.iter().map(ToString::to_string).collect(),
            state: match job.state {
                JobState::Queued => v1::AssistantJobState::Queued,
                JobState::Running => v1::AssistantJobState::Running,
                JobState::Done => v1::AssistantJobState::Done,
                JobState::Failed => v1::AssistantJobState::Failed,
            } as i32,
            error: job.error.unwrap_or_default(),
            model: job.model,
            input_tokens: job.input_tokens,
            output_tokens: job.output_tokens,
            cached_tokens: job.cached_tokens,
            cost_usd: job.cost_usd,
            duration_ms: job.duration_ms,
            created_at: Some(timestamp_to_proto(job.created_at)),
            suggestion_ids,
        })
    }
}

#[tonic::async_trait]
impl AssistantService for Assistants {
    async fn refine(
        &self,
        request: Request<RefineRequest>,
    ) -> Result<Response<RefineResponse>, Status> {
        let model = self.model()?;
        let req = request.into_inner();
        if req.item_ids.is_empty() || req.item_ids.len() > MAX_ITEMS {
            return Err(error::bad_request("item_ids", "must name 1 to 20 Items"));
        }
        let Some(ids) = crate::assistant::item_ids(&req.item_ids) else {
            return Err(error::bad_request("item_ids", "must be ULIDs"));
        };
        let today = jiff::Zoned::now().date();
        for &id in &ids {
            let got = self
                .0
                .store
                .get_item(id, today)
                .await
                .map_err(error::store)?;
            if got.item.is_deleted() {
                return Err(error::store(todo_store::StoreError::NotFound(id)));
            }
        }
        let id = self
            .0
            .store
            .queue_refine(ids, model, Timestamp::now())
            .await
            .map_err(error::store)?;
        self.0.assistant.push(id.clone());
        let job = self
            .0
            .store
            .job(id)
            .await
            .map_err(error::store)?
            .expect("just queued");
        Ok(Response::new(RefineResponse {
            job: Some(self.job_proto(job).await?),
        }))
    }

    async fn get_job(
        &self,
        request: Request<GetJobRequest>,
    ) -> Result<Response<GetJobResponse>, Status> {
        let id = request.into_inner().id;
        if id.parse::<ulid::Ulid>().is_err() {
            return Err(error::bad_request("id", "must be a ULID"));
        }
        match self.0.store.job(id).await.map_err(error::store)? {
            Some(job) => Ok(Response::new(GetJobResponse {
                job: Some(self.job_proto(job).await?),
            })),
            None => Err(error::error_info(
                tonic::Code::NotFound,
                "JOB_NOT_FOUND",
                "no such job",
                [],
            )),
        }
    }

    async fn list_jobs(
        &self,
        request: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let limit = match request.into_inner().limit {
            0 => 50,
            n => n.min(500) as usize,
        };
        let mut jobs = Vec::new();
        for job in self.0.store.jobs(limit).await.map_err(error::store)? {
            jobs.push(self.job_proto(job).await?);
        }
        Ok(Response::new(ListJobsResponse { jobs }))
    }
}
