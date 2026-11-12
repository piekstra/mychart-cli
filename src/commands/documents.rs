//! `mychart documents list|download` (documents/v1) over Epic's
//! `DocumentReference` and `Binary`.
//!
//! A document is one attachment: `<DocumentReference id>` for the first,
//! `<DocumentReference id>#<n>` for the n-th after it (a clinical note
//! comes as HTML and RTF, two attachments of one DocumentReference).

use std::path::{Path, PathBuf};

use base64::Engine as _;
use pk_cli_core::{output, CliError};
use pk_cli_documents::{
    fs_safe, verify_download, DownloadBatch, Paged, SavedDocument, SkippedDocument,
};
use serde_json::Value;

use super::{transport, Ctx};
use crate::cli::{DocumentsCmd, PatientArgs};
use crate::fhir::{Failure, Fhir};
use crate::session::Live;

pub fn run(ctx: &Ctx, cmd: &DocumentsCmd) -> Result<(), CliError> {
    match cmd {
        DocumentsCmd::List { who, range } => {
            range.validate()?;
            let docs = list(ctx, who, range)?;
            let mut page = Paged::new("document", docs.into_iter().map(|d| d.document).collect());
            page.total = Some(page.items.len() as u64);
            page.emit(ctx.json());
            Ok(())
        }
        DocumentsCmd::Download {
            id,
            all,
            output,
            who,
            range,
        } => {
            range.validate()?;
            if *all {
                download_all(ctx, who, range, output)
            } else {
                download_one(ctx, who, id.as_deref().unwrap_or_default(), output)
            }
        }
    }
}

/// A listed document and where its bytes are.
pub struct Listed {
    pub document: pk_cli_documents::Document,
    pub url: String,
    pub content_type: Option<String>,
}

fn text(v: &Value, pointers: &[&str]) -> Option<String> {
    pointers
        .iter()
        .find_map(|p| v.pointer(p).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The file extension for a MIME type, and the `verify_download` label.
pub fn extension(content_type: Option<&str>) -> (&'static str, Option<&'static str>) {
    let ct = content_type
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match ct.as_str() {
        "application/pdf" => ("pdf", Some("pdf")),
        "text/html" => ("html", Some("html")),
        "text/rtf" | "application/rtf" => ("rtf", Some("rtf")),
        "text/plain" => ("txt", Some("txt")),
        "application/xml" | "text/xml" | "application/hl7-v3+xml" => ("xml", Some("xml")),
        "image/jpeg" => ("jpg", None),
        "image/png" => ("png", None),
        "image/tiff" => ("tif", None),
        "image/gif" => ("gif", None),
        _ => ("bin", None),
    }
}

/// The documents of one DocumentReference.
pub fn documents_of(v: &Value) -> Vec<Listed> {
    let Some(id) = v["id"].as_str() else {
        return Vec::new();
    };
    let date = text(v, &["/date", "/context/period/start"])
        .map(|d| d.chars().take(10).collect::<String>());
    let name = text(v, &["/description", "/type/text", "/type/coding/0/display"]);
    let category = text(
        v,
        &[
            "/category/0/text",
            "/category/0/coding/0/display",
            "/category/0/coding/0/code",
        ],
    );
    v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let a = &c["attachment"];
            Some((a["url"].as_str()?.to_string(), a))
        })
        .enumerate()
        .map(|(n, (url, a))| {
            let content_type = a["contentType"].as_str().map(str::to_string);
            let (ext, _) = extension(content_type.as_deref());
            let doc_id = if n == 0 {
                id.to_string()
            } else {
                format!("{id}#{}", n + 1)
            };
            let mut d = pk_cli_documents::Document::new(
                doc_id,
                name.clone()
                    .or_else(|| a["title"].as_str().map(str::to_string))
                    .unwrap_or_else(|| "Document".into()),
            );
            d.date = date.clone();
            d.category = category.clone();
            d.file = Some(format!(
                "{}{}.{ext}",
                date.as_deref()
                    .map(|d| format!("{}_", fs_safe(d)))
                    .unwrap_or_default(),
                fs_safe(&d.id)
            ));
            Listed {
                document: d,
                url,
                content_type,
            }
        })
        .collect()
}

