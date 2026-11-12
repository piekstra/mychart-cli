# Fixtures

Every person, record and identifier here is invented. None is a capture of
a real patient's data, and none may become one: real records never enter
this repository, its issues or its pull requests.

| File | What it is | Read by |
|---|---|---|
| `brands_bundle.json` | The structure of Epic's User-access Brands bundle (`https://open.epic.com/Endpoints/Brands`, 2026-11-12): a collection Bundle of brand `Organization`s with `identifier` (`https://open.epic.com/brand-identifier`), `address`, `partOf` for locations, and `Endpoint`s with `status`, `managingOrganization.display` and `address`. Organizations, places and URLs are invented (`Example Health`, `fhir.example.org`, postal code `00000`). | `src/orgs.rs` tests |
| `epic_metadata.json` | Epic's sandbox CapabilityStatement (`GET {base}/metadata` with `Epic-Client-ID`, 2026-11-12), cut to two resources with three search parameters each. The OAuth URIs point at `fhir.example.org`; the `register` URI is the one Epic adds for a client allowed dynamic registration. | `src/epic.rs` and `tests/fixture_shapes.rs` tests |
| `smart_configuration.json` | Epic's sandbox `.well-known/smart-configuration` (2026-11-12) with the host changed to `fhir.example.org`. No `registration_endpoint`, as Epic serves it. | `src/epic.rs` and `tests/fixture_shapes.rs` tests |
| `mock_records.json` | The mock organization's records (`tests/support/mock_epic.rs`): two invented patients, Pat and Sam Example, with a few US Core resources, a DocumentReference with a PDF and an HTML Binary, and a decimal written `13.50` to prove the stream keeps the server's text. | `tests/e2e.rs`, `tests/fixture_shapes.rs` |

## Rules

- Patients are named `Example`; ids follow `e<Word>000…<n>`; birth dates are
  `1990-01-01` or `2026-01-01`; no telephone, email or street address.
- Hosts are `example.org`/`example.com` or 127.0.0.1; ids of Epic's own
  public sandbox are allowed only where Epic publishes them.
- `tests/fixture_shapes.rs` checks these rules positively, and
  `tests/no_personal_data.rs` scans every tracked file for personal-data
  shapes.
