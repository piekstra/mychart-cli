# Epic's patient-access API: what mychart relies on

Read from Epic's documentation on 2026-11-12 (fhir.epic.com renders with
JavaScript; it was read in a real browser) and checked against Epic's
public sandbox. Quotations are Epic's wording. Sources:

| Short name | Page |
|---|---|
| oauth2 | <https://fhir.epic.com/Documentation?docId=oauth2> |
| tutorial | <https://fhir.epic.com/Documentation?docId=oauth2tutorial> |
| patient apps | <https://fhir.epic.com/Documentation?docId=patientfacingfhirapps> |
| request process | <https://fhir.epic.com/Documentation?docId=epiconfhirrequestprocess> |
| guidelines | <https://fhir.epic.com/Documentation?docId=developerguidelines> |
| patient auth | <https://open.epic.com/Tutorial/PatientAuthentication> |
| endpoints | <https://open.epic.com/MyApps/Endpoints> |

## 1. Registering a patient-facing app

The form ("request process"): "The app name / The app consumer type /
Applicable APIs / The default FHIR version / Applicable redirect URIs /
Whether you'll use the confidential client profile (required for refresh
tokens)". [registration.md](registration.md) has the values mychart needs.

- **Audience**: Patients ("For patients and their caretakers authenticating
  with MyChart").
- **APIs**: only those on Epic's USCDI v3 list keep automatic distribution
  (section 3). Selecting `ImagingStudy`, `ExplanationOfBenefit` or any other
  API outside it ends that for every organization.
- **FHIR version**: R4. `fhirUser` in the id token needs it: "The fhirUser
  claim will only be present if app has the R4 (or greater) SMART on FHIR
  version selected, and requests both the openid and fhirUser scopes"
  (oauth2).
- **Public, not confidential.** "If your application launches without SSO,
  and specifically does not include a web server component at all, then you
  can keep this unchecked" (tutorial). A public client registers no
  credential: "you will not register credentials (public keys or client
  secrets) when building your app and will configure your library to use
  client authentication type "none."" (tutorial). So no JWK Set URL.
