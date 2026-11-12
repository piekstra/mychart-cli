//! `mychart patients list`.

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::auth::{patient_dto, stored};
use super::Ctx;
use crate::cli::PatientsCmd;

pub fn run(ctx: &Ctx, cmd: &PatientsCmd) -> Result<(), CliError> {
    let PatientsCmd::List { status } = cmd;
    let mut items = Vec::new();
    for c in ctx.contexts()? {
        let session = if *status { stored(ctx, &c)? } else { None };
        let mut v = patient_dto(&c, session.as_ref());
        if *status && session.is_none() {
            v["session"] = json!({ "authenticated": false });
        }
        items.push(v);
    }
    if ctx.json() {
        output::json(&json!({ "schema": "patient-list/v1", "items": items }));
    } else if items.is_empty() {
        eprintln!("no patient is signed in (`mychart auth login --org <ORG> --label <WHO>`)");
    } else {
        let mut view: Vec<Value> = output::table_view(
            &items,
            &["label", "id", "organization_name", "relationship"],
        );
        if *status {
            for (row, item) in view.iter_mut().zip(&items) {
                row["usable"] = item
                    .pointer("/session/authenticated")
                    .cloned()
                    .unwrap_or(false.into());
            }
        }
        output::table(&view);
    }
    Ok(())
}
