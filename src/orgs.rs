//! Epic's organization directory: the User-access Brands bundle, cached.
//!
//! Epic publishes every organization a patient app can reach as one FHIR
//! Bundle (about 100 MB) and asks apps to download it regularly, weekly,
//! instead of querying it at runtime. `orgs refresh` downloads it at most
//! once a week and keeps a small index under `~/.cache/mychart`; every
//! lookup reads only that index.
//!
//! The bundle holds three kinds of entry (docs/api.md, "The directory"):
//! primary brands (an `Organization` with a `brand-identifier` and one
//! `endpoint`), location brands (an `Organization` whose `partOf` is a
//! primary brand), and `Endpoint`s, whose `address` is the FHIR R4 base.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pk_cli_core::transport::{require_secure_url, HttpRequest, Transport};
use pk_cli_core::{block_on, CliError};
use serde::{Deserialize, Serialize};

pub const DEFAULT_DIRECTORY_URL: &str = "https://open.epic.com/Endpoints/Brands";

/// Epic's own sandbox, reached with the non-production client id.
pub const SANDBOX_ID: &str = "sandbox";
pub const SANDBOX_FHIR_BASE: &str = "https://fhir.epic.com/interconnect-fhir-oauth/api/FHIR/R4";

/// Refresh at most this often (Epic: "download ... weekly").
pub const REFRESH_EVERY_SECS: i64 = 7 * 24 * 3600;

/// The download is large; the family's 60 s client timeout is too short
/// for it on a slow line.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

const INDEX_SCHEMA: &str = "mychart-org-index/1";
const BRAND_ID_SYSTEM: &str = "https://open.epic.com/brand-identifier";

/// One reachable organization: a primary brand and its endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Org {
    /// Epic's brand identifier (`718`), the `--org` value; `sandbox` for
    /// Epic's sandbox.
    pub id: String,
    pub name: String,
    /// The brand's FHIR resource id in the bundle.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub uuid: String,
    /// The R4 base URL, trailing slash removed.
    pub fhir_base: String,
    /// The endpoint's address exactly as the directory spells it, trailing
    /// slash and capitals included: the `aud` a sign-in sends.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub address: String,
    /// The Epic customer that runs the endpoint.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub customer: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    /// Location brands (clinics, hospitals) under this brand, as
    /// "Name (City, ST)", for search.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub locations: Vec<String>,
}

impl Org {
    pub fn sandbox() -> Self {
        Org {
            id: SANDBOX_ID.into(),
            name: "Epic sandbox (fhir.epic.com, synthetic patients)".into(),
            uuid: String::new(),
            fhir_base: SANDBOX_FHIR_BASE.into(),
            address: SANDBOX_FHIR_BASE.into(),
            customer: "Epic".into(),
            states: Vec::new(),
            locations: Vec::new(),
        }
    }

    /// The `aud` for this organization's authorize request.
    pub fn aud(&self) -> &str {
        if self.address.is_empty() {
            &self.fhir_base
        } else {
            &self.address
        }
    }

    pub fn is_sandbox(&self) -> bool {
        self.id == SANDBOX_ID
    }

    /// The profile's `Organization` DTO (org-list/v1 items).
    pub fn dto(&self, matched: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({
            "id": self.id,
            "name": self.name,
            "fhir_base": self.fhir_base,
            "vendor": "epic",
        });
        if !self.states.is_empty() {
            v["location"] = self.states.join(", ").into();
        }
        if !self.uuid.is_empty() {
            v["aliases"] = serde_json::json!([self.uuid]);
        }
        if let Some(m) = matched {
            v["matched"] = m.into();
        }
        v
    }
}

/// The cached index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgIndex {
    pub schema: String,
    pub source_url: String,
    /// Unix seconds.
    pub fetched_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_timestamp: Option<String>,
    pub orgs: Vec<Org>,
}

impl OrgIndex {
    pub fn is_stale(&self, now: i64) -> bool {
        now - self.fetched_at >= REFRESH_EVERY_SECS
    }

    /// Every organization `--org` can name: the directory's, then Epic's
    /// sandbox.
    pub fn all(&self) -> Vec<Org> {
        let mut all = self.orgs.clone();
        all.push(Org::sandbox());
        all
    }
}

