//! People (design 3.5): the tracker's users as Person rows, found by their
//! identity on each pull, and listed for `~` and the `w` prompt.

use std::sync::Arc;

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use todo_core::remote::RemoteUser;
use todo_core::{Identity, Person, PersonId, PersonRef, Topic, new_ulid};

use crate::codec::ts_to_sql;
use crate::{Resource, Result, StoreError, WriteTx};

/// The Person of `user` on `instance`: found by identity, its name, username
/// or email brought up to what the tracker says (an email it hides is kept),
/// else made. Each change is a [`Topic::Person`] change.
pub(crate) fn upsert_remote(
    tx: &mut WriteTx<'_>,
    instance: &str,
    user: &RemoteUser,
    now: Timestamp,
) -> Result<PersonId> {
    let conn = tx.conn();
    let found: Option<String> = conn
        .prepare_cached(
            "SELECT person_id FROM person_identities WHERE instance_id = ?1 AND account_id = ?2",
        )?
        .query_row(params![instance, user.id], |r| r.get(0))
        .optional()?;
    let at = ts_to_sql(now)?;
    let id = match found {
        Some(id) => {
            let id = person_id(&id)?;
            let old = load(conn, id)?.ok_or_else(|| {
                StoreError::Corrupt(format!("identity {instance}/{} names no person", user.id))
            })?;
            let mut next = old.clone();
            next.display_name = user.display_name.clone();
            if !user.username.is_empty() {
                next.username = user.username.clone();
            }
            if !user.email.is_empty()
                && !next
                    .emails
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(&user.email))
            {
                // The tracker's address first: it is the current one.
                next.emails.insert(0, user.email.clone());
            }
            if next == old {
                return Ok(id);
            }
            conn.prepare_cached(
                "UPDATE people SET display_name = ?2, username = ?3, emails = ?4, updated_at = ?5
                 WHERE id = ?1",
            )?
            .execute(params![
                id.to_string(),
                next.display_name,
                next.username,
                emails_to_sql(&next.emails)?,
                at
            ])?;
            id
        }
        None => {
            let id = new_ulid();
            let emails: Vec<String> = Some(user.email.clone())
                .filter(|e| !e.is_empty())
                .into_iter()
                .collect();
            conn.prepare_cached(
                "INSERT INTO people (id, display_name, username, emails, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            )?
            .execute(params![
                id.to_string(),
                user.display_name,
                user.username,
                emails_to_sql(&emails)?,
                at
            ])?;
            conn.prepare_cached(
                "INSERT INTO person_identities (person_id, instance_id, account_id)
                 VALUES (?1, ?2, ?3)",
            )?
            .execute(params![id.to_string(), instance, user.id])?;
            id
        }
    };
    record(tx, id)?;
    Ok(id)
}

/// Each tracker user an issue names, as its Person, by tracker id: the
/// assignees and reporters of `issues`, made or brought up to date first.
pub(crate) fn resolve_users<'a>(
    tx: &mut WriteTx<'_>,
    instance: &str,
    users: impl IntoIterator<Item = &'a RemoteUser>,
    now: Timestamp,
) -> Result<std::collections::HashMap<String, PersonId>> {
    let mut ids = std::collections::HashMap::new();
    for user in users {
        if ids.contains_key(&user.id) {
            continue;
        }
        let id = upsert_remote(tx, instance, user, now)?;
        ids.insert(user.id.clone(), id);
    }
    Ok(ids)
}

/// A resolver over `ids` for todo_core::remote: a user it holds is their
/// Person, any other (none should be) their name.
pub(crate) fn resolver(
    ids: &std::collections::HashMap<String, PersonId>,
) -> impl Fn(&RemoteUser) -> PersonRef + '_ {
    move |u| match ids.get(&u.id) {
        Some(id) => PersonRef::Person(*id),
        None => todo_core::remote::by_name(u),
    }
}

fn record(tx: &mut WriteTx<'_>, id: PersonId) -> Result<()> {
    let person = load(tx.conn(), id)?;
    tx.record_change(
        Topic::Person,
        &id.to_string(),
        person.is_none(),
        person.map(|p| Resource::Person(Arc::new(p))),
    )?;
    Ok(())
}

/// One person, with their identities.
pub(crate) fn load(conn: &Connection, id: PersonId) -> Result<Option<Person>> {
    let row: Option<(String, String, String)> = conn
        .prepare_cached("SELECT display_name, username, emails FROM people WHERE id = ?1")?
        .query_row([id.to_string()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    let Some((display_name, username, emails)) = row else {
        return Ok(None);
    };
    let identities = conn
        .prepare_cached(
            "SELECT instance_id, account_id FROM person_identities WHERE person_id = ?1
             ORDER BY instance_id, account_id",
        )?
        .query_map([id.to_string()], |r| {
            Ok(Identity {
                instance_id: r.get(0)?,
                account_id: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(Person {
        id,
        display_name,
        username,
        emails: emails_from_sql(&emails)?,
        identities,
    }))
}

/// Everyone, by display name: two queries, whatever their number.
pub(crate) fn load_all(conn: &Connection) -> Result<Vec<Person>> {
    let mut identities: std::collections::HashMap<String, Vec<Identity>> =
        std::collections::HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT person_id, instance_id, account_id FROM person_identities
         ORDER BY instance_id, account_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(r) = rows.next()? {
        identities.entry(r.get(0)?).or_default().push(Identity {
            instance_id: r.get(1)?,
            account_id: r.get(2)?,
        });
    }
    let mut stmt = conn.prepare_cached(
        "SELECT id, display_name, username, emails FROM people
         ORDER BY display_name COLLATE NOCASE, id",
    )?;
    let mut rows = stmt.query([])?;
    let mut all = Vec::new();
    while let Some(r) = rows.next()? {
        let id: String = r.get(0)?;
        let emails: String = r.get(3)?;
        all.push(Person {
            id: person_id(&id)?,
            display_name: r.get(1)?,
            username: r.get(2)?,
            emails: emails_from_sql(&emails)?,
            identities: identities.remove(&id).unwrap_or_default(),
        });
    }
    Ok(all)
}

/// Whether `id` is a person here.
pub(crate) fn exists(conn: &Connection, id: PersonId) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM people WHERE id = ?1)")?
        .query_row([id.to_string()], |r| r.get(0))?)
}

pub(crate) fn person_id(s: &str) -> Result<PersonId> {
    s.parse()
        .map_err(|_| StoreError::Corrupt(format!("person id {s:?}")))
}

pub(crate) fn emails_to_sql(emails: &[String]) -> Result<String> {
    serde_json::to_string(emails).map_err(|e| StoreError::Corrupt(e.to_string()))
}

pub(crate) fn emails_from_sql(s: &str) -> Result<Vec<String>> {
    serde_json::from_str(s).map_err(|e| StoreError::Corrupt(format!("emails {s:?}: {e}")))
}
