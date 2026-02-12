use colored::*;
use rfd::FileDialog;
use std::path::PathBuf;
use rustyline::DefaultEditor;

pub fn step(msg: &str) {
    println!("{} {}", ">>".cyan().bold(), msg.bright_white());
}

pub fn ok(msg: &str) {
    println!("{} {}", "✔".green().bold(), msg.green());
}

pub fn warn(msg: &str) {
    println!("{} {}", "⚠️".yellow().bold(), msg.yellow());
}

pub fn err(msg: &str) {
    println!("{} {}", "[!]".red().bold(), msg.red());
}

pub fn select_file(title: &str, extensions: &[&str]) -> Option<PathBuf> {
    FileDialog::new()
        .set_title(title)
        .add_filter("Image", extensions)
        .pick_file()
}

pub fn select_directory(title: &str) -> Option<PathBuf> {
    FileDialog::new()
        .set_title(title)
        .pick_folder()
}

pub fn confirm(msg: &str, default_yes: bool) -> bool {
    if default_yes {
        println!("{} [Y/n]", msg.cyan());
    } else {
        println!("{} [y/N]", msg.cyan());
    }
    let mut rl = DefaultEditor::new().unwrap();
    if let Ok(line) = rl.readline("> ") {
        let line = line.trim().to_lowercase();
        if line.is_empty() {
            default_yes
        } else {
            line == "y"
        }
    } else {
        default_yes
    }
}