// The bundle, read only as far as the index needs.
#[derive(Deserialize)]
struct Bundle {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    entry: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(rename = "fullUrl", default)]
    full_url: Option<String>,
    resource: Resource,
}

#[derive(Deserialize)]
#[serde(tag = "resourceType")]
enum Resource {
    Organization(Organization),
    Endpoint(Endpoint),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Organization {
    #[serde(default)]
    id: String,
    #[serde(default)]
    identifier: Vec<Identifier>,
    #[serde(default = "yes")]
    active: bool,
    #[serde(default)]
    name: String,
    #[serde(default)]
    address: Vec<Address>,
    #[serde(default)]
    endpoint: Vec<Reference>,
    #[serde(rename = "partOf", default)]
    part_of: Option<Reference>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct Identifier {
    #[serde(default)]
    system: String,
    #[serde(default)]
    value: String,
}

#[derive(Deserialize)]
struct Address {
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
struct Reference {
    #[serde(default)]
    reference: String,
}

#[derive(Deserialize)]
struct Endpoint {
    #[serde(default)]
    id: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    address: String,
    #[serde(rename = "managingOrganization", default)]
    managing_organization: Option<ManagingOrganization>,
}

#[derive(Deserialize)]
struct ManagingOrganization {
    #[serde(default)]
    display: Option<String>,
}

/// A FHIR base for joining paths and for the profile's `fhir_base`: scheme
/// and host lower case, trailing slashes removed, the path untouched (as
/// `pk_med_fhir::normalize_base`). The directory spells some hosts in
/// capitals (`FHIR.KP.ORG`); its untouched spelling is `Org::address`, the
/// `aud`.
pub fn normalize_base(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let split = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(split);
    format!(
        "{}://{}{path}",
        scheme.to_ascii_lowercase(),
        authority.to_ascii_lowercase()
    )
}

/// Build the index from the bundle's text.
pub fn parse_brands(text: &str) -> Result<(Vec<Org>, Option<String>), CliError> {
    let bundle: Bundle = serde_json::from_str(text).map_err(|e| {
        CliError::Upstream(format!(
            "Epic's directory is not the brands Bundle it should be: {e}"
        ))
    })?;
    let key = |full_url: &Option<String>, kind: &str, id: &str| {
        full_url.clone().unwrap_or_else(|| format!("{kind}/{id}"))
    };
    let mut endpoints: HashMap<String, Endpoint> = HashMap::new();
    let mut primaries: Vec<(String, Organization)> = Vec::new();
    let mut locations: Vec<Organization> = Vec::new();
    for e in bundle.entry {
        match e.resource {
            Resource::Endpoint(ep) => {
                endpoints.insert(key(&e.full_url, "Endpoint", &ep.id), ep);
            }
            Resource::Organization(o) if o.part_of.is_some() => locations.push(o),
            Resource::Organization(o) if !o.endpoint.is_empty() => {
                primaries.push((key(&e.full_url, "Organization", &o.id), o))
            }
            _ => {}
        }
    }
    let mut by_ref: HashMap<String, usize> = HashMap::new();
    let mut orgs = Vec::new();
    for (full_url, o) in primaries {
        if !o.active || o.name.trim().is_empty() {
            continue;
        }
        let Some(ep) = o.endpoint.iter().find_map(|r| endpoints.get(&r.reference)) else {
            continue;
        };
        if ep.address.is_empty() || (!ep.status.is_empty() && ep.status != "active") {
            continue;
        }
        let id = o
            .identifier
            .iter()
            .find(|i| i.system == BRAND_ID_SYSTEM && !i.value.is_empty())
            .map(|i| i.value.clone())
            .unwrap_or_else(|| o.id.clone());
        let mut states: Vec<String> = o.address.iter().filter_map(|a| a.state.clone()).collect();
        states.dedup();
        by_ref.insert(full_url.clone(), orgs.len());
        by_ref.insert(format!("Organization/{}", o.id), orgs.len());
        orgs.push(Org {
            id,
            name: o.name.trim().to_string(),
            uuid: o.id.clone(),
            fhir_base: normalize_base(&ep.address),
            address: ep.address.trim().to_string(),
            customer: ep
                .managing_organization
                .as_ref()
                .and_then(|m| m.display.clone())
                .unwrap_or_default(),
            states,
            locations: Vec::new(),
        });
    }
    for loc in locations {
        if !loc.active || loc.name.trim().is_empty() {
            continue;
        }
        let Some(parent) = loc.part_of.as_ref().and_then(|p| by_ref.get(&p.reference)) else {
            continue;
        };
        let place = loc.address.first().map(|a| {
            [a.city.as_deref(), a.state.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ")
        });
        let label = match place.filter(|p| !p.is_empty()) {
            Some(p) => format!("{} ({p})", loc.name.trim()),
            None => loc.name.trim().to_string(),
        };
        orgs[*parent].locations.push(label);
    }
    orgs.sort_by_key(|o| o.name.to_lowercase());
    if orgs.is_empty() {
        return Err(CliError::Upstream(
            "Epic's directory lists no organization with an endpoint".into(),
        ));
    }
    Ok((orgs, bundle.timestamp))
}

/// `${XDG_CACHE_HOME:-~/.cache}/mychart`.
pub fn cache_dir() -> Result<PathBuf, CliError> {
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(x).join("mychart"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .ok_or_else(|| CliError::Other("cannot locate the home directory for the cache".into()))?;
    Ok(PathBuf::from(home).join(".cache").join("mychart"))
}

pub fn index_path(cache: &Path) -> PathBuf {
    cache.join("orgs.json")
}

pub fn load_index(cache: &Path) -> Result<Option<OrgIndex>, CliError> {
    let path = index_path(cache);
    match std::fs::read_to_string(&path) {
        Ok(s) => match serde_json::from_str::<OrgIndex>(&s) {
            Ok(i) if i.schema == INDEX_SCHEMA => Ok(Some(i)),
            // An index from another version is rebuilt, not an error.
            _ => Ok(None),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CliError::Other(format!("reading {}: {e}", path.display()))),
    }
}

/// Download the directory and replace the index.
pub fn download<T: Transport>(
    t: &T,
    cache: &Path,
    url: &str,
    now: i64,
) -> Result<OrgIndex, CliError> {
    require_secure_url(url)?;
    let resp = block_on(
        t.send(
            HttpRequest::get(url)
                .header("Accept", "application/fhir+json, application/json")
                .user_agent("mychart", env!("CARGO_PKG_VERSION")),
        ),
    )?;
    if !resp.is_success() {
        return Err(CliError::Upstream(format!(
            "downloading Epic's directory failed (HTTP {})",
            resp.status
        )));
    }
    let (orgs, bundle_timestamp) = parse_brands(&resp.body)?;
    let index = OrgIndex {
        schema: INDEX_SCHEMA.into(),
        source_url: url.into(),
        fetched_at: now,
        bundle_timestamp,
        orgs,
    };
    let body = serde_json::to_vec(&index)
        .map_err(|e| CliError::Other(format!("serializing the directory index: {e}")))?;
    crate::fsutil::write_private(&index_path(cache), &body)?;
    Ok(index)
}

/// What [`ensure`] did.
pub enum Freshness {
    /// The cached index was young enough.
    Cached,
    /// Downloaded now.
    Refreshed,
    /// The download failed; the stale index is in use (the failure, for a
    /// warning).
    Stale(CliError),
}

/// The index, downloading the directory first when there is none, it is a
/// week old, or `force`. A failed download with an older index still
/// answers from that index.
pub fn ensure<T: Transport>(
    t: &T,
    cache: &Path,
    url: &str,
    now: i64,
    force: bool,
) -> Result<(OrgIndex, Freshness), CliError> {
    let cached = load_index(cache)?;
    match cached {
        Some(i) if !force && !i.is_stale(now) && i.source_url == url => Ok((i, Freshness::Cached)),
        cached => match download(t, cache, url, now) {
            Ok(i) => Ok((i, Freshness::Refreshed)),
            Err(e) => match cached {
                Some(old) if e.exit_code() == 5 => Ok((old, Freshness::Stale(e))),
                _ => Err(e),
            },
        },
    }
}

/// Resolve `--org` by brand id, uuid, or name (exact, then any case, then
/// a unique part), per `pk_cli_core::resolve::pick`.
pub fn resolve<'a>(orgs: &'a [Org], query: &str) -> Result<&'a Org, CliError> {
    pk_cli_core::resolve::pick(
        orgs,
        query,
        |o| {
            let mut ids = vec![o.id.clone()];
            if !o.uuid.is_empty() {
                ids.push(o.uuid.clone());
            }
            ids
        },
        |o| &o.name,
        "organization",
    )
    .map_err(|e| {
        e.with_remedy("find it with `mychart orgs search <text>` and pass its id (`--org 1234`)")
    })
}

/// Organizations whose name, customer or a location's name contains every
/// word of `text` (any case). A match through a location reports which.
pub fn search<'a>(orgs: &'a [Org], text: &str) -> Vec<(&'a Org, Option<&'a str>)> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    let hits = |s: &str| {
        let s = s.to_lowercase();
        words.iter().all(|w| s.contains(w.as_str()))
    };
    let mut out = Vec::new();
    for o in orgs {
        if hits(&o.name) || hits(&o.customer) || o.id.eq_ignore_ascii_case(text.trim()) {
            out.push((o, None));
        } else if let Some(loc) = o.locations.iter().find(|l| hits(l)) {
            out.push((o, Some(loc.as_str())));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_is_lower_cased_in_scheme_and_host_only() {
        assert_eq!(
            normalize_base(" HTTPS://FHIR.Example.ORG/Api/FHIR/R4/ "),
            "https://fhir.example.org/Api/FHIR/R4"
        );
        assert_eq!(
            normalize_base("http://127.0.0.1:9481/api/FHIR/R4"),
            "http://127.0.0.1:9481/api/FHIR/R4"
        );
        assert_eq!(
            normalize_base("https://FHIR.Example.ORG?Q=X"),
            "https://fhir.example.org?Q=X"
        );
        assert_eq!(normalize_base("not a url/"), "not a url");
    }

    fn fixture() -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/brands_bundle.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    #[test]
    fn the_bundle_becomes_one_org_per_primary_brand() {
        let (orgs, ts) = parse_brands(&fixture()).unwrap();
        assert_eq!(ts.as_deref(), Some("2026-01-01T03:00:00-06:00"));
        let names: Vec<&str> = orgs.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["Example Children's Hospital", "Example Health"]);
        let health = &orgs[1];
        assert_eq!(health.id, "1000-1");
        assert_eq!(
            health.fhir_base,
            "https://fhir.example.org/FHIRPRD/api/FHIR/R4"
        );
        assert_eq!(
            health.aud(),
            "https://fhir.example.org/FHIRPRD/api/FHIR/R4/"
        );
        assert_eq!(health.customer, "Example Health System, Inc.");
        assert_eq!(health.states, ["NY", "NJ"]);
        assert_eq!(
            health.locations,
            ["Example Health Pediatrics (Exampleton, NY)"]
        );
    }

    #[test]
    fn inactive_brands_and_endpoints_are_left_out() {
        let (orgs, _) = parse_brands(&fixture()).unwrap();
        assert!(orgs.iter().all(|o| !o.name.contains("Closed")));
    }

    #[test]
    fn resolve_by_id_name_or_part() {
        let (orgs, _) = parse_brands(&fixture()).unwrap();
        assert_eq!(resolve(&orgs, "1000-1").unwrap().name, "Example Health");
        assert_eq!(resolve(&orgs, "example health").unwrap().id, "1000-1");
        assert_eq!(resolve(&orgs, "Children").unwrap().id, "1000-2");
        // "Example" is in both names: ambiguous, never a silent first pick.
        assert_eq!(resolve(&orgs, "Example").unwrap_err().exit_code(), 4);
        assert_eq!(resolve(&orgs, "nowhere").unwrap_err().exit_code(), 4);
    }

    #[test]
    fn search_reaches_locations() {
        let (orgs, _) = parse_brands(&fixture()).unwrap();
        let hits = search(&orgs, "pediatrics exampleton");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, "1000-1");
        assert_eq!(
            hits[0].1,
            Some("Example Health Pediatrics (Exampleton, NY)")
        );
        assert_eq!(search(&orgs, "example").len(), 2);
        assert!(search(&orgs, "  ").is_empty());
    }

    #[test]
    fn a_wrong_shape_is_upstream() {
        assert_eq!(parse_brands("{\"entry\": 3}").unwrap_err().exit_code(), 5);
        assert_eq!(
            parse_brands("{\"resourceType\":\"Bundle\",\"entry\":[]}")
                .unwrap_err()
                .exit_code(),
            5
        );
    }
}
