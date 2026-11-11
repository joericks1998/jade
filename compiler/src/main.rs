use jade::cli;

use clap::{Parser, Subcommand};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "jade",
    version,
    about = "The Jade programming language",
    long_about = "A production-grade language with first-class LLM integration.\n\nQuick start:\n  jade new myapp && cd myapp && jade run"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
// Parsed once per process, so the size of its largest variant costs nothing.
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Run a Jade file, project entry point, or named script
    ///
    /// With no argument, runs the project entry file (main.jde by default).
    /// If TARGET ends in .jde, runs that file.
    /// Otherwise, runs the named script from jade.toml [scripts].
    Run {
        /// File to run, script name, or omit to run the project entry point
        target: Option<String>,
        /// Arguments passed to the program
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Type-check a source file without executing it
    Check {
        /// Source file to check
        file: String,
    },

    /// Compile a Jade file to a native binary
    Build {
        /// Source file to compile
        file: Option<String>,
        /// Output binary path (default: input filename without extension)
        #[arg(short, long, value_name = "PATH")]
        output: Option<String>,
        /// Print LLVM IR (`ir`) or the typed middle layer (`mir`) instead of
        /// compiling a binary
        #[arg(long = "emit", value_name = "FORMAT")]
        emit: Option<String>,
        /// Removed in Jade 2.0: a Jade package is shared as source
        #[arg(long, hide = true)]
        lib: bool,
    },

    /// Create a new Jade project in a new directory
    New {
        /// Project name (also used as the directory name)
        name: String,
        /// Project template: basic (default) or llm
        #[arg(long, default_value = "basic")]
        template: String,
    },

    /// Initialize a Jade project in the current directory
    Init {
        /// Project template: basic (default) or llm
        #[arg(long, default_value = "basic")]
        template: String,
    },

    /// Discover and run test files (test_*.jde or *_test.jde)
    Test {
        /// Only run tests whose name contains this string
        pattern: Option<String>,
        /// Show output from each test file
        #[arg(short, long)]
        verbose: bool,
    },

    /// Format Jade source files
    ///
    /// Works on a single file or all .jde files in a directory (recursively).
    Fmt {
        /// File or directory to format
        path: String,
        /// Exit 1 if any file would be changed (for CI)
        #[arg(long)]
        check: bool,
    },

    /// Show jade environment: version, config, cache, and project info
    Env {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Manage the build cache
    Cache {
        #[command(subcommand)]
        subcommand: CacheCommands,
    },

    /// Manage project dependencies (jade.toml / jade.lock / libs/)
    ///
    /// Distinct from `jade upgrade`, which updates the jade toolchain itself.
    #[command(subcommand)]
    Pkg(PkgCommands),

    /// Upgrade jade to the latest release
    Upgrade,

    /// Reinstall jade, even if it is already the latest version
    ///
    /// Use when an installation is damaged rather than out of date — `upgrade`
    /// stops when it is already current, which is exactly no help then.
    Reinstall {
        /// Also remove ~/.jade first: cache, config, credentials, providers
        #[arg(long)]
        clean: bool,
        /// Do not ask before removing anything
        #[arg(long)]
        yes: bool,
    },

    /// Remove jade from this machine
    Uninstall {
        /// Also remove ~/.jade: cache, config, credentials, providers
        #[arg(long)]
        purge: bool,
        /// Do not ask before removing anything
        #[arg(long)]
        yes: bool,
    },

    /// Run a file directly (backward-compatible shorthand)
    ///
    /// This command is hidden — prefer `jade run <file.jde>`.
    #[command(hide = true, external_subcommand)]
    External(Vec<String>),
}

#[derive(Subcommand)]
enum CacheCommands {
    /// Show cache statistics
    Info,
    /// Remove stale or old cache entries
    Clean {
        /// Also remove entries older than this many days
        #[arg(long, value_name = "DAYS")]
        older_than: Option<u64>,
        /// Show what would be removed without deleting
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum PkgCommands {
    /// Add a dependency and install it
    Add {
        /// Name the dependency is imported by (`use <name>`)
        name: String,
        /// Local .so/.dylib, or a directory holding a Jade source package,
        /// relative to the project root
        #[arg(long, value_name = "FILE", conflicts_with_all = ["url", "git"])]
        path: Option<String>,
        /// Download URL; may contain {platform}. A .tar.gz is a Jade source package
        #[arg(long, value_name = "URL", conflicts_with = "git")]
        url: Option<String>,
        /// A git repository holding a Jade source package
        #[arg(long, value_name = "URL")]
        git: Option<String>,
        /// The tag of --git to build
        #[arg(long, value_name = "TAG", requires = "git", conflicts_with_all = ["rev", "branch"])]
        tag: Option<String>,
        /// The commit of --git to build
        #[arg(long, value_name = "COMMIT", requires = "git", conflicts_with = "branch")]
        rev: Option<String>,
        /// The branch of --git to build; jade.lock pins the commit it is at
        #[arg(long, value_name = "BRANCH", requires = "git")]
        branch: Option<String>,
        /// Exact version (required for --url; there are no version ranges)
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// Force plain-C binding. Usually unnecessary — a local artifact is
        /// recognised by whether it exports jade_pkg_init
        #[arg(long = "c-abi")]
        c_abi: bool,
        /// The C library's header. Implies --c-abi, and binds it on the spot
        #[arg(long, value_name = "FILE")]
        header: Option<String>,
        /// Extra include directory for the header; repeatable
        #[arg(short = 'I', long = "include", value_name = "DIR")]
        include: Vec<String>,
        /// Macro to define before the header is read, as clang's -D; repeatable
        #[arg(short = 'D', long = "define", value_name = "NAME[=VALUE]")]
        define: Vec<String>,
        /// Only bind symbols whose name contains this
        #[arg(long, value_name = "TEXT")]
        only: Option<String>,
    },
    /// Re-generate a C dependency's symbol table from its header
    ///
    /// `jade pkg add --header` already does this; reach for `bind` to re-run it
    /// after a header changes, or to narrow a large one with --only.
    Bind {
        /// The dependency to bind, which must already be in jade.toml
        name: String,
        /// The library's header, e.g. /opt/homebrew/include/sqlite3.h
        #[arg(long, value_name = "FILE")]
        header: String,
        /// Extra include directory; repeatable
        #[arg(short = 'I', long = "include", value_name = "DIR")]
        include: Vec<String>,
        /// Macro to define before the header is read, as clang's -D; repeatable
        #[arg(short = 'D', long = "define", value_name = "NAME[=VALUE]")]
        define: Vec<String>,
        /// Only bind symbols whose name contains this
        #[arg(long, value_name = "TEXT")]
        only: Option<String>,
        /// Show what would be written without changing jade.toml
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a dependency from jade.toml, jade.lock, and libs/
    Remove { name: String },
    /// Fetch and verify everything jade.lock pins
    Install {
        /// Fail instead of updating jade.lock (use in CI)
        #[arg(long)]
        locked: bool,
    },
    /// Re-resolve dependencies against jade.toml and rewrite jade.lock
    Update {
        /// Only this dependency (default: all)
        name: Option<String>,
    },
    /// List locked dependencies and their install status
    List,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    // `jade run app.jde | head` closes stdout early; without this the print
    // macros panic and dump a Rust backtrace over ordinary shell usage.
    jade::stdio::install_broken_pipe_hook();

    // Parsed here rather than inside `run_cli` so the libs root can be published
    // below, while this is still the only thread. See `publish_libs_root`.
    let cli = Cli::parse();
    publish_libs_root(&cli);

    // Every compiler stage recurses over the source, so the CLI runs on a thread
    // with a stack sized for that. See `COMPILER_STACK_SIZE`.
    std::thread::Builder::new()
        .name("jade-main".to_string())
        .stack_size(jade::frontend::error::COMPILER_STACK_SIZE)
        .spawn(move || run_cli(cli))
        .unwrap_or_else(|e| {
            eprintln!("jade: could not start the main thread: {e}");
            eprintln!("jade: the process is at its thread limit; raise it (ulimit -u).");
            std::process::exit(1);
        })
        .join()
        .expect("the main thread panicked");
}

/// Say where this project's dependencies live, before anything can load one.
///
/// A dependency must be loaded once per process, not once per package that uses
/// it, and `dlopen` keys a loaded image by the path it was asked for — so every
/// image has to resolve to the same path, which means agreeing on one root. The
/// CLI is the host here, so the CLI is what says which.
///
/// The channel is the environment because it is the only one there is: the
/// program runs as its own process, and a native package it loads is another
/// image, while the environment is held by libc and shared by both.
/// `backend/native.rs` reads it as the first place to look.
///
/// The root is taken from the *source file's* directory rather than the working
/// directory, matching `cli::run::run_file` — reading it from the CWD is what
/// once made the two engines disagree about what a `use` resolves to.
fn publish_libs_root(cli: &Cli) {
    // A root the user set wins, and is never replaced. It is the only way to
    // give one root to a process with no Jade host in it — a C program that
    // embeds a Jade package has no `main` of ours to publish from.
    if std::env::var_os("JADE_LIBS").is_some() {
        return;
    }

    let target = match &cli.command {
        Commands::Run { target, .. } => target.clone(),
        // `test` discovers from the working directory, so it has no path of
        // its own to anchor on.
        Commands::Test { .. } => None,
        Commands::Build { file, .. } => file.clone(),
        _ => return,
    };
    let from = match target {
        Some(t) => std::path::Path::new(&t).canonicalize().ok().and_then(|p| {
            if p.is_dir() { Some(p) } else { p.parent().map(std::path::Path::to_path_buf) }
        }),
        None => std::env::current_dir().ok(),
    };
    let Some(root) = from.and_then(|d| jade::project::find_project_root_from(&d)) else { return };

    let libs = root.join(jade::pkg::LIBS_DIR);
    if !libs.is_dir() {
        return;
    }
    let libs = std::fs::canonicalize(&libs).unwrap_or(libs);

    // SAFETY: no thread has been spawned yet, so this is the only thread in the
    // process and nothing can be reading the environment concurrently.
    // `src/cache/mod.rs` documents the same rule for the same reason.
    unsafe { std::env::set_var("JADE_LIBS", &libs) };
}

fn run_cli(cli: Cli) {
    match cli.command {
        // ── run ───────────────────────────────────────────────────────────────
        Commands::Run { target: None, args } => {
            cli::run::run_entry(&args);
        }
        Commands::Run { target: Some(ref t), ref args }
            if t.ends_with(".jde") || std::path::Path::new(t).exists() =>
        {
            cli::run::run_file(t, args);
        }
        Commands::Run { target: Some(ref t), .. } => {
            cli::run::run_script(t);
        }

        // ── check ─────────────────────────────────────────────────────────────
        Commands::Check { file } => {
            cli::check::run_check(&file);
        }

        // ── build ─────────────────────────────────────────────────────────────
        Commands::Build { lib: true, .. } => {
            eprintln!("error: `jade build --lib` is gone in Jade 2.0; a Jade package is shared");
            eprintln!("       as source: give its jade.toml a [package] section, and a project");
            eprintln!("       depends on it with `jade pkg add <name> --path <its directory>`");
            std::process::exit(1);
        }
        Commands::Build { file, output, emit, .. } => {
            cli::build::run_build(file.as_deref(), output.as_deref(), emit.as_deref());
        }

        // ── new ──────────────────────────────────────────────────────────────
        Commands::New { name, template } => {
            cli::new::run_new(&name, &template);
        }

        // ── init ─────────────────────────────────────────────────────────────
        Commands::Init { template } => {
            cli::new::run_init(&template);
        }

        // ── test ─────────────────────────────────────────────────────────────
        Commands::Test { pattern, verbose } => {
            cli::test::run_test(pattern.as_deref(), verbose);
        }

        // ── fmt ──────────────────────────────────────────────────────────────
        Commands::Fmt { path, check } => {
            cli::fmt::run_fmt(&path, check);
        }

        // ── env ──────────────────────────────────────────────────────────────
        Commands::Env { json } => {
            cli::env::run_env(json);
        }

        // ── cache ─────────────────────────────────────────────────────────────
        Commands::Cache { subcommand } => match subcommand {
            CacheCommands::Info => cli::cache::run_cache_info(),
            CacheCommands::Clean { older_than, dry_run } => {
                cli::cache::run_cache_clean(older_than, dry_run);
            }
        },

        // ── upgrade ───────────────────────────────────────────────────────────
        Commands::Pkg(subcommand) => match subcommand {
            PkgCommands::Add {
                name,
                path,
                url,
                git,
                tag,
                rev,
                branch,
                version,
                c_abi,
                header,
                include,
                define,
                only,
            } => {
                let reference = match (&tag, &rev, &branch) {
                    (Some(t), _, _) => ("tag", t.as_str()),
                    (_, Some(r), _) => ("rev", r.as_str()),
                    (_, _, Some(b)) => ("branch", b.as_str()),
                    _ => ("branch", "main"),
                };
                let git = git.as_deref().map(|url| cli::pkg::GitSource { url, reference });
                cli::pkg::run_add(
                    &name,
                    path.as_deref(),
                    url.as_deref(),
                    git,
                    version.as_deref(),
                    c_abi,
                    header.as_deref(),
                    cli::pkg::HeaderOptions {
                        include: &include,
                        defines: &define,
                        only: only.as_deref(),
                    },
                )
            }
            PkgCommands::Bind { name, header, include, define, only, dry_run } => {
                cli::pkg::run_bind(
                    &name,
                    &header,
                    cli::pkg::HeaderOptions {
                        include: &include,
                        defines: &define,
                        only: only.as_deref(),
                    },
                    dry_run,
                )
            }
            PkgCommands::Remove { name } => cli::pkg::run_remove(&name),
            PkgCommands::Install { locked } => cli::pkg::run_install(locked),
            PkgCommands::Update { name } => cli::pkg::run_update(name.as_deref()),
            PkgCommands::List => cli::pkg::run_list(),
        },

        Commands::Upgrade => {
            cli::upgrade::run_upgrade();
        }

        Commands::Reinstall { clean, yes } => {
            cli::upgrade::run_reinstall(clean, yes);
        }

        Commands::Uninstall { purge, yes } => {
            cli::upgrade::run_uninstall(purge, yes);
        }

        // ── backward-compat: jade <file.jde> [-v] ────────────────────────────
        Commands::External(args) => {
            let Some(file) = args.first() else {
                eprintln!("error: no file specified");
                std::process::exit(1);
            };
            // check it ends with .jde or the path exists, otherwise show unknown command error
            if !file.ends_with(".jde") && !std::path::Path::new(file).exists() {
                eprintln!("error: unknown command '{}'\n       Run 'jade --help' for usage.", file);
                std::process::exit(1);
            }
            cli::run::run_file(file, &args[1..]);
        }
    }
}
