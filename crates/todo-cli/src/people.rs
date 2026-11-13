//! `todo people [query] [--jira]` (design 3.5): the people sync knows, and
//! with `--jira` those Jira's directory finds that it does not.

use std::process::ExitCode;

use todo_daemon::paths::Paths;
use todo_proto::v1::people_service_client::PeopleServiceClient;
use todo_proto::v1::{ListPeopleRequest, SearchDirectoryRequest};

use crate::print_out;
use crate::rpc::{call, ready};
use crate::status::failed;

/// One line per person: their name, then `<address>` and `(username)` when
/// known.
pub(crate) async fn people(
    paths: &Paths,
    query: Vec<String>,
    jira: bool,
) -> anyhow::Result<ExitCode> {
    let channel = ready(paths).await.map_err(|why| anyhow::anyhow!("{why}"))?;
    let query = query.join(" ");
    let mut people = PeopleServiceClient::new(channel);
    let known = call(people.list_people(ListPeopleRequest {
        query: query.clone(),
        limit: 0,
    }))
    .await
    .map_err(failed)?
    .people;
    for p in &known {
        print_out(line(&p.display_name, p.emails.first(), &p.username));
    }
    if jira {
        if query.trim().is_empty() {
            anyhow::bail!("--jira needs a name to search for");
        }
        let found = call(people.search_directory(SearchDirectoryRequest {
            query,
            instance_id: String::new(),
        }))
        .await
        .map_err(failed)?
        .users;
        for u in &found {
            let email = Some(&u.email).filter(|e| !e.is_empty());
            print_out(format!(
                "{} · from Jira",
                line(&u.display_name, email, &u.username)
            ));
        }
        if known.is_empty() && found.is_empty() {
            print_out("no one");
        }
    } else if known.is_empty() {
        print_out("no one (--jira asks Jira's directory)");
    }
    Ok(ExitCode::SUCCESS)
}

fn line(name: &str, email: Option<&String>, username: &str) -> String {
    let mut out = name.to_owned();
    if let Some(e) = email {
        out.push_str(&format!(" <{e}>"));
    }
    if !username.is_empty() {
        out.push_str(&format!(" ({username})"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::line;

    #[test]
    fn a_line_shows_what_is_known() {
        let e = "dana@example.com".to_owned();
        assert_eq!(
            line("Dana Example", Some(&e), "dana"),
            "Dana Example <dana@example.com> (dana)"
        );
        assert_eq!(line("Sam Example", None, ""), "Sam Example");
    }
}
