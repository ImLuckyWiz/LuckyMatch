fn main() {
    // Embed the app icon as resource ID 1: Explorer uses it for the .exe and
    // GPUI's Windows backend loads icon #1 for the window / taskbar icon.
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=luckymatch.rc");
        println!("cargo:rerun-if-changed=../../assets/branding/luckymatch.ico");
        embed_resource::compile("luckymatch.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("failed to embed Windows icon resource");
    }
}