- **Redirect URIs**: "An https protocol is required for use in a production
  environment, but http protocol-based redirect_uris are allowed for
  development and testing" (oauth2), and "apps should not include
  localhost/loopback URIs in this list. An exception is for Native
  applications which are designed to use localhost for the redirect"
  (oauth2, "Limit Use of Localhost URIs"). mychart is such a native app. Its
  redirect is `http://127.0.0.1:7468/callback`: the loopback listener
  (cli-common's `pk-cli-auth`) binds and names only the IP literal (RFC
  8252 §7.3). [registration.md](registration.md) also lists the
  `http://localhost:7468/callback` spelling, which mychart does not send.
  Epic compares redirect URIs exactly, so the port is fixed (`mychart
  config set redirect_port`), never chosen at random. mychart 0.1 had a
  `redirect_host` setting; 0.2 refuses `config set redirect_host` (exit 2),
  and `auth login` warns while config.json still holds a host other than
  127.0.0.1, since an app registered with only that host's URI does not
  match.
  **Not settled by the docs:** whether a production organization accepts an
  http loopback URI. Redirect URIs stay editable after Ready for Production
  ("With the exception of adding redirect URIs and modifying your JWK Set
  URLs, all technical changes will require you to build a new app"), so an
  https page that forwards `code` and `state` to the loopback can be added
  later if one does not.
- **After Ready for Production** nothing else changes, and an app "cannot be
  deactivated" once deployed (request process).

## 2. Refresh tokens, dynamic client registration, lifetimes

A public client gets no refresh token: "If you are using a client-side OAuth
2.0 library, you cannot use the "refresh_token" grant type for persistent
access to APIs (>1 hour of access)… If you cannot add a server component to
your application, you can add dynamic client registration to your
client-side only application and follow the directions here to use your
initial access token to get persistent access. This app is only available
for Patient-facing applications at this time." (tutorial)

What mychart does (oauth2, "Dynamic Client Registration"):

1. Standalone launch with PKCE, as a public client, scope `openid
   fhirUser`, with a fresh OpenID `nonce`. Epic's ID token carries it back:
   "The nonce claim will be present only if it was specified in the
   authorization request." pk-med-smart refuses an ID token without it, or
   with another (exit 3): nothing is stored and no client is registered.
   The access token the launch returns is the RFC
   7591 initial access token: "This token is intentionally one-time-use
   and should be used for registration immediately."
2. "Generate a public-private key pair on the user's phone or computer" and
   "Securely store that device-specific key pair": a 3072-bit RSA key, kept
   in the Keychain item with the tokens.
3. `POST` the `register` endpoint with `Authorization: Bearer <initial
   token>` and `{"software_id": <the app's client id>, "jwks": {"keys":
   [<public RSA JWK>]}}`. "software_id: This parameter must contain the
   application's client ID issued by Epic." The answer carries the dynamic
   `client_id`. Only RSA keys go inline: Epic accepts EC "only… for JWKS
   returned from a JWKS URL". pk-med-smart's body also carries RFC 7591's
   `token_endpoint_auth_method: "none"`, `grant_types: [<the JWT-bearer
   grant>]`, `response_types: []` and `redirect_uris: []`, which describe
   that client. **Not settled by the docs:** whether Epic ignores or refuses
   those members; Epic's example sends only the two above. The sandbox
   sign-in settles it.
4. Every access token after that comes from the JWT-bearer grant (RFC 7523),
   which Epic recommends over a second launch for a refresh token:
   `grant_type=urn:ietf:params:oauth:grant-type:jwt-bearer`,
   `assertion=<JWT>`, `client_id=<dynamic id>`. The JWT's `iss` and `sub`
   are the dynamic client id, `aud` is the token endpoint, `exp` at most 5
   minutes out (mychart: 4); mychart signs RS384. No `scope` goes with it:
   Epic's example sends none, and a scope Epic did not grant could fail the
   grant.

mychart stores the sign-in, its registered client included, before the
first JWT-bearer grant. A first grant that fails leaves the registered
client in place, so the next command asks again without a new sign-in or
a second registration. A refused one shows in `auth status` until then
(below).

Lifetimes:

| What | Lifetime | Epic's wording |
|---|---|---|
| Access token | about an hour (`expires_in` 3240 to 3600 in Epic's examples) | "the number of seconds for which the access token is valid" |
| id token | 5 minutes | "This is set to the current time plus 5 minutes." |
| Dynamic client | the period the patient chose | "For the duration the user selected when approving your app… your app can get an access token whenever it needs using the JWT bearer authorization grant" |
| That period | chosen in MyChart from the organization's options, 1 hour to 1 year on Epic's screenshots; indefinite only if the organization configures it | "In patient-facing contexts, it can be set by the patient through MyChart"; "Note that this only gives patients the option of indefinite access. Each patient still chooses their max access period." |

`mychart auth status` shows each sign-in's `renewable` flag and token
expiry; when the period ends the grant fails and mychart asks for a new
`auth login` (exit 3). A token answer without `expires_in` is taken to last
an hour, as mychart 0.1 took it (Epic's answers state it), so `expires_at`
in `auth login` and `auth status` is always an RFC 3339 time and such a
token is renewed before it lapses rather than after a 401.

A server can refuse an access token before its `expires_in` is up (a 401:
revoked, or forgotten after a restart). mychart then renews it once under
the refresh lock (the JWT-bearer grant, or the refresh token when there is
no dynamic client) and asks again; another process that renewed meanwhile
has stored a different token, which is used instead. When the grant is
refused, the sign-in has no way to renew, or the server answers 401 to the
token it has just issued (and the Keychain still holds that token), the
Keychain item records `refused_at` (pk-med-smart's `SmartSession`, stamped
by its `Renewer` for a refused grant and by mychart for a 401 to the token
just issued): `auth status` shows the sign-in as not
authenticated (`status.refused_at` in `--json`, a `refused_at` column in
the table), the refused token is not sent again, and the next command tries one renewal grant
before any FHIR request, which clears it on success, so a refusal that
passes does not cost a sign-in. A sign-in with no way to renew (no
dynamic client and no refresh token) stays refused, exit 3, until `auth
login`.

Each Keychain item is one `SmartSession` (pk-med-smart, `format` 1), under
the account `smart/<organization>/<patient>`. mychart 0.1 stored its own
shape under the same account. 0.2 does not read it: `auth status` lists it
with `status.unreadable`, any command for that patient exits 3 with the
`auth login` command that replaces it, and `auth logout` deletes it unread.

Epic's guidelines: an app holding persistent access without a BAA "informs
the patient user of the period of time for which the app is seeking
authorization and obtains the patient user's consent" (guidelines). MyChart's
own access-period page does that; mychart prints what it got.

## 3. How a production client id reaches organizations

"Epic customers receive licenses of applications automatically on a rolling
12-hour cycle." (request process) Automatic distribution applies when the
app (patient apps):

- "Uses only USCDI v3 FHIR APIs" (Epic August 2024 and later)
- "Only reads data from Epic"
- "Is patient-facing"
- "Does not use refresh tokens OR uses refresh tokens and has a client
  credential uploaded by the vendor for that community member"
- "Selects "Enable Auto-download" and "USCDI v1" or "USCDI v3" on the app
  creation page"
- "Is marked "Ready for Production""

and the organization "Has not disabled auto-download functionality". Also:
"All US provider organizations currently receive client IDs that meet the
automatic distribution criteria." There is no per-organization activation
for such an app. A sign-in in the first 12 hours after Ready for Production,
or at an organization that turned auto-download off, fails with an
unknown-client error.

mychart's app uses no refresh tokens (the JWT-bearer grant is not one), so
it meets the condition; the docs do not say outright how a dynamic client
is counted.

## 4. Proxy access (a parent and a child)

MyChart asks at sign-in. Epic's patient guide: "If you have access to
another person's account, you will need to select to which account you are
linking the app." Its screenshot reads "Which patient do you want
<application name> to be able to link to? Yourself / Your child" (patient
auth). One sign-in links one patient, so a parent signs in once per child
(`mychart auth login --org <ORG> --label <child>`).

The token response's `patient` is the patient picked. The id token says who
signed in: "MyChart self access workflows: Patient resource / MyChart proxy
access workflows: RelatedPerson resource" (oauth2, `sub` and `fhirUser`).
mychart records `relationship: self` when `fhirUser` is `Patient/<patient>`
and `proxy` otherwise. The proxy's RelatedPerson "is represented by a
MyChart account record ID and their link to a Patient record ID" (the
RelatedPerson (Proxy) spec).

## 5. What a patient app reads; notes and PDFs

A patient app reads the APIs it registered, of those the patient allows:
"In some cases, you may be able to select what information to share" (patient
auth). A category the patient withheld answers 403, which mychart records as a
`forbidden` skip. The USCDI v3 list is in [registration.md](registration.md).

Documents are `DocumentReference` + `Binary`:

- Clinical notes: `DocumentReference?patient=<id>&category=clinical-note`;
  "This should be set to "clinical-note" to restrict the search results to
  only include the Clinical Notes sub-resource." Each note has HTML and RTF
  attachments: "The MIME content type of the note text. This may be
  text/rtf or text/html." Scanned documents carry other types.
- Lab reports: `DocumentReference (Labs)` and `Binary.Read (Labs)`; the
  Binary is the report, "first converted to HTML" or a PDF.
- `Binary` with `Accept: application/fhir+json` "is returned as a structured
  FHIR resource response with the content Base64-encoded and returned in
  the "data" element. Alternatively, if one of those Accept header values is
  not specified, the raw content is returned directly". mychart always asks
  for the FHIR form; the stream carries the Binary resource right after the
  DocumentReference or DiagnosticReport naming it.

Imaging: `ImagingStudy` is not on the USCDI v3 auto-download list, so
mychart's app does not select it. The list has `Binary (Study)`, `Media
(Study)` and `Observation (Study Finding)`: imaging reports and findings.
No API on the list serves DICOM images to a patient; images come from the
facilities (medrec).

## 6. NYU Langone

In the User-access Brands bundle (`https://open.epic.com/Endpoints/Brands`):
brand id `718`, R4 base `https://epicfhir.nyumc.org/FHIRPRD/api/FHIR/R4`.
The deprecated R4 Endpoint bundle (`https://open.epic.com/Endpoints/R4`)
lists the same base with a trailing slash. `mychart orgs search nyu` finds
it.

## The organization directory

"Applications SHOULD NOT perform runtime queries for endpoint data hosted
on open.epic.com. Applications should reguarly (for example, weekly)
download endpoint information" (endpoints). The Brands bundle is
a FHIR collection Bundle of about 97 MB on 2026-11-12, brand `Organization`s
(identifier system `https://open.epic.com/brand-identifier`), their
locations (`partOf`), and `Endpoint`s. mychart downloads it at most once a
week into `~/.cache/mychart/orgs.json` (an index of the primary brands with
an active endpoint) and never queries it per lookup. The server ignores
range requests, so a failed download starts over.

## Traps

- **`Epic-Client-ID` on `/metadata`.** "If you do not send the
  Epic-Client-ID HTTP header with the appropriate production or
  non-production client ID… you can receive the wrong token and authorize
  endpoints" (oauth2). The `register` endpoint appears only for a client id
  allowed to register dynamic clients; the sandbox's
  `.well-known/smart-configuration` has no `registration_endpoint` at all.
  mychart reads the CapabilityStatement with the header and falls back to
  smart-configuration.
- **`aud` is required** ("Starting in the May 2023 version of Epic, this
  parameter will be required"). mychart sends the directory's address
  untouched: the directory spells some hosts in capitals (`FHIR.KP.ORG`) and
  some bases with a trailing slash. The `aud` stays that untouched spelling;
  only FHIR requests, and the `fhir_base` that `orgs`, `patients` and the
  records stream report, use the base with its scheme and host lower-cased
  and the trailing slash removed.
- **PKCE is S256 only**: "Currently, only the S256 method is supported."
- **Standalone launch is GET**: "POST requests are supported for EHR launches
  in Epic version November 2020 and later, and are not currently supported
  for standalone launches" (oauth2).
- **Scopes**: "The scope parameter is set to "openid fhirUser"… Epic issues
  additional scopes based on the incoming APIs you configured on your
  application", and of `launch`: "Do not add it for standalone launches"
  (tutorial). mychart asks for `openid fhirUser` (`config set scope` changes
  it). `offline_access` is not needed: dynamic registration gives the
  persistent access.
- **An empty search is not empty**: Epic answers with an `OperationOutcome`
  entry (`search.mode = outcome`, "No results were found"); mychart drops
  it from the stream.
- **Observation, Condition, CarePlan and DocumentReference split by
  category**; a category an organization does not serve answers 400, which
  mychart records as an `unsupported` skip and goes on.
- **What each refusal becomes** (pk-med-fhir maps the status and any
  OperationOutcome issue code; mychart picks stop or skip):

  | Answer | Exit or skip |
  |---|---|
  | 401, or an issue `login`, `unknown` or `expired`, after one renewal | stop, exit 3 |
  | 403, or an issue `forbidden`, `suppressed` or `security` | `forbidden` skip |
  | 404, 410, or an issue `not-found` or `deleted` | `not_found` skip |
  | another 4xx | `unsupported` skip |
  | 429, 5xx or an HTML 403 (an edge, not the server) after its retries; a body that is not the FHIR asked for | `upstream` skip |
  | no answer after its retries | stop, exit 5 |

  A search is written page by page as it is read (pk-med-fhir's
  `search_pages`). A page that fails ends that search with one skip, whose
  reason names the page and how many before it were kept; their resources
  are in the stream.
- **Next links** are absolute URLs on the server's own spelling of the host,
  which can differ in case from the directory's; mychart compares origins
  without case and follows no link to another host. An attachment URL on
  another host is a `no_file` skip; the token is not sent there.
- **`--since`** adds `date=ge<D>` only to the searches whose type Epic
  documents a `date` parameter for: Observation, DiagnosticReport,
  DocumentReference, Encounter, Immunization and Procedure. mychart adds no
  `_lastUpdated`, which a server that does not support it can answer with a
  400 for the whole search. The Patient, the other searches and the reads by
  reference are not bounded; the stream header names their types in
  `since_unbounded`, so a reader knows they may hold older resources. A
  `Binary` comes only from a dated `DocumentReference` or `DiagnosticReport`.
- **Transient failures**: a request is sent at most three times when it gets
  no answer (the transport waits a minute for one), a 429, a 5xx or an HTML
  403: 1 then 2 seconds apart, or as long as `Retry-After` asks up to 30
  seconds (a longer ask is not waited for; its answer is the skip). Each
  retry prints a line on stderr. A 200 whose body is not FHIR JSON (a
  maintenance page) is an `upstream` skip; a search entry that is not a
  resource costs only itself and is counted in one `upstream` skip.
- **A `Binary`** is asked for as FHIR JSON (`data` in base64). One answered
  with another content type (the raw file) is an `upstream` skip, as is one
  over the stream's line limit (`too_large`).
- **`records export -o <DIR>`** (Epic's sandbox and servers on this machine
  only) writes `<DIR>/records.ndjson` when the stream is complete. A stream
  cut short (exit 3 or 5) goes to `<DIR>/records.incomplete.ndjson`
  instead, and `records.ndjson`, the last complete export, is left as it
  was. The `records-export/v1` result's `path` names the file written and
  its `complete` is false; the text output also names the kept file. The
  next complete export removes the incomplete one. A symbolic link at
  `records.ndjson` is refused before the Keychain or the network (exit 2).
- **The sandbox** is shared and refreshed weekly: "The Sandbox is refreshed
  every Sunday at approximately 8:00 PM Central Time"; a new app syncs to it
  in about an hour. Its test patients (`fhircamila`, `fhirderrick`,
  `fhirdesiree`, password `epicepic1`) are synthetic and published by Epic.

## Kaiser Permanente

Kaiser's clinical records are Epic. Kaiser's developer portal
(<https://developer.kp.org/#/apis>) lists its clinical API as "Other
Interoperability": "For Clinical Data exchange Kaiser Permanente use Epic on
FHIR." Its clinical developer guide (June 2023, linked from that entry):
"No approval (beyond the OAuth security flow) is needed for the open.epic
API" and "There is no testing with Kaiser. All testing is done with Epic in
their Testing Sandbox." So each region is an `--org` of mychart, signed in
separately.

| Brand (Brands bundle id) | R4 base | Authorization server number |
|---|---|---|
| Kaiser Permanente - Colorado (478) | `.../esb-envlbl/140/api/FHIR/R4/` | 140 |
| Kaiser Permanente - Georgia (237) | `.../esb-envlbl/200/api/FHIR/R4/` | 200 |
| Kaiser Permanente - Hawaii (479) | `.../esb-envlbl/130/api/FHIR/R4/` | 130 |
| Kaiser Permanente - Mid-Atlantic (480) | `https://FHIR.KP.ORG/.../esb-envlbl/170/api/FHIR/R4` | 170 |
| Kaiser Permanente - Northern California (482) | `https://FHIR.KP.ORG/.../esb-envlbl/326/api/FHIR/R4/` | 326 |
| Kaiser Permanente - Northwest (132) | `https://FHIR.KP.ORG/.../esb-envlbl/190/api/FHIR/R4/` | 190 |
| Kaiser Permanente - Southern California (483) | `.../esb-envlbl/226/api/FHIR/R4/` | 226 |
| Kaiser Permanente - Washington (439) | `https://fhir.kp.org/Interconnect-FHIR-PRD/api/FHIR/R4` | 750 |

`...` is `https://fhir.kp.org/service/ptnt_care/EpicEdiFhirRoutingSvc/v2014`.
The authorization server is
`https://fhir.kp.org/KPPolarisPortal/esb-envlbl/<number>/oauth2`, off the
base's path, which discovery finds; never derive it from the base. The
deprecated R4 bundle still lists Southern California at 212 and Northern
California at 312 plus a second entry ("Kaiser – NCAL - prod 326 (CIS)");
those bases authorize at 226 and 326. mychart uses Brands.

- **Discovery** at every base (read 2026-11-12) is identical apart from its
  URLs: `client-public`, `context-standalone-patient`, `permission-offline`,
  `permission-v1` and `-v2`, PKCE `S256`, and the `jwt-bearer` grant. The
  CapabilityStatement's `implementation.url` is the `KPPolarisPortal` path,
  not the base; requests and `aud` use the directory's base.
- **Northwest and Washington are two organizations.** Northwest covers Oregon
  and SW Washington; Washington is the former Group Health. A person with
  records in both signs in to each.
- **Coverage** comes through mychart: Kaiser's health plan runs in the same
  Epic instance, and `Coverage` is on the USCDI v3 list. **Claims**
  (`ExplanationOfBenefit`) do not: Kaiser's servers list EOB, but selecting
  it in the app ends automatic distribution (section 1). Kaiser serves
  claims from a separate health-plan Patient Access API at
  `kpx-service-bus.kp.org`, which needs its own reviewed registration at
  developer.kp.org; Kaiser: "The health plan API (Patient Access API) is
  separate and distinct from the open.epic clinical data API", with separate
  member consent. mychart does not call it.
- **Proxy.** Kaiser's "Act for a Family Member" gives a parent kp.org access
  to a dependent under 18 (the child's name, birth date and record number; a
  security code mailed home, or set up in person). Whether the proxied child
  then appears in the patient picker of a standalone launch at Kaiser shows
  on the first proxy sign-in.
- **Images.** Kaiser's servers list `ImagingStudy`, but the app does not
  select it (section 5). No Kaiser API serves DICOM to a patient; studies
  come by a release-of-information request to the region. Washington:
  "Copies of digital images are available on CD", and a CD not handed to the
  patient in person "will be encrypted". Northwest: an Imaging ROI request;
  dental and eye images through a download portal (Nuance PowerShare) with a
  DICOM download.
- **Distribution is unverified.** Epic distributes a qualifying app's client
  id unless an organization "has disabled auto-download functionality";
  Kaiser does not say whether it has. Loading one region's authorize URL
  with the production client id answers it without signing in: Kaiser's
  sign-in page means the client is known, an OAuth error page means it is
  not.
