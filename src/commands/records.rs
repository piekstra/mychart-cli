//! `mychart records export`.

use std::io::Write;

use pk_cli_core::{output, CliError};
use serde_json::json;

use super::{rfc3339, transport, Ctx, BIN};
use crate::cli::{ExportArgs, RecordsCmd};
use crate::export;
use crate::fhir::{origin, Fhir};
use crate::orgs;
use crate::session::{now, Context, Live};
use crate::stream::{
    Header, RecordsWriter, SourceCli, StreamOrganization, StreamPatient, FHIR_VERSION,
    STREAM_SCHEMA,
};

pub fn run(ctx: &Ctx, cmd: &RecordsCmd) -> Result<(), CliError> {
    let RecordsCmd::Export(args) = cmd;
    export_cmd(ctx, args)
}

/// Whether a base URL is Epic's sandbox or a server on this machine: the
/// only places `-o` writes records to a loose file from.
pub fn synthetic(c: &Context) -> bool {
    c.organization == orgs::SANDBOX_ID
        || origin(&c.fhir_base).is_some_and(|o| {
            o.starts_with("http://127.0.0.1:") || o.starts_with("http://localhost:")
        })
}

fn validate(args: &ExportArgs) -> Result<(), CliError> {
    if let Some(d) = &args.since {
        pk_cli_core::dates::parse_iso(d)?;
    }
    if !args.ndjson && args.output.is_none() {
        return Err(CliError::Usage(
            "say where the records go: --ndjson for stdout (`| medrec import --from mychart`) or -o <DIR>".into(),
        ));
    }
    Ok(())
}

pub fn header(c: &Context, since: Option<&str>) -> Header {
    Header {
        schema: STREAM_SCHEMA.into(),
        source: SourceCli {
            cli: BIN.into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        organization: StreamOrganization {
            id: c.organization.clone(),
            name: c.organization_name.clone(),
            fhir_base: c.fhir_base.clone(),
            vendor: Some("epic".into()),
        },
        patient: StreamPatient {
            id: c.id.clone(),
            organization: Some(c.organization.clone()),
            label: c.label.clone(),
            relationship: c.relationship.clone(),
        },
        since: since.map(str::to_string),
        pulled_at: rfc3339(now()),
        fhir_version: Some(FHIR_VERSION.into()),
    }
}

fn export_cmd(ctx: &Ctx, args: &ExportArgs) -> Result<(), CliError> {
    validate(args)?;
    let c = ctx.signed_in(&args.who)?;
    if args.output.is_some() && !synthetic(&c) {
        return Err(CliError::Usage(format!(
            "-o writes records to a plain file, so it is for Epic's sandbox and test servers only; \
             patient {} is at {}",
            c.display(),
            c.organization_name
        ))
        .with_remedy(format!("pipe them into the encrypted vault: `{BIN} records export --patient {} --ndjson | medrec import --from mychart`", c.display())));
    }
    let t = transport()?;
    let live = Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c)?;
    let fhir = Fhir::new(&t, &c.fhir_base, &live);
    let note = |line: String| {
        if ctx.common.verbose {
            ctx.note(line)
        }
    };
    let since = args.since.as_deref();

    match &args.output {
        None => {
            let stdout = std::io::stdout();
            let mut w =
                RecordsWriter::new(std::io::BufWriter::new(stdout.lock()), &header(&c, since))?;
            let result = export::run(&fhir, &mut w, &c.id, since, &note);
            let counts: u64 = w.counts().values().sum();
            let trailer = w.finish(rfc3339(now()), result.as_ref().err())?;
            match result {
                Ok(()) => {
                    ctx.note(format!(
                        "exported {counts} resources for patient {} ({} skipped searches or files)",
                        c.display(),
                        trailer.skipped.len()
                    ));
                    Ok(())
                }
                Err(e) => {
                    // The stream on stdout is the one document; the error goes to stderr.
                    eprintln!("error: {e}");
                    std::process::exit(e.exit_code());
                }
            }
        }
        Some(dir) => {
            crate::fsutil::create_private_dir(dir)?;
            let path = dir.join("records.ndjson");
            let mut buf = Vec::new();
            let mut w = RecordsWriter::new(&mut buf, &header(&c, since))?;
            let result = export::run(&fhir, &mut w, &c.id, since, &note);
            let trailer = w.finish(rfc3339(now()), result.as_ref().err())?;
            crate::fsutil::write_private(&path, &buf)?;
            let payload = json!({
                "schema": "records-export/v1",
                "path": path.display().to_string(),
                "counts": trailer.counts,
                "skipped": trailer.skipped,
                "complete": trailer.complete,
            });
            if ctx.json() {
                output::json(&payload);
            } else {
                let n: u64 = trailer.counts.values().sum();
                let _ = writeln!(
                    std::io::stdout(),
                    "{} {n} resources to {} ({} skipped)",
                    if trailer.complete {
                        "wrote"
                    } else {
                        "wrote an incomplete stream of"
                    },
                    path.display(),
                    trailer.skipped.len()
                );
            }
            if let Err(e) = result {
                eprintln!("error: {e}");
                std::process::exit(e.exit_code());
            }
            Ok(())
        }
    }
}
