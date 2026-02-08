use colored::*;

pub fn title(msg: &str) {
    println!("{}", msg.bright_green());
}

pub fn step(msg: &str) {
    println!("{}", format!(">> {}", msg).cyan());
}

pub fn ok(msg: &str) {
    println!("{}", format!("✓ {}", msg).green());
}

pub fn warn(msg: &str) {
    println!("{}", format!("⚠ {}", msg).yellow());
}

pub fn err(msg: &str) {
    eprintln!("{}", format!("✗ {}", msg).red());
}

#[allow(dead_code)]
pub fn section(msg: &str) {
    let bar = "=".repeat(60).white();
    println!("{}", bar);
    println!("{}", msg.bright_white().bold());
    println!("{}", bar);
}

 
