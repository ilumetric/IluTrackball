fn main() {
    // The icon of the program in Explorer and on the taskbar.
    embed_resource::compile("app.rc", embed_resource::NONE)
        .manifest_optional()
        .unwrap();
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
