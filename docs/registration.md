# Registering your household's app with Epic

`mychart` ships without a client id. Each household registers its own
patient-facing app at [fhir.epic.com](https://fhir.epic.com) and sets the two
client ids it is given. The registration is free, needs no Epic customer, and
takes about 20 minutes. Epic's wording for each step is quoted in
[api.md](api.md).

A client id of a public client is not a secret: it travels in every sign-in
URL. Epic's guidelines still ask developers to keep client identifiers
confidential, so do not publish yours.

## 1. Create a developer account

1. Open <https://fhir.epic.com> and choose **Sign Up**.
2. Fill in the sign-up page as an individual: a household registering its
   own app is not a company and not a healthcare organization. Fields
   marked `*` are required.

   | Section | Field | What to enter |
   | --- | --- | --- |
   | User Information | First Name\*, Last Name\* | Your own. |
   | Business Information | Company Email\* | Your own email address. |
   | | Phone | Optional: your own, or empty. |
   | | Company Name\* | Your own name. Epic's public pages do not say where this name appears; assume a patient signing in may see it. |
   | | Website | Optional. Choose `https://` and enter the repository of the mychart you run: `github.com/piekstra/mychart-cli`. |
   | | Country\*, Address\*, City\*, State\*, Zip Code\* | Your own address, for Epic. |

   Complete the reCAPTCHA and choose **Submit**. The page asks nothing
   about funding or data sharing; the app's questionnaire does, in step 2.
3. Confirm the address from the email Epic sends.
4. Accept the open.epic Terms of Use when asked. If Epic shows the account
   as awaiting review, wait for its email before signing in to create the
   app.

## 2. Create the app

On **Build Apps**, choose **Create** (or **Create My First App**) and fill in
the form:

| Field | Value |
| --- | --- |
| Application Name | A name your household recognises, for example `Household Records (mychart-cli)`. MyChart shows it on the sign-in consent page ("Which patient do you want <application name> to be able to link to?"). |
| Application Audience | **Patients** ("For patients and their caretakers authenticating with MyChart"; the others are Clinicians or Administrative Users, and Backend Systems) |
| Public Documentation URL | Choose `https://` (the default is `http://`) and enter `github.com/piekstra/mychart-cli`: its README says what the app reads, where records go, that it only reads, and how to end its access. |
| Incoming APIs | Every API in the [list below](#apis-to-select). Select nothing else: an ExplanationOfBenefit, ImagingStudy or any other API outside Epic's USCDI v3 list ends automatic distribution to every organization. |
| Default FHIR Version | **R4** |
| Redirect URIs | `http://127.0.0.1:7468/callback` and `http://localhost:7468/callback` (exact; Epic compares them character for character) |
| Is Confidential Client | **Unchecked**. mychart has no server to keep a secret; it registers a per-install key instead (dynamic client registration). |
| Can Register Dynamic Clients | **Checked**, if the form shows it. |
| Requires Persistent Access | Unchecked, if shown (it applies to confidential clients). |
| JWK Set URLs | Leave empty. Dynamic clients send their public key inline. |
| Enable Auto-download | **Checked** |
| USCDI version | **USCDI v3** |
| SMART scope version | Leave the default, if asked. |
| Summary | `A command-line tool a household uses to download its members' own records from MyChart through Epic's patient-access FHIR API. It only reads. Records stay on the household's computer.` |

If the form offers the app questionnaire (how the app is funded, whether it
shares data), answer it: MyChart shows the answers on the consent page, and
a warning when they are missing. For mychart: funded by the household, no
data shared or sold, records stay on the household's computer.

Choose **Save**. Do **not** choose **Ready for Production** yet: after that,
nothing but redirect URIs and JWK Set URLs can change, and a mistake means a
new app.

The saved app shows two client ids: **Non-Production Client ID** and
**Client ID** (production).

## 3. Test in Epic's sandbox

Epic syncs a new app to its sandbox within about an hour. Then:

```sh
mychart config set sandbox_client_id <Non-Production Client ID>
mychart auth login --org sandbox --label test
mychart patients list
mychart records export --org sandbox --patient test --ndjson | head -c 300
```

Sign in with one of Epic's published test patients (`fhircamila`,
`fhirderrick` or `fhirdesiree`, password `epicepic1`; synthetic people). Put
only synthetic data in the sandbox.

`mychart auth login` reports whether the organization offered dynamic client
registration. If it did not, the sign-in lasts an hour and the app's
**Can Register Dynamic Clients** setting needs checking before the next step.

## 4. Mark it Ready for Production

Open the app on **Build Apps** and choose **Save and Ready for Production**.
Epic distributes the client id to every US organization that has not turned
off auto-download, "on a rolling 12-hour cycle". An app deployed to
production "cannot be deactivated", only deactivated by each organization.

```sh
mychart config set client_id <Client ID>
mychart orgs search "<your health system>"
mychart auth login --org <ID> --label <WHO>
```

A sign-in at an organization within 12 hours of marking the app ready can
fail with an unknown-client error; wait and try again.

## APIs to select

Every R4 API on Epic's USCDI v3 auto-download list: 153 APIs, in two groups.
Nothing can be added after **Ready for Production**, so select both groups.

### Records the organization keeps (93)

- AllergyIntolerance.Read (R4)
- AllergyIntolerance.Search (R4)
- Binary.Read (Clinical Notes) (R4)
- Binary.Read (Generated CCDA) (R4)
- Binary.Read (Labs) (R4)
- Binary.Read (Study) (R4)
- Binary.Search (Clinical Notes) (R4)
- Binary.Search (Generated CCDA) (R4)
- Binary.Search (Labs) (R4)
- Binary.Search (Study) (R4)
- CarePlan.Read (Encounter-Level) (R4)
- CarePlan.Read (Longitudinal) (R4)
- CarePlan.Search (Encounter-Level) (R4)
- CarePlan.Search (Longitudinal) (R4)
- CareTeam.Read (Longitudinal) (R4)
- CareTeam.Search (Longitudinal) (R4)
- Condition.Read (Care Plan Problem) (R4)
- Condition.Read (Encounter Diagnosis) (R4)
- Condition.Read (Health Concern) (R4)
- Condition.Read (Problems) (R4)
- Condition.Search (Care Plan Problem) (R4)
- Condition.Search (Encounter Diagnosis) (R4)
- Condition.Search (Health Concern) (R4)
- Condition.Search (Problems) (R4)
- Coverage.Read (R4)
- Coverage.Search (R4)
- Device.Read (Implants) (R4)
- Device.Search (Implants) (R4)
- DiagnosticReport.Read (Results) (R4)
- DiagnosticReport.Search (Results) (R4)
- DocumentReference.Read (Clinical Notes) (R4)
- DocumentReference.Read (Generated CCDA) (R4)
- DocumentReference.Read (Labs) (R4)
- DocumentReference.Search (Clinical Notes) (R4)
- DocumentReference.Search (Generated CCDA) (R4)
- DocumentReference.Search (Labs) (R4)
- Encounter.Read (Patient Chart) (R4)
- Encounter.Search (Patient Chart) (R4)
- Goal.Read (Care Plan) (R4)
- Goal.Read (Patient) (R4)
- Goal.Search (Care Plan) (R4)
- Goal.Search (Patient) (R4)
- Immunization.Read (R4)
- Immunization.Search (R4)
- Location.Read (R4)
- Location.Search (R4)
- Media.Read (Study) (R4)
- Media.Search (Study) (R4)
- Medication.Read (R4)
- Medication.Search (R4)
- MedicationDispense.Read (Fill Status) (R4)
- MedicationDispense.Search (Fill Status) (R4)
- MedicationRequest.Read (Orders) (R4)
- MedicationRequest.Search (Orders) (R4)
- Observation.Read (Assessments) (R4)
- Observation.Read (Labs) (R4)
- Observation.Read (SDOH Assessments) (R4)
- Observation.Read (SmartData Elements) (R4)
- Observation.Read (Social History) (R4)
- Observation.Read (Study Finding) (R4)
- Observation.Read (Vitals) (R4)
- Observation.Search (Assessments) (R4)
- Observation.Search (Labs) (R4)
- Observation.Search (SDOH Assessments) (R4)
- Observation.Search (SmartData Elements) (R4)
- Observation.Search (Social History) (R4)
- Observation.Search (Study Finding) (R4)
- Observation.Search (Vitals) (R4)
- Organization.Read (R4)
- Organization.Search (R4)
- Patient.Read (R4)
- Patient.Search (R4)
- Practitioner.Read (R4)
- Practitioner.Search (R4)
- PractitionerRole.Read (R4)
- PractitionerRole.Search (R4)
- Procedure.Read (Orders) (R4)
- Procedure.Read (SDOH Intervention) (R4)
- Procedure.Read (Surgeries) (R4)
- Procedure.Search (Orders) (R4)
- Procedure.Search (SDOH Intervention) (R4)
- Procedure.Search (Surgeries) (R4)
- Provenance.Read (R4)
- RelatedPerson.Read (Friends and Family) (R4)
- RelatedPerson.Read (Proxy) (R4)
- RelatedPerson.Search (Friends and Family) (R4)
- RelatedPerson.Search (Proxy) (R4)
- ServiceRequest.Read (Community Resource) (R4)
- ServiceRequest.Read (Order Procedure) (R4)
- ServiceRequest.Search (Community Resource) (R4)
- ServiceRequest.Search (Order Procedure) (R4)
- Specimen.Read (R4)
- Specimen.Search (R4)

### Copies other organizations sent ("Outside Record", 60)

- AllergyIntolerance.Read (Outside Record) (R4)
- AllergyIntolerance.Search (Outside Record) (R4)
- Binary.Read (Outside Record Clinical Notes) (R4)
- Binary.Search (Outside Record Clinical Notes) (R4)
- CarePlan.Read (Outside Record) (R4)
- CarePlan.Search (Outside Record) (R4)
- CareTeam.Read (Outside Record) (R4)
- CareTeam.Search (Outside Record) (R4)
- Condition.Read (Outside Record Encounter Diagnosis) (R4)
- Condition.Read (Outside Record Health Concern) (R4)
- Condition.Read (Outside Record Problems) (R4)
- Condition.Search (Outside Record Encounter Diagnosis) (R4)
- Condition.Search (Outside Record Health Concern) (R4)
- Condition.Search (Outside Record Problems) (R4)
- Coverage.Read (Outside Record) (R4)
- Coverage.Search (Outside Record) (R4)
- Device.Read (Outside Record) (R4)
- Device.Search (Outside Record) (R4)
- DiagnosticReport.Read (Outside Record Results) (R4)
- DiagnosticReport.Search (Outside Record Results) (R4)
- DocumentReference.Read (Outside Record Clinical Notes) (R4)
- DocumentReference.Search (Outside Record Clinical Notes) (R4)
- Encounter.Read (Outside Record) (R4)
- Encounter.Search (Outside Record) (R4)
- Goal.Read (Outside Record) (R4)
- Goal.Search (Outside Record) (R4)
- Immunization.Read (Outside Record) (R4)
- Immunization.Search (Outside Record) (R4)
- Location.Read (Outside Record) (R4)
- Location.Search (Outside Record) (R4)
- Medication.Read (Outside Record) (R4)
- Medication.Search (Outside Record) (R4)
- MedicationDispense.Read (Outside Record) (R4)
- MedicationDispense.Search (Outside Record) (R4)
- MedicationRequest.Read (Outside Record) (R4)
- MedicationRequest.Search (Outside Record) (R4)
- Observation.Read (Outside Record Activities of Daily Living) (R4)
- Observation.Read (Outside Record Occupation) (R4)
- Observation.Read (Outside Record Pregnancy Status) (R4)
- Observation.Read (Outside Record Results) (R4)
- Observation.Read (Outside Record SDOH Assessment) (R4)
- Observation.Read (Outside Record Screening Assessment) (R4)
- Observation.Read (Outside Record Sexual Orientation) (R4)
- Observation.Read (Outside Record Smoking Status) (R4)
- Observation.Read (Outside Record Vital Sign) (R4)
- Observation.Search (Outside Record Activities of Daily Living) (R4)
- Observation.Search (Outside Record Occupation) (R4)
- Observation.Search (Outside Record Pregnancy Status) (R4)
- Observation.Search (Outside Record Results) (R4)
- Observation.Search (Outside Record SDOH Assessment) (R4)
- Observation.Search (Outside Record Screening Assessment) (R4)
- Observation.Search (Outside Record Sexual Orientation) (R4)
- Observation.Search (Outside Record Smoking Status) (R4)
- Observation.Search (Outside Record Vital Sign) (R4)
- PractitionerRole.Read (Outside Record) (R4)
- PractitionerRole.Search (Outside Record) (R4)
- Procedure.Read (Outside Record) (R4)
- Procedure.Search (Outside Record) (R4)
- Specimen.Read (Outside Record) (R4)
- Specimen.Search (Outside Record) (R4)
