//! `mychart api GET <path>`: one FHIR read as the signed-in patient, the
//! body printed as the server sent it.

use pk_cli_core::CliError;

use super::{transport, Ctx};
use crate::cli::ApiArgs;
use crate::fhir::{status_failure, Fhir};
use crate::session::Live;

pub fn run(ctx: &Ctx, args: &ApiArgs) -> Result<(), CliError> {
    if !args.method.eq_ignore_ascii_case("GET") {
        return Err(CliError::Usage(format!(
            "`{}`: mychart only reads, so `api` takes GET",
            args.method
        )));
    }
    if args.path.trim().is_empty() {
        return Err(CliError::Usage(
            "the path is empty (for example `Patient/<id>`)".into(),
        ));
    }
    let c = ctx.signed_in(&args.who)?;
    let t = transport()?;
    let live = Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c)?;
    let fhir = Fhir::new(&t, &live.fhir_base(), &live);
    fhir.url(&args.path)?;
    let resp = fhir.get(&args.path, "application/fhir+json")?;
    if !resp.is_success() {
        return Err(status_failure(&resp).into_error());
    }
    println!("{}", resp.body.trim_end());
    Ok(())
}
