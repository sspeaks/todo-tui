//! PeopleService (design 3.5): the people a pull made, and the tracker's
//! directory for those on no issue that syncs.

use todo_core::PersonId;
use todo_proto::v1::people_service_server::PeopleService;
use todo_proto::v1::{
    self, AddPersonRequest, AddPersonResponse, GetPersonRequest, GetPersonResponse,
    ListPeopleRequest, ListPeopleResponse, SearchDirectoryRequest, SearchDirectoryResponse,
};
use tonic::{Code, Request, Response, Status};

use super::Ctx;
use crate::error;

pub(crate) struct People(pub(crate) Ctx);

/// The most ListPeople returns.
const MAX_LIMIT: u32 = 1000;

impl People {
    /// The instance named, else the first configured.
    fn instance(&self, id: &str) -> Result<todo_sync::Instance, Status> {
        let config = self.0.config.current();
        let found = if id.is_empty() {
            config.instances.first()
        } else {
            config.instances.iter().find(|i| i.id == id)
        };
        found.cloned().ok_or_else(|| {
            error::error_info(
                Code::FailedPrecondition,
                "NO_INSTANCE",
                if id.is_empty() {
                    "no [[instances]] are configured to sync".to_owned()
                } else {
                    format!("no instance {id:?} is configured")
                },
                [],
            )
        })
    }

    async fn adapter(&self, i: &todo_sync::Instance) -> Result<todo_jira::Adapter, Status> {
        crate::sync::adapter_for(i)
            .await
            .map_err(super::sync::jira_status)
    }
}

#[tonic::async_trait]
impl PeopleService for People {
    async fn list_people(
        &self,
        request: Request<ListPeopleRequest>,
    ) -> Result<Response<ListPeopleResponse>, Status> {
        let req = request.into_inner();
        let limit = match req.limit {
            0 => 50,
            n if n <= MAX_LIMIT => n,
            _ => {
                return Err(error::bad_request(
                    "limit",
                    format!("must be 0 to {MAX_LIMIT}"),
                ));
            }
        };
        let (all, revision) = self.0.store.list_people().await.map_err(error::store)?;
        let found = todo_core::person::search(&req.query, &all, limit as usize);
        Ok(Response::new(ListPeopleResponse {
            people: found.into_iter().map(v1::Person::from).collect(),
            revision,
        }))
    }

    async fn get_person(
        &self,
        request: Request<GetPersonRequest>,
    ) -> Result<Response<GetPersonResponse>, Status> {
        let req = request.into_inner();
        let id = req
            .id
            .parse::<PersonId>()
            .map_err(|_| error::bad_request("id", "must be a ULID"))?;
        match self.0.store.get_person(id).await.map_err(error::store)? {
            Some(p) => Ok(Response::new(GetPersonResponse {
                person: Some(v1::Person::from(&p)),
            })),
            None => Err(error::error_info(
                Code::NotFound,
                "PERSON_NOT_FOUND",
                format!("person {id} not found"),
                [("person_id", id.to_string())],
            )),
        }
    }

    async fn search_directory(
        &self,
        request: Request<SearchDirectoryRequest>,
    ) -> Result<Response<SearchDirectoryResponse>, Status> {
        let req = request.into_inner();
        let query = req.query.trim();
        if query.is_empty() {
            return Err(error::bad_request("query", "must not be empty"));
        }
        let i = self.instance(&req.instance_id)?;
        let adapter = self.adapter(&i).await?;
        let found = adapter
            .search_users(query)
            .await
            .map_err(super::sync::jira_status)?;
        let mut users = Vec::with_capacity(found.len());
        for u in found {
            // Those known already are ListPeople's.
            if self
                .0
                .store
                .person_by_identity(i.id.clone(), u.id.clone())
                .await
                .map_err(error::store)?
                .is_some()
            {
                continue;
            }
            users.push(v1::DirectoryUser {
                instance_id: i.id.clone(),
                account_id: u.id,
                display_name: u.display_name,
                username: u.username,
                email: u.email,
            });
        }
        Ok(Response::new(SearchDirectoryResponse { users }))
    }

    async fn add_person(
        &self,
        request: Request<AddPersonRequest>,
    ) -> Result<Response<AddPersonResponse>, Status> {
        let req = request.into_inner();
        let account = req.account_id.trim();
        if account.is_empty() {
            return Err(error::bad_request("account_id", "must not be empty"));
        }
        let i = self.instance(&req.instance_id)?;
        let adapter = self.adapter(&i).await?;
        let Some(user) = adapter
            .get_user(account)
            .await
            .map_err(super::sync::jira_status)?
        else {
            return Err(error::error_info(
                Code::NotFound,
                "USER_NOT_FOUND",
                format!("{} has no user {account:?}", i.id),
                [("account_id", account.to_owned())],
            ));
        };
        let person = self
            .0
            .store
            .add_remote_person(i.id.clone(), user)
            .await
            .map_err(error::store)?;
        Ok(Response::new(AddPersonResponse {
            person: Some(v1::Person::from(&person)),
        }))
    }
}
