use embed_manifest::manifest::{ActiveCodePage, DpiAwareness, Setting};
use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    // Common Controls v6 is required for TaskDialogIndirect; without it the exe fails to load.
    let manifest = new_manifest("SoundHonter")
        .active_code_page(ActiveCodePage::Utf8)
        .dpi_awareness(DpiAwareness::PerMonitorV2)
        .long_path_aware(Setting::Enabled);
    embed_manifest(manifest).expect("failed to embed the application manifest");
    println!("cargo:rerun-if-changed=build.rs");
}
