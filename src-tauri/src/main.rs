#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod session_store;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            session_store::get_session_detail,
            session_store::list_sessions
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