fn in_range(date: Option<&str>, range: &pk_cli_documents::RangeArgs) -> bool {
    let Some(d) = date else {
        return range.since.is_none() && range.until.is_none();
    };
    range.since.as_deref().is_none_or(|s| d >= s) && range.until.as_deref().is_none_or(|u| d <= u)
}

pub fn list(
    ctx: &Ctx,
    who: &PatientArgs,
    range: &pk_cli_documents::RangeArgs,
) -> Result<Vec<Listed>, CliError> {
    let c = ctx.signed_in(who)?;
    let t = transport()?;
    let live = Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c)?;
    let fhir = Fhir::new(&t, &live.fhir_base(), &live);
    list_in(ctx, &fhir, &c.id, range)
}

/// The patient's documents in `range`, newest first, through one sign-in.
fn list_in<T: pk_cli_core::transport::Transport>(
    ctx: &Ctx,
    fhir: &Fhir<'_, T>,
    patient: &str,
    range: &pk_cli_documents::RangeArgs,
) -> Result<Vec<Listed>, CliError> {
    let mut out: Vec<Listed> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for extra in [&[("category", "clinical-note")][..], &[][..]] {
        let mut params = vec![("patient".to_string(), patient.to_string())];
        params.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        let r = fhir.search_each("DocumentReference", &params, |r| {
            if seen.insert(r.id.clone()) {
                if let Some(v) = r.value() {
                    out.extend(documents_of(&v));
                }
            }
            Ok(())
        });
        match r {
            Ok(summary) if summary.malformed > 0 => ctx.note(format!(
                "warning: {} DocumentReference entries were not FHIR resources",
                summary.malformed
            )),
            Ok(_) => {}
            Err(Failure::Skip { code, reason }) => ctx.note(format!(
                "warning: DocumentReference search {extra:?} refused ({code}: {reason})"
            )),
            Err(f) => return Err(f.into_error()),
        }
    }
    out.retain(|d| in_range(d.document.date.as_deref(), range));
    out.sort_by(|a, b| {
        b.document
            .date
            .cmp(&a.document.date)
            .then(a.document.id.cmp(&b.document.id))
    });
    if let Some(n) = range.limit {
        out.truncate(n as usize);
    }
    Ok(out)
}

/// Fetch one document's bytes and check they are what its type says.
fn fetch<T: pk_cli_core::transport::Transport>(
    fhir: &Fhir<'_, T>,
    d: &Listed,
) -> Result<Vec<u8>, Failure> {
    let b = fhir.binary(&d.url)?;
    let v = b.value().unwrap_or_default();
    let Some(data) = v["data"].as_str() else {
        return Err(Failure::Skip {
            code: "no_file",
            reason: "the Binary carries no data".into(),
        });
    };
    let compact: String = data.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(compact.as_bytes())
        .map_err(|e| Failure::Skip {
            code: "verify_failed",
            reason: format!("the Binary's data is not base64: {e}"),
        })?;
    let ct = v["contentType"].as_str().or(d.content_type.as_deref());
    match extension(ct).1 {
        Some(label) => verify_download(&bytes, Some(label)),
        None if bytes.is_empty() => Err(CliError::Upstream("the document is empty".into())),
        None => Ok(()),
    }
    .map_err(|e| Failure::Skip {
        code: "verify_failed",
        reason: e.message().to_string(),
    })?;
    Ok(bytes)
}

fn target(output: &Path, d: &Listed) -> PathBuf {
    if output.is_dir() {
        output.join(d.document.file.as_deref().unwrap_or("document.bin"))
    } else {
        output.to_path_buf()
    }
}

