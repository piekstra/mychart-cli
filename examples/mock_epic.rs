//! A mock Epic organization on 127.0.0.1 for dogfooding the installed
//! binary end to end with invented data: `make mock`, then follow what it
//! prints. Everything it serves is synthetic (tests/fixtures/README.md).

#[path = "../tests/support/mock_epic.rs"]
mod mock_epic;

fn main() {
    let port: u16 = std::env::var("MOCK_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(9481);
    let mock = mock_epic::MockEpic::start_on(port);
    if std::env::args().any(|a| a == "--child") {
        mock.set(|st| st.pick = mock_epic::Pick::Child);
    }
    println!(
        "Mock Epic organization `{}` ({}) on {}\n\n\
         In another terminal, with a scratch config and Keychain service:\n\n  \
         export MYCHART_CONFIG=/tmp/mychart-mock/config.json\n  \
         export MYCHART_KEYCHAIN_SERVICE=piekstra.mychart.mock\n  \
         export XDG_CACHE_HOME=/tmp/mychart-mock/cache\n  \
         export MYCHART_BROWSER='curl -s -o /dev/null -L'\n  \
         mychart config set directory_url {}\n  \
         mychart config set client_id {}\n  \
         mychart auth login --org {} --label adult1\n  \
         mychart records export --patient adult1 --ndjson | head -c 400\n\n\
         The stand-in browser (curl) follows the sign-in redirect; `--child` signs in a child\n\
         through proxy access instead. Ctrl-C stops the mock.",
        mock_epic::ORG_ID,
        mock_epic::ORG_NAME,
        mock.origin(),
        mock.directory_url(),
        mock_epic::CLIENT_ID,
        mock_epic::ORG_ID,
    );
    loop {
        std::thread::park();
    }
}
