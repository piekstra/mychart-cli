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
| `src/fhir.rs` | the read client: reads, paged searches, `Binary`, status to skip or error |
| `src/export.rs` | Epic's search plan and the export order |
| `src/stream.rs` | the `records-stream/v1` writer |
| `tests/support/mock_epic.rs` | a mock Epic organization on 127.0.0.1 (also `make mock`) |
| `docs/api.md` | Epic's behaviour and traps, quoted; read before changing auth or export |

`src/smart.rs`, `src/fhir.rs` and `src/stream.rs` stand in for
medical-common's `pk-med-smart`, `pk-med-fhir` and `pk-med-records`; each
goes when its crate is tagged and pinned, with no change to the stream.

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
- A token goes only to the organization's own origin: absolute paths and
  next links are checked (`fhir::origin`).
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
