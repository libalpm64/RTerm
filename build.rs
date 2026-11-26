fn main() {
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
