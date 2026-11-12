//! `mychart patients list`.

use pk_cli_core::{output, CliError};
use serde_json::{json, Value};

use super::auth::{patient_dto, status_row, stored, usable, Stored};
use super::Ctx;
use crate::cli::PatientsCmd;

pub fn run(ctx: &Ctx, cmd: &PatientsCmd) -> Result<(), CliError> {
    let PatientsCmd::List { status } = cmd;
    let mut items = Vec::new();
    for c in ctx.contexts()? {
        let v = if *status {
            let held = stored(ctx, &c)?;
            let mut v = status_row(&c, &held);
            if let Stored::Missing = held {
                v["status"] = json!({ "authenticated": false });
            }
            if let Some(row) = v.as_object_mut() {
                row.remove("credential_in_keychain");
            }
            v
        } else {
            patient_dto(&c, None)
        };
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
                row["usable"] = usable(item).into();
            }
        }
        output::table(&view);
    }
    Ok(())
}
