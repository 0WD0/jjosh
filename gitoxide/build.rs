use std::process::Command;

fn main() {
    let version = Command::new(if cfg!(windows) { "git.exe" } else { "git" })
        .args(["describe", r"--match=v*\.*\.*"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                parse_describe(&out.stdout)
            } else {
                None
            }
        })
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into());
    println!("cargo:rustc-env=GIX_VERSION={version}");
}

fn parse_describe(input: &[u8]) -> Option<String> {
    let input = std::str::from_utf8(input).ok()?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}
