//! `mychart orgs refresh|list|search`.

use pk_cli_core::{output, CliError};
use pk_cli_http::BlockingTransport;
use serde_json::{json, Value};

use super::{rfc3339, Ctx};
use crate::cli::{OrgListArgs, OrgsCmd};
use crate::orgs::{self, Freshness, Org, OrgIndex};
use crate::session::now;

pub fn run(ctx: &Ctx, cmd: &OrgsCmd) -> Result<(), CliError> {
    match cmd {
        OrgsCmd::Refresh { force } => refresh(ctx, *force),
        OrgsCmd::List(list) => {
            validate(list)?;
            let index = index(ctx, false)?;
            let all = index.all();
            let rows: Vec<(&Org, Option<&str>)> = all.iter().map(|o| (o, None)).collect();
            emit(ctx, rows, list);
            Ok(())
        }
        OrgsCmd::Search { text, list } => {
            validate(list)?;
            if text.trim().is_empty() {
                return Err(CliError::Usage("search text is empty".into()));
            }
            let index = index(ctx, false)?;
            let all = index.all();
            let hits = orgs::search(&all, text);
            if hits.is_empty() {
                return Err(CliError::NotFound(format!(
                    "no organization in Epic's directory matches `{text}`"
                )));
            }
            emit(ctx, hits, list);
            Ok(())
        }
    }
}

fn validate(list: &OrgListArgs) -> Result<(), CliError> {
    if let Some(st) = &list.state {
        if st.len() != 2 || !st.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(CliError::Usage(format!("`{st}` is not a two-letter state")));
        }
    }
    if list.limit == Some(0) {
        return Err(CliError::Usage("--limit must be at least 1".into()));
    }
    Ok(())
}

/// The download needs a longer timeout than the family's 60 s; the
/// directory is public, so the client carries no credential.
pub fn directory_transport() -> Result<BlockingTransport, CliError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(orgs::DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| CliError::Other(format!("failed to build HTTP client: {e}")))?;
    Ok(BlockingTransport::with_client(client))
}

pub fn directory_url(ctx: &Ctx) -> Result<String, CliError> {
    Ok(ctx
        .config()?
        .directory_url
        .unwrap_or_else(|| orgs::DEFAULT_DIRECTORY_URL.to_string()))
}

/// The index, downloading Epic's directory first when the cache is missing
/// or a week old.
pub fn index(ctx: &Ctx, force: bool) -> Result<OrgIndex, CliError> {
    let url = directory_url(ctx)?;
    let had = orgs::load_index(&ctx.cache_dir)?.is_some();
    if force || !had {
        ctx.note("downloading Epic's organization directory (about 100 MB; once a week)");
    }
    let (index, freshness) =
        orgs::ensure(&directory_transport()?, &ctx.cache_dir, &url, now(), force)?;
    match freshness {
        Freshness::Refreshed if had && !force => {
            ctx.note("refreshed Epic's organization directory (the cached copy was a week old)")
        }
        Freshness::Stale(e) => ctx.note(format!(
            "warning: could not refresh Epic's directory ({e}); using the copy from {}",
            rfc3339(index.fetched_at)
        )),
        _ => {}
    }
    Ok(index)
}

fn refresh(ctx: &Ctx, force: bool) -> Result<(), CliError> {
    let before = orgs::load_index(&ctx.cache_dir)?.map(|i| i.fetched_at);
    let index = index(ctx, force)?;
    let refreshed = before != Some(index.fetched_at);
    let payload = json!({
        "source_url": index.source_url,
        "fetched_at": rfc3339(index.fetched_at),
        "bundle_timestamp": index.bundle_timestamp,
        "organizations": index.orgs.len(),
        "locations": index.orgs.iter().map(|o| o.locations.len()).sum::<usize>(),
        "refreshed": refreshed,
        "next_refresh_after": rfc3339(index.fetched_at + orgs::REFRESH_EVERY_SECS),
        "cache": orgs::index_path(&ctx.cache_dir).display().to_string(),
    });
    output::emit(ctx.json(), "org-refresh", payload, |p| {
        println!(
            "{} organizations{} (directory of {}); next refresh after {}",
            p["organizations"],
            if refreshed {
                ", downloaded now"
            } else {
                ", cached copy is current"
            },
            p["bundle_timestamp"].as_str().unwrap_or("unknown date"),
            p["next_refresh_after"].as_str().unwrap_or_default()
        );
    });
    Ok(())
}

fn emit(ctx: &Ctx, rows: Vec<(&Org, Option<&str>)>, list: &OrgListArgs) {
    let items: Vec<Value> = rows
        .into_iter()
        .filter(|(o, _)| {
            list.state
                .as_ref()
                .is_none_or(|st| o.states.iter().any(|s| s.eq_ignore_ascii_case(st)))
        })
        .take(list.limit.unwrap_or(usize::MAX))
        .map(|(o, matched)| o.dto(matched))
        .collect();
    if ctx.json() {
        output::json(&json!({ "schema": "org-list/v1", "items": items }));
    } else {
        output::table(&output::table_view(
            &items,
            &["id", "name", "location", "matched", "fhir_base"],
        ));
    }
}
