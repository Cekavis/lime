fn main() {
    // Keep the management shell independently buildable before the branded icon asset is
    // introduced. The generated 1x1 icon is ignored and can be replaced by the final asset.
    let icon_dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("icons");
    std::fs::create_dir_all(&icon_dir).expect("create icon directory");
    std::fs::write(icon_dir.join("icon.ico"), PLACEHOLDER_ICON).expect("write placeholder icon");
    tauri_build::build();
}

// A valid 1x1 ICO containing a transparent PNG. This avoids making a binary asset part of the
// Phase 3 scaffold; the production icon can replace it without changing Rust code.
const PLACEHOLDER_ICON: &[u8] = &[
    0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1, 0, 32, 0, 67, 0, 0, 0, 22, 0, 0, 0,
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1,
    0, 0, 0, 1, 8, 4, 0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65,
    84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1, 39, 24, 227, 102, 0, 0,
    0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
