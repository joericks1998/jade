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
        _ => format!("print(\"Hello from {name}!\")\n"),
    };
    fs::write(dir.join("main.jde"), main_jde).unwrap_or_else(|e| {
        eprintln!("error: could not write main.jde: {}", e);
        process::exit(1);
    });

    // .gitignore
    // libs/ holds downloaded dependency binaries; jade.lock is what travels, so
    // the artifacts themselves stay out of the repository.
    fs::write(dir.join(".gitignore"), "dist/\n*.o\nlibs/\n").unwrap_or_else(|e| {
        eprintln!("error: could not write .gitignore: {}", e);
        process::exit(1);
    });
}

/// `jade new <name> [--template basic|llm]`
pub fn run_new(name: &str, template: &str) {
    if name.contains('/') || name.contains('\\') || name.starts_with('.') {
        eprintln!(
            "error: project name '{}' must not contain path separators or start with '.'",
            name
        );
        process::exit(1);
    }

    let dir = Path::new(name);
    if dir.exists() {
        eprintln!("error: directory '{}' already exists", name);
        process::exit(1);
    }
    scaffold(dir, name, template);
    println!("created: {}/", name);
    println!("  jade.toml");
    println!("  main.jde");
    println!("  .gitignore");
    println!();
    println!("Get started:");
    println!("  cd {}", name);
    println!("  jade run");
}

/// `jade init [--template basic|llm]`
pub fn run_init(template: &str) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let toml_path = cwd.join("jade.toml");

    if toml_path.exists() {
        // Check if it already has a [project] section.
        if let Ok(content) = fs::read_to_string(&toml_path)
            && content.contains("[project]")
        {
            eprintln!("error: jade.toml already contains a [project] section");
            process::exit(1);
        }
    }

    let name = cwd.file_name().and_then(|n| n.to_str()).unwrap_or("jade-project").to_string();

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
        println!("created: main.jde");
    }

    println!("initialized jade project: {}", name);
    println!("  jade run    — run main.jde");
    println!("  jade test   — run test files");
}
