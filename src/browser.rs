//! The system browser for the sign-in page.

/// Open `url` in a browser. `MYCHART_BROWSER` names the program to run
/// instead of the system's (`open`, `xdg-open`): its words are the program
/// and leading arguments, the URL is appended, and no shell is involved.
/// Returns whether a launcher ran; the URL is printed either way.
pub fn open_browser(url: &str) -> bool {
    let custom = std::env::var("MYCHART_BROWSER")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let (program, mut args): (String, Vec<String>) = match custom {
        Some(cmd) => {
            let mut words = cmd.split_whitespace().map(str::to_string);
            let Some(program) = words.next() else {
                return false;
            };
            (program, words.collect())
        }
        None if cfg!(target_os = "macos") => ("open".into(), vec![]),
        None if cfg!(target_os = "linux") => ("xdg-open".into(), vec![]),
        None => return false,
    };
    args.push(url.to_string());
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}
