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
    Header, RecordsWriter, SourceCli, StreamOrganization, StreamPatient, Trailer, FHIR_VERSION,
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

fn validate(args: &ExportArgs, json: bool) -> Result<(), CliError> {
    if args.ndjson && json {
        return Err(CliError::Usage(
            "--ndjson is the output; leave out --json (it is for -o <DIR>, which prints records-export/v1)".into(),
        ));
    }
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
            kind: Some("patient".into()),
            fhir_user: c.fhir_user.clone(),
        },
        since: since.map(str::to_string),
        pulled_at: rfc3339(now()),
        fhir_version: FHIR_VERSION.into(),
    }
}

/// `records-export/v1`: `{schema, path, bytes, complete, counts, skipped?}`.
pub fn export_payload(path: &std::path::Path, bytes: u64, trailer: &Trailer) -> serde_json::Value {
    let mut payload = json!({
        "schema": "records-export/v1",
        "path": path.display().to_string(),
        "bytes": bytes,
        "complete": trailer.complete,
        "counts": trailer.counts,
    });
    if !trailer.skipped.is_empty() {
        payload["skipped"] = json!(trailer.skipped);
    }
    payload
}

fn export_cmd(ctx: &Ctx, args: &ExportArgs) -> Result<(), CliError> {
    validate(args, ctx.json())?;
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
    let tell = |line: String| ctx.note(line);
    let fhir = Fhir::new(&t, &live.fhir_base(), &live).with_note(&tell);
    let since = args.since.as_deref();
    let mut head = header(&c, since);
    head.organization.fhir_base = fhir.base().to_string();
    let note = |line: String| {
        if ctx.common.verbose {
            ctx.note(line)
        }
    };

    match &args.output {
        None => {
            let stdout = std::io::stdout();
            let mut w = RecordsWriter::new(std::io::BufWriter::new(stdout.lock()), &head)?;
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
            let mut w = RecordsWriter::new(&mut buf, &head)?;
            let result = export::run(&fhir, &mut w, &c.id, since, &note);
            let trailer = w.finish(rfc3339(now()), result.as_ref().err())?;
            crate::fsutil::write_private(&path, &buf)?;
            let payload = export_payload(&path, buf.len() as u64, &trailer);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PatientArgs;
    use crate::stream::{Skip, END_SCHEMA};

    fn args(ndjson: bool, output: Option<&str>) -> ExportArgs {
        ExportArgs {
            who: PatientArgs::default(),
            since: None,
            ndjson,
            output: output.map(Into::into),
        }
    }

    #[test]
    fn the_stream_is_the_output_so_json_is_refused_with_it() {
        let e = validate(&args(true, None), true).unwrap_err();
        assert_eq!(e.exit_code(), 2);
        assert!(validate(&args(true, None), false).is_ok());
        assert!(validate(&args(false, Some("out")), true).is_ok());
        assert_eq!(
            validate(&args(false, None), false).unwrap_err().exit_code(),
            2
        );
    }

    #[test]
    fn the_export_document_carries_bytes_and_skips_only_when_there_are_any() {
        let mut trailer = Trailer {
            schema: END_SCHEMA.into(),
            complete: true,
            counts: [("Patient".to_string(), 1u64)].into_iter().collect(),
            skipped: Vec::new(),
            finished_at: Some("2026-01-01T00:00:00Z".into()),
            error: None,
        };
        let path = std::path::Path::new("/tmp/out/records.ndjson");
        let v = export_payload(path, 1234, &trailer);
        assert_eq!(v["schema"], "records-export/v1");
        assert_eq!(v["path"], "/tmp/out/records.ndjson");
        assert_eq!(v["bytes"], 1234);
        assert_eq!(v["complete"], true);
        assert_eq!(v["counts"]["Patient"], 1);
        assert!(v.get("skipped").is_none(), "{v}");

        trailer.skipped.push(Skip {
            resource_type: Some("Media".into()),
            id: None,
            search: Some("Media".into()),
            reason: "HTTP 403".into(),
            code: "forbidden".into(),
        });
        let v = export_payload(path, 1234, &trailer);
        assert_eq!(v["skipped"][0]["code"], "forbidden");
    }
}
