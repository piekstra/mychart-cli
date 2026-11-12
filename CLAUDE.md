# CLAUDE.md

See **[AGENTS.md](AGENTS.md)**, the canonical guide for this repo. Claude-specific notes:

- Never read, print or summarize a real record, document or patient name.
  Use the mock (`make mock`) or Epic's sandbox; against real data, report
  counts, statuses and exit codes only.
- A sign-in is the owner's to do (MyChart, MFA). Record it as a `twapp
  action` with the exact command; do not drive MyChart.
- If a `mychart` call hangs, it is waiting on a Keychain approval. Stop
  calling it; a retry only raises another dialog.
- Read `docs/api.md` before changing sign-in or export, and update it in the
  same change.
