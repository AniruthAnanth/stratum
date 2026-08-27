//! Tauri build glue (W17). Generates the context `tauri::generate_context!`
//! consumes from `tauri.conf.json` and the `capabilities/` directory.

fn main() {
    tauri_build::build();
}
