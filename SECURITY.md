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
  A request to the redirect without this sign-in's `state` (an old tab, a
  web page, another program) is answered 400 and ignored; it cannot end the
  sign-in, and its text never reaches the terminal.
- **Tokens go only to the organization's own server**, over https (http
  only to this machine): the origin is the one the Keychain's sign-in was
  made at, not the plain-file patient index, and paths, next links and
  attachment URLs on another host are refused or skipped without the token.
- **Files** are written to a new temp file with a random name, created
  exclusively (`O_EXCL`), and renamed over the target, so another account
  cannot plant a file or a symbolic link that receives the records.
- **Renewals** are serialized across processes by a refresh lock under
  `~/.config/mychart/`, so two processes never renew one sign-in at once.
- **Records** go to stdout (`--ndjson`) or, for Epic's sandbox and local
  test servers only, to a `0600` file. Saved documents are `0600`.

`mychart` is read-only: it makes no request that changes a record.
