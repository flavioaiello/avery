use std::fs;
use std::io;
use std::path::Path;
use std::process::ExitCode;

fn find_mod_rs(dir: &Path, found: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            find_mod_rs(&entry.path(), found)?;
        } else if entry.file_name() == "mod.rs" {
            found.push(entry.path().display().to_string());
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    println!("cargo:rerun-if-changed=src");
    let mut found = Vec::new();
    if let Err(error) = find_mod_rs(Path::new("src"), &mut found) {
        eprintln!("build.rs: cannot scan src/: {error}");
        return ExitCode::FAILURE;
    }
    for path in &found {
        eprintln!("build.rs: {path}: mod.rs is not allowed, use <name>.rs beside <name>/");
    }
    if found.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