fn download_one(ctx: &Ctx, who: &PatientArgs, id: &str, output: &Path) -> Result<(), CliError> {
    let id = id.strip_suffix("#1").unwrap_or(id);
    let c = ctx.signed_in(who)?;
    let t = transport()?;
    let live = Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c)?;
    let fhir = Fhir::new(&t, &live.fhir_base(), &live);
    let docs = list_in(ctx, &fhir, &c.id, &pk_cli_documents::RangeArgs::default())?;
    let Some(d) = docs.iter().find(|d| d.document.id == id) else {
        return Err(
            CliError::NotFound(format!("no document `{id}` for this patient"))
                .with_remedy("list them with `mychart documents list`"),
        );
    };
    let bytes = fetch(&fhir, d).map_err(Failure::into_error)?;
    let path = target(output, d);
    crate::fsutil::write_private(&path, &bytes)?;
    let saved =
        SavedDocument::from_document(&d.document, path.display().to_string(), bytes.len() as u64);
    pk_cli_documents::emit(&saved, ctx.json());
    Ok(())
}

fn download_all(
    ctx: &Ctx,
    who: &PatientArgs,
    range: &pk_cli_documents::RangeArgs,
    dir: &Path,
) -> Result<(), CliError> {
    if dir.exists() && !dir.is_dir() {
        return Err(CliError::Usage(format!(
            "--all saves into a directory; {} is a file",
            dir.display()
        )));
    }
    let c = ctx.signed_in(who)?;
    let t = transport()?;
    let live = Live::start(&t, ctx.vault.as_ref(), &ctx.lock, &c)?;
    let fhir = Fhir::new(&t, &live.fhir_base(), &live);
    let docs = list_in(ctx, &fhir, &c.id, range)?;
    crate::fsutil::create_private_dir(dir)?;
    let (mut saved, mut skipped) = (Vec::new(), Vec::new());
    for d in &docs {
        match fetch(&fhir, d) {
            Ok(bytes) => {
                let path = target(dir, d);
                crate::fsutil::write_private(&path, &bytes)?;
                saved.push(SavedDocument::from_document(
                    &d.document,
                    path.display().to_string(),
                    bytes.len() as u64,
                ));
            }
            Err(Failure::Skip { code, reason }) => {
                skipped.push(SkippedDocument::new(d.document.id.clone(), reason).with_code(code))
            }
            Err(f) => return Err(f.into_error()),
        }
    }
    let batch = DownloadBatch::new(dir.display().to_string(), saved).with_skipped(skipped);
    if ctx.json() {
        output::json(&serde_json::to_value(&batch).unwrap_or_default());
    } else {
        println!(
            "saved {} documents to {} ({} skipped)",
            batch.count,
            batch.dir,
            batch.skipped.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_attachment_is_a_document_named_safely() {
        let v: Value = serde_json::json!({
            "resourceType": "DocumentReference", "id": "doc-1", "date": "2026-01-06T10:00:00Z",
            "type": {"text": "Progress note"},
            "category": [{"coding": [{"code": "clinical-note"}]}],
            "content": [
                {"attachment": {"contentType": "text/html", "url": "Binary/b1"}},
                {"attachment": {"contentType": "text/rtf", "url": "Binary/b2"}},
                {"attachment": {"contentType": "text/plain"}}
            ]
        });
        let docs = documents_of(&v);
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0].document.id, "doc-1");
        assert_eq!(docs[1].document.id, "doc-1#2");
        assert_eq!(
            docs[0].document.file.as_deref(),
            Some("2026-01-06_doc-1.html")
        );
        assert_eq!(
            docs[1].document.file.as_deref(),
            Some("2026-01-06_doc-1_2.rtf")
        );
        assert_eq!(docs[0].document.category.as_deref(), Some("clinical-note"));
        assert_eq!(docs[0].document.name, "Progress note");
    }
}
