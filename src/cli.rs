//! The command tree.

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_core::CommonArgs;
use pk_cli_selfupdate::SelfUpdateArgs;

/// Pull your own health records from any Epic MyChart organization through
/// Epic's patient-access SMART on FHIR API. Unofficial; not affiliated with
/// Epic Systems.
#[derive(Parser, Debug)]
#[command(name = "mychart", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Config file location (default ~/.config/mychart/config.json).
    #[arg(long, global = true, value_name = "PATH", env = "MYCHART_CONFIG")]
    pub config: Option<std::path::PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Organizations reachable through Epic's directory (org-list/v1).
    #[command(subcommand)]
    Orgs(OrgsCmd),
    /// Sign in to an organization as a patient, and the stored sign-ins.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// The patient contexts signed in (patient-list/v1).
    #[command(subcommand)]
    Patients(PatientsCmd),
    /// A patient's records as a records-stream/v1 NDJSON stream.
    #[command(subcommand)]
    Records(RecordsCmd),
    /// Clinical notes, reports and scans (documents/v1).
    #[command(subcommand)]
    Documents(DocumentsCmd),
    /// One FHIR read as the signed-in patient: `mychart api GET Patient/<id>`.
    Api(ApiArgs),
    /// Non-secret settings: the client ids, the redirect port.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
pub enum OrgsCmd {
    /// Download Epic's directory if the cached copy is a week old (Epic asks
    /// for a weekly download, never a query per lookup).
    Refresh {
        /// Download even if the cached copy is less than a week old.
        #[arg(long)]
        force: bool,
    },
    /// List every organization in the directory, by name.
    #[command(visible_alias = "ls")]
    List(OrgListArgs),
    /// Organizations whose name, operator or a clinic's name contains every
    /// word of TEXT.
    Search {
        text: String,
        #[command(flatten)]
        list: OrgListArgs,
    },
}

#[derive(Args, Debug, Clone, Default)]
pub struct OrgListArgs {
    /// At most N organizations.
    #[arg(long, value_name = "N")]
    pub limit: Option<usize>,
    /// Only organizations in this US state (two letters).
    #[arg(long, value_name = "ST")]
    pub state: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// Sign in as a patient in the system browser (MyChart's own sign-in
    /// page, PKCE, a redirect to this machine). One sign-in per
    /// organization and person; a parent with proxy access signs in once
    /// per person, picking them in MyChart.
    Login(LoginArgs),
    /// Each stored sign-in and whether it can still be used (auth-status/v1).
    Status,
    /// Remove stored sign-ins (all, or those --org/--patient select);
    /// --forget also clears the config.
    Logout(LogoutArgs),
}

#[derive(Args, Debug)]
pub struct LoginArgs {
    /// The organization: its id or name from `mychart orgs search`, or
    /// `sandbox` for Epic's sandbox.
    #[arg(long, value_name = "ORG")]
    pub org: String,
    /// A name for this person in this household (`adult1`, `child1`), to
    /// pick them with `--patient` later.
    #[arg(long, value_name = "WHO")]
    pub label: Option<String>,
    /// Print the sign-in URL instead of opening a browser.
    #[arg(long)]
    pub no_browser: bool,
    /// How long to wait for the sign-in to finish, in minutes.
    #[arg(long, value_name = "MIN", default_value_t = 10)]
    pub timeout: u64,
}

#[derive(Args, Debug)]
pub struct LogoutArgs {
    /// Only sign-ins at this organization.
    #[arg(long, value_name = "ORG")]
    pub org: Option<String>,
    /// Only this patient (FHIR id or label).
    #[arg(long, value_name = "ID")]
    pub patient: Option<String>,
    /// Also clear the config (client ids and settings).
    #[arg(long)]
    pub forget: bool,
}

#[derive(Subcommand, Debug)]
pub enum PatientsCmd {
    /// The patient contexts held: each person signed in, at each
    /// organization, with how they were reached (self or proxy).
    #[command(visible_alias = "ls")]
    List {
        /// Also read each stored sign-in to report whether it is usable
        /// (reads the Keychain).
        #[arg(long)]
        status: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Show the effective configuration.
    Show,
    /// Set a key: client_id, sandbox_client_id, redirect_port, scope,
    /// directory_url, dynamic_registration.
    Set { key: String, value: String },
    /// Remove a key.
    Unset { key: String },
}

/// Which signed-in patient a command reads.
#[derive(Args, Debug, Clone, Default)]
pub struct PatientArgs {
    /// The patient: a label from `auth login --label` or the FHIR id from
    /// `patients list`. Needed when more than one patient is signed in.
    #[arg(long, value_name = "ID")]
    pub patient: Option<String>,
    /// Only patients at this organization (its id or name).
    #[arg(long, value_name = "ORG")]
    pub org: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum RecordsCmd {
    /// Every resource the organization serves for the patient, documents
    /// included, as records-stream/v1 (what `medrec import --from mychart`
    /// reads).
    Export(ExportArgs),
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    #[command(flatten)]
    pub who: PatientArgs,
    /// Only results, notes, visits, procedures and immunizations dated on or
    /// after this day (YYYY-MM-DD); other types come whole.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub since: Option<String>,
    /// Write the stream to stdout.
    #[arg(long, conflicts_with = "output")]
    pub ndjson: bool,
    /// Write the stream to DIR/records.ndjson instead (Epic's sandbox and
    /// test servers only: real records go straight into medrec).
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<std::path::PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum DocumentsCmd {
    /// The patient's documents, newest first (document-list/v1).
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        who: PatientArgs,
        #[command(flatten)]
        range: pk_cli_documents::RangeArgs,
    },
    /// Save one document (`documents download <ID> -o <PATH>`) or every one
    /// (`--all -o <DIR>`).
    Download {
        /// The document id from `documents list`.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        id: Option<String>,
        /// Every document in the list.
        #[arg(long)]
        all: bool,
        /// A file, or a directory to save into.
        #[arg(short, long, value_name = "PATH")]
        output: std::path::PathBuf,
        #[command(flatten)]
        who: PatientArgs,
        #[command(flatten)]
        range: pk_cli_documents::RangeArgs,
    },
}

#[derive(Args, Debug)]
pub struct ApiArgs {
    /// GET only: mychart reads and never writes.
    pub method: String,
    /// A path under the organization's FHIR base (`Patient/<id>`,
    /// `Observation?patient=<id>&category=laboratory`), or a URL on that
    /// server.
    pub path: String,
    #[command(flatten)]
    pub who: PatientArgs,
}
