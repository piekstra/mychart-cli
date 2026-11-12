# mychart

Pull your own health records, and your children's through MyChart proxy
access, from any health system that runs Epic, through Epic's
patient-access SMART on FHIR API.

Unofficial; not affiliated with or endorsed by Epic Systems Corporation.
MyChart is Epic's trademark.

```sh
mychart orgs search "<health system>"                # Epic's directory, cached weekly
mychart auth login --org <ID> --label adult1         # sign in in the browser
mychart patients list
mychart records export --patient adult1 --ndjson | medrec import --from mychart
mychart documents list --patient adult1
mychart documents download <ID> -o note.html --patient adult1
mychart api GET Patient/<id> --patient adult1
```

`mychart` only reads. Records go where you send them; nothing is sent
anywhere else.

## What this app does with your records

`mychart` runs on your own computer. There is no mychart server, account
or company between you and your health system: each household registers
its own copy of the app with Epic.

- **What it reads.** The records your health system lets your MyChart
  account share with apps, over Epic's patient-access FHIR API: allergies,
  conditions, medications, immunizations, lab results and vital signs,
  procedures, encounters, care plans and goals, clinical notes, and the
  reports and scanned documents attached to them. For a child, only what
  your proxy access in MyChart allows. MyChart may let you choose what to
  share when you sign in.
- **What it changes.** Nothing. It never writes to your health system.
- **Where records go.** To the computer it runs on: printed to its output
  or saved to a file or folder you name. It sends them to no other server,
  sells and shares nothing, and has no analytics or tracking.
- **What it keeps.** Your sign-in (access tokens and a key made on your
  computer) in the macOS Keychain, and a list of who is signed in (patient
  ids and labels, no record contents) under `~/.config/mychart/`.
- **Ending access.** `mychart auth logout` deletes the stored sign-in.
  MyChart's "Manage My Linked Apps and Devices" ends the app's access at
  your health system. Access also ends on its own after the period you
  chose in MyChart.

The source code is public in this repository, and anyone can read what it
does.

## Before the first sign-in

Each household registers its own free patient app with Epic and gives
`mychart` the two client ids: [docs/registration.md](docs/registration.md).

```sh
mychart config set sandbox_client_id <Non-Production Client ID>
mychart config set client_id <Client ID>
```

## Install

```sh
curl -sSL https://github.com/piekstra/mychart-cli/releases/latest/download/mychart-aarch64-apple-darwin.tar.gz | tar xz
mv mychart ~/.local/bin/        # or anywhere on PATH
mychart self-update             # later releases
```

Release assets: `mychart-<target>.tar.gz` and `.sha256` for
`aarch64-apple-darwin`, `x86_64-apple-darwin` and
`x86_64-unknown-linux-gnu`. From source: `make install`.

## Commands

| Command | What it does |
|---|---|
| `orgs refresh\|list\|search` | Epic's directory of organizations (`org-list/v1`), downloaded at most weekly into `~/.cache/mychart` |
| `auth login --org <ORG> [--label <WHO>]` | Standalone patient launch in the browser (PKCE), then Epic's dynamic client registration so access lasts for the period chosen in MyChart |
| `auth status` / `auth logout` | Each sign-in's state (`auth-status/v1`); remove one or all |
| `patients list [--status]` | The patients signed in, each with `self` or `proxy` (`patient-list/v1`) |
| `records export --patient <ID> [--since D] --ndjson` | Every resource the organization serves, documents included, as `records-stream/v1` |
| `documents list\|download` | Notes, reports and scans (`documents/v1`) |
| `api GET <path>` | One FHIR read as the signed-in patient |
| `config path\|show\|set\|unset` | Client ids, redirect port, scope, directory URL |
| `info`, `completions`, `self-update` | Family standard |

Every command takes `--json`. Exit codes: 0 ok, 2 usage, 3 sign-in needed,
4 not found, 5 the organization or network failed.

## How sign-in works

1. `mychart` reads the organization's endpoints from its FHIR server and
   opens its MyChart sign-in page. A parent with proxy access picks the
   child there; each person is one sign-in.
2. MyChart redirects to `http://127.0.0.1:7468/callback`, where `mychart`
   listens.
3. `mychart` exchanges the code (PKCE), registers a key made on this
   computer with the organization (Epic's dynamic client registration), and
   from then on gets access tokens by signing with that key, until the
   access period chosen in MyChart ends.

Tokens and the key are one macOS Keychain item per patient
(`piekstra.mychart`, account `smart/<organization>/<patient>`). Nothing
secret is written anywhere else.

## Kaiser Permanente

Kaiser's regions are Epic organizations in the directory:
`mychart orgs search kaiser`.

## Docs

- [docs/registration.md](docs/registration.md): registering a household's app
- [docs/api.md](docs/api.md): Epic's behaviour and traps, with Epic's wording
- [AGENTS.md](AGENTS.md): working on the code

Licensed under MIT or Apache-2.0.
