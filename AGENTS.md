# AGENTS.md — mychart-cli

`mychart` reads a household member's own records from any Epic
organization through Epic's patient-access SMART on FHIR API. It conforms to
piekstra-cli/1 and the `health-records/v1` and `documents/v1` profiles, on
the `pk-cli-*` crates from [cli-common](https://github.com/piekstra/cli-common).

## Layout

| File | Owns |
|---|---|
| `src/main.rs` | parsing, the `Ctx`, dispatch, `info`, exit codes |
| `src/cli.rs` | the clap tree |
| `src/commands/*.rs` | one module per command group; `Ctx` (config, vault, lock, cache, browser) in `mod.rs` |
| `src/orgs.rs` | Epic's Brands bundle: download, weekly cache, index, `--org` resolution, search |
| `src/oauth.rs` | PKCE, the loopback listener, opening the browser |
| `src/smart.rs` | discovery with `Epic-Client-ID`, authorize URL, code exchange, dynamic client registration, JWT-bearer and refresh grants |
| `src/key.rs` | the per-install RSA key and its RS384 client assertion |
| `src/session.rs` | the stored sign-in (`Session`), the patient index (`contexts.json`), `pick`, renewal under the refresh lock (`access_token`, `Live`) |
| `src/vault.rs` | the Keychain behind a trait, so tests use memory |
| `src/fhir.rs` | the request layer: reads, paged searches, `Binary`, retries, status to skip or error |
| `src/export.rs` | Epic's search plan, the export order, and the types `--since` leaves unbounded |
| `tests/support/mock_epic.rs` | a mock Epic organization on 127.0.0.1 (also `make mock`) |
| `docs/api.md` | Epic's behaviour and traps, quoted; read before changing auth or export |

The stream (`RecordsWriter`, `Header`, `Skip`, the trailer), the DTOs,
`records-export/v1` and the export file are medical-common's
`pk-med-records`; `RawResource`, `normalize_base` and `describe_json_error`
are its `pk-med-fhir` (v0.1.0, pinned with cli-common v0.31.0, the tag it
builds on). `src/smart.rs` and the request layer in `src/fhir.rs` stand in
for `pk-med-smart` and `pk_med_fhir::FhirClient` until medical-common
v0.2.0, which adds what an Epic export needs from the client: a token
source that renews mid-export, a network failure told from a 5xx, retry
notes and pages read around an unreadable entry. Move every `pk-cli-*` and
`pk-med-*` together, so the build holds one copy of each crate.

## Medical data

Medical records are the most sensitive data a household has.

- The repository carries no personal data: invented fixtures only
  (`tests/fixtures/README.md`), checked by `tests/no_personal_data.rs` and
  `tests/fixture_shapes.rs`.
- Develop and dogfood against the mock or Epic's sandbox (synthetic
  patients). A check against real data reports counts, statuses and exit
  codes only, or is left to the owner in their own terminal. An agent never
  reads, prints or summarizes a real record, document or patient name.
- `records export -o <DIR>` writes a plain file and refuses any organization
  but the sandbox and 127.0.0.1; real records go into medrec's encrypted
  vault through `--ndjson`.
- No MCP surface: mychart is not registered in cli-mcp and ships no `mcp`
  command, because a tool call would hand records to the model making it
  (the effort's plan, §8, until the owner decides otherwise).

## Credentials

- One Keychain item per (organization, patient): service
  `piekstra.mychart`, account `smart/<organization>/<patient>`, JSON holding
  the access token, the dynamic client id and its private key. Never
  printed, logged, or put in argv.
- Renewals run under `pk_cli_auth::refresh_lock::RefreshLock`, so an MCP
  server and a terminal never renew one sign-in at once.
- Tests and `make verify` never read the real Keychain:
  `MYCHART_KEYCHAIN_SERVICE` names an empty service for the surface tests and
  `smoke`; the in-process tests use `MemoryVault`.
- If a `mychart` call hangs, it waits on a Keychain approval. Stop calling it
  and tell the owner; a retry only raises another dialog.

## Conventions

- `--json` on every command, one schema-tagged document on stdout. `records
  export --ndjson` writes the stream instead, so `--json` with it is a usage
  error (exit 2, health-records/v1 §1.1); a failure after the header ends the
  stream with an incomplete trailer and the exit code.
- The DTOs are `pk-med-records`' (medical-common `DESIGN.md` §1): a
  `PatientContext`'s `session` is the Keychain account, never its content,
  and `records-export/v1` carries `skipped` only when something was skipped.
- Arguments are checked before the Keychain or the network.
- Read-only: `api` takes GET and nothing else.
- A token goes only to the organization's own origin, the one the Keychain
  session records (`Live::fhir_base`), never the index's: absolute paths,
  next links and attachment URLs are checked (`fhir::origin`).
- An export stops only when the sign-in, the network or the output fails.
  Anything else (a refusal, an answer that is not FHIR, a file on another
  server) is a skip, and a Binary is fetched once, right after the first
  resource that names it.
- Every binary-producing `make` target signs with the stable
  `pk-cli-codesign` identity.

## Tests

`make verify` (fmt, clippy, tests, smoke), offline:

- `tests/e2e.rs`: sign-in, renewal, proxy, logout, export, documents and
  `api` in-process against the mock.
- `tests/cli_surface.rs`: the binary's help, exit codes, JSON errors, `info`,
  completions, the directory against the mock.
- `tests/fixture_shapes.rs`, `tests/no_personal_data.rs`.

## Definition of done

`make verify` and CI green, the installed binary dogfooded against the mock
and Epic's sandbox (orgs, login, patients, export into a synthetic medrec
vault, documents, `api`, the error paths), `docs/api.md` matching what Epic
does, and no personal data anywhere in the diff.
