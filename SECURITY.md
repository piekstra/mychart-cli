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
  web page, another program) is answered and ignored; it cannot end the
  sign-in. One that carries an OAuth error and no `state` may be the
  organization's own refusal, so its error is named on stderr, cut to 200
  characters with every control character replaced.
- **Tokens go only to the organization's own server**, over https (http
  only to this machine): the origin is the one the Keychain's sign-in was
  made at, not the plain-file patient index, and paths, next links and
  attachment URLs on another host are refused or skipped without the token.
- **Files** are created new under a temporary name (`O_EXCL`, `0600`, which
  never follows a symbolic link), synced, and renamed over the target, so a
  file or symbolic link another account plants never receives the records;
  at worst the write fails. `records export -o` streams into its temporary
  file and renames it only once its trailer is written and synced: a
  complete stream replaces `records.ndjson`, one cut short becomes
  `records.incomplete.ndjson` and leaves the last complete export whole, and
  a symbolic link at `records.ndjson` is refused (exit 2).
- **Renewals** are serialized across processes by a refresh lock under
  `~/.config/mychart/`, so two processes never renew one sign-in at once.
- **Records** go to stdout (`--ndjson`) or, for Epic's sandbox and local
  test servers only, to a `0600` file. Saved documents are `0600`.

`mychart` is read-only: it makes no request that changes a record.
