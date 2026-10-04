fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "get_state",
                "add_tab",
                "remove_tab",
                "switch_tab",
                "rename_tab",
                "reorder_tabs",
                "tab_icon",
                "refresh_tab_icon",
                "set_tab_icon",
                "reload_active_tab",
            ]),
        ),
    )
    .expect("failed to run tauri-build");
}
