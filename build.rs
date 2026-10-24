fn main() {
    println!("cargo:rerun-if-changed=kacha.rc");
    println!("cargo:rerun-if-changed=icon.ico");
    embed_resource::compile("kacha.rc", embed_resource::NONE);
}