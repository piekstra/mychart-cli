# Security

Report a vulnerability privately through GitHub's "Report a vulnerability"
on this repository. Please do not open a public issue.

## What mychart protects

- **Tokens and keys** live only in the OS keychain, one item per patient
  (service `piekstra.mychart`). They are never printed, logged, written to
  a file, or passed in argv.
- **The per-install key**: 3072-bit RSA, made on the computer at sign-in,
  registered with the organization through Epic's dynamic client
  registration. Anyone holding the Keychain item can read that patient's
  records until the access period ends; `mychart auth logout` deletes it,
  and MyChart's "Manage My Linked Apps and Devices" revokes it at the
  organization.
- **The sign-in redirect** goes to `127.0.0.1` only, and its `state` and
  PKCE verifier are checked, so a code another program catches is useless.
- **Tokens go only to the organization's own server**: paths and next links
  on another host are refused.
- **Renewals** are serialized across processes by a refresh lock under
  `~/.config/mychart/`, so two processes never renew one sign-in at once.
- **Records** go to stdout (`--ndjson`) or, for Epic's sandbox and local
  test servers only, to a `0600` file. Saved documents are `0600`.

`mychart` is read-only: it makes no request that changes a record.
