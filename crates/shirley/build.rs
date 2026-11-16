//! Builds the web UI (`src/interface/web`) so a plain `cargo run` serves a working page.
//!
//! The bundle (`dist/`) is not checked in. It is rebuilt when the UI sources change, and only
//! then. Set `SHIRLEY_SKIP_WEB_BUILD=1` to skip (CI, or when `npm run dev` serves the UI).
//! A missing `npm` or a failed build is a warning, not an error: the server still starts and
//! says what to do.

use std::path::Path;
use std::process::Command;

fn main() {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/interface/web");
    for input in [
        "src",
        "public",
        "index.html",
        "package.json",
        "vite.config.ts",
    ] {
        println!("cargo:rerun-if-changed={}", web.join(input).display());
    }
    println!("cargo:rerun-if-env-changed=SHIRLEY_SKIP_WEB_BUILD");
    if std::env::var_os("SHIRLEY_SKIP_WEB_BUILD").is_some() {
        return;
    }

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    if !web.join("node_modules").is_dir() && !run(npm, &["install"], &web) {
        return;
    }
    run(npm, &["run", "build"], &web);
}

fn run(program: &str, args: &[&str], dir: &Path) -> bool {
    match Command::new(program).args(args).current_dir(dir).output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            println!(
                "cargo:warning=web UI: `{program} {}` failed; the server will start without a page",
                args.join(" ")
            );
            let text = String::from_utf8_lossy(&output.stderr);
            for line in text.lines().rev().take(8).collect::<Vec<_>>().into_iter().rev() {
                println!("cargo:warning=  {line}");
            }
            false
        }
        Err(error) => {
            println!(
                "cargo:warning=web UI not built: could not run `{program}` ({error}). Install Node.js, then `cargo run` again."
            );
            false
        }
    }
}
