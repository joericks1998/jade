use std::{fs, path::Path, process};

/// Scaffold a new Jade project directory.
///
/// `dir` — the directory to create (must not already exist).
/// `name` — the project name written into `jade.toml`.
/// `template` — `"basic"` or `"llm"`.
pub fn scaffold(dir: &Path, name: &str, template: &str) {
    // Create directory structure.
    fs::create_dir_all(dir).unwrap_or_else(|e| {
        eprintln!("error: could not create directory '{}': {}", dir.display(), e);
        process::exit(1);
    });

    // jade.toml
    let toml = format!(
        "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[scripts]\nbuild = \"jade build main.jde -o dist/{name}\"\n"
    );
    fs::write(dir.join("jade.toml"), toml).unwrap_or_else(|e| {
        eprintln!("error: could not write jade.toml: {}", e);
        process::exit(1);
    });

    // main.jde
    let main_jde = match template {
        "llm" => format!(
            "prompt p = \"Say exactly: Hello from {name}!\"\n\
             let response = ?p\n\
             print(response)\n"
        ),
        _ => format!(
            "print(\"Hello from {name}!\")\n"
        ),
    };
    fs::write(dir.join("main.jde"), main_jde).unwrap_or_else(|e| {
        eprintln!("error: could not write main.jde: {}", e);
        process::exit(1);
    });

    // .gitignore
    fs::write(dir.join(".gitignore"), "dist/\n*.o\n").unwrap_or_else(|e| {
        eprintln!("error: could not write .gitignore: {}", e);
        process::exit(1);
    });
}

/// `jade new <name> [--template basic|llm]`
pub fn run_new(name: &str, template: &str) {
    let dir = Path::new(name);
    if dir.exists() {
        eprintln!("error: directory '{}' already exists", name);
        process::exit(1);
    }
    scaffold(dir, name, template);
    eprintln!("created: {}/", name);
    eprintln!("  jade.toml");
    eprintln!("  main.jde");
    eprintln!("  .gitignore");
    eprintln!();
    eprintln!("Get started:");
    eprintln!("  cd {}", name);
    eprintln!("  jade run");
}

/// `jade init [--template basic|llm]`
pub fn run_init(template: &str) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let toml_path = cwd.join("jade.toml");

    if toml_path.exists() {
        // Check if it already has a [project] section.
        if let Ok(content) = fs::read_to_string(&toml_path) {
            if content.contains("[project]") {
                eprintln!("error: jade.toml already contains a [project] section");
                process::exit(1);
            }
        }
    }

    let name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("jade-project")
        .to_string();

    // Don't overwrite an existing main.jde.
    let has_main = cwd.join("main.jde").exists();

    // Write jade.toml (overwrite or create).
    let toml = format!(
        "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"\"\n\n[scripts]\nbuild = \"jade build main.jde -o dist/{name}\"\n"
    );
    fs::write(&toml_path, toml).unwrap_or_else(|e| {
        eprintln!("error: could not write jade.toml: {}", e);
        process::exit(1);
    });

    if !has_main {
        let main_jde = match template {
            "llm" => format!(
                "prompt p = \"Say exactly: Hello from {name}!\"\n\
                 let response = ?p\n\
                 print(response)\n"
            ),
            _ => format!("print(\"Hello from {name}!\")\n"),
        };
        fs::write(cwd.join("main.jde"), main_jde).unwrap_or_else(|e| {
            eprintln!("error: could not write main.jde: {}", e);
            process::exit(1);
        });
        eprintln!("created: main.jde");
    }

    eprintln!("initialized jade project: {}", name);
    eprintln!("  jade run    — run main.jde");
    eprintln!("  jade test   — run test files");
}
