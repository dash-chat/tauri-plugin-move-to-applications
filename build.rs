const COMMANDS: &[&str] = &["needs_move", "move_to_applications_folder"];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
