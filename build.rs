fn main() {
    // Releases are tagged by commit, not by the Cargo version alone.
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git").args(args).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &reference])
    {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(revision) = git(&["rev-parse", "HEAD"]).or_else(|| std::env::var("GITHUB_SHA").ok())
        && revision.len() == 40
        && revision.bytes().all(|b| b.is_ascii_hexdigit())
    {
        println!("cargo:rustc-env=RTERM_BUILD_COMMIT={revision}");
    }
    println!("cargo:rerun-if-changed=assets/rterm.ico");
    println!("cargo:rerun-if-changed=packaging/windows/rterm.rc");
    embed_resource::compile_for(
        "packaging/windows/rterm.rc",
        ["rterm"],
        embed_resource::NONE,
    )
    .manifest_optional()
    .expect("embed the RTerm application icon");
}
