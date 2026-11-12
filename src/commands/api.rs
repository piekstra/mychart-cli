//! `mychart api GET <path>`: one FHIR read as the signed-in patient, the
//! body printed as the server sent it.

use pk_cli_core::CliError;

use super::{transport, Ctx};
use crate::cli::ApiArgs;
use crate::fhir::{origin, Fhir};

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
    let path = args.path.trim();
    let c = ctx.signed_in(&args.who)?;
    // Refused before the Keychain is read: the sign-in goes only to the
    // server it was made at.
    if path.contains("://") && (origin(path).is_none() || origin(path) != origin(&c.fhir_base)) {
        return Err(CliError::Usage(format!(
            "`{path}` is not on this organization's server ({}); the sign-in is sent only there",
            c.fhir_base
        )));
    }
    let t = transport()?;
    let live = ctx.live(&t, &c)?;
    let fhir = Fhir::new(&t, &live)?;
    let body = fhir.get_raw(path.trim_start_matches('/'))?;
    println!("{}", body.trim_end());
    Ok(())
}
