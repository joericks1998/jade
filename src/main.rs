use jade::cli;

use clap::{Parser, Subcommand};

// The `jade` binary's global allocator. Declared HERE, in the binary — never in
// `jade-runtime` — so it applies only to the `jade` process and can never be
// linked into a dlopen'd package (the mistake mimalloc made). Normally the
// Phase-1 segregated free-list pool; the `alloc-profile` feature swaps in the
// measuring allocator instead.
#[cfg(not(feature = "alloc-profile"))]
#[global_allocator]
static ALLOC: jade::pool_alloc::PoolAlloc = jade::pool_alloc::PoolAlloc;

#[cfg(feature = "alloc-profile")]
#[global_allocator]
static ALLOC: jade::alloc_profile::ProfilingAlloc = jade::alloc_profile::ProfilingAlloc;

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
enum Commands {
    /// Run a Jade file, project entry point, or named script
    ///
    /// With no argument, runs the project entry file (main.jde by default).
    /// If TARGET ends in .jde, runs that file.
    /// Otherwise, runs the named script from jade.toml [scripts].
    Run {
        /// File to run, script name, or omit to run the project entry point
        target: Option<String>,
        /// Print all global variables after execution
        #[arg(short, long)]
        verbose: bool,
    },

    /// Type-check a source file without executing it
    Check {
        /// Source file to check
        file: String,
    },

    /// Compile a Jade file to a native binary via the build daemon
    Build {
        /// Source file to compile
        file: String,
        /// Output binary path (default: input filename without extension)
        #[arg(short, long, value_name = "PATH")]
        output: Option<String>,
        /// Emit LLVM IR from the daemon instead of compiling a binary
        #[arg(long = "emit", value_name = "FORMAT")]
        emit: Option<String>,
        /// Build a shared library other Jade projects can depend on
        #[arg(long)]
        lib: bool,
        /// Export only these functions (repeatable; default: all of them)
        #[arg(long = "export", value_name = "NAME", requires = "lib")]
        export: Vec<String>,
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

    /// Start an interactive REPL session
    Repl {
        /// Print extra debug info for each evaluated expression
        #[arg(short, long)]
        verbose: bool,
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

    /// Register an inference provider and its API key (global, per-user)
    ///
    /// Providers ship with jade; this records which one `?p` uses and stores its
    /// key under ~/.jade. With no PROVIDER it lists installed providers and
    /// prompts interactively.
    Register {
        /// Provider to register (e.g. anthropic, openai); omit for interactive
        provider: Option<String>,
        /// API key to store (omit to be prompted). For CI, prefer the
        /// <PROVIDER>_API_KEY env var over passing a secret on the command line.
        #[arg(long, value_name = "KEY")]
        key: Option<String>,
        /// List installed providers and the active selection, then exit
        #[arg(long)]
        list: bool,
        /// Remove the provider's stored credential instead of setting one
        #[arg(long)]
        remove: bool,
    },

    /// Select which registered provider `?p` uses (global, per-user)
    Use {
        /// Provider name (must be installed)
        provider: String,
    },

    /// Upgrade jade to the latest release
    Upgrade,

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
        /// Local .so/.dylib, relative to the project root
        #[arg(long, value_name = "FILE", conflicts_with = "url")]
        path: Option<String>,
        /// Download URL; may contain {platform}
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Exact version (required for --url; there are no version ranges)
        #[arg(long, value_name = "VERSION")]
        version: Option<String>,
        /// The artifact is a plain C library, not a Jade-ABI package
        #[arg(long = "c-abi")]
        c_abi: bool,
    },
    /// Remove a dependency from jade.toml, jade.lock, and libs/
    Remove {
        name: String,
    },
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

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(run_cli());

    #[cfg(feature = "alloc-profile")]
    if std::env::var_os("JADE_ALLOC_PROFILE").is_some() {
        eprint!("{}", jade::alloc_profile::report());
    }
}

async fn run_cli() {
    let cli = Cli::parse();

    match cli.command {
        // ── run ───────────────────────────────────────────────────────────────
        Commands::Run { target: None, verbose } => {
            cli::run::run_entry(verbose).await;
        }
        Commands::Run { target: Some(ref t), verbose } if t.ends_with(".jde") || std::path::Path::new(t).exists() => {
            cli::run::run_file(t, verbose).await;
        }
        Commands::Run { target: Some(ref t), .. } => {
            cli::run::run_script(t);
        }

        // ── check ─────────────────────────────────────────────────────────────
        Commands::Check { file } => {
            cli::check::run_check(&file);
        }

        // ── build ─────────────────────────────────────────────────────────────
        Commands::Build { file, output, emit, lib, export } => {
            let emit_ir = emit.as_deref() == Some("ir");
            cli::build::run_build(&file, output.as_deref(), emit_ir, lib, &export);
        }

        // ── new ──────────────────────────────────────────────────────────────
        Commands::New { name, template } => {
            cli::new::run_new(&name, &template);
        }

        // ── init ─────────────────────────────────────────────────────────────
        Commands::Init { template } => {
            cli::new::run_init(&template);
        }

        // ── repl ─────────────────────────────────────────────────────────────
        Commands::Repl { verbose } => {
            cli::repl::run_repl(verbose).await;
        }

        // ── test ─────────────────────────────────────────────────────────────
        Commands::Test { pattern, verbose } => {
            cli::test::run_test(pattern.as_deref(), verbose).await;
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
            PkgCommands::Add { name, path, url, version, c_abi } => {
                cli::pkg::run_add(&name, path.as_deref(), url.as_deref(), version.as_deref(), c_abi)
            }
            PkgCommands::Remove { name } => cli::pkg::run_remove(&name),
            PkgCommands::Install { locked } => cli::pkg::run_install(locked),
            PkgCommands::Update { name } => cli::pkg::run_update(name.as_deref()),
            PkgCommands::List => cli::pkg::run_list(),
        },

        // ── provider registry ────────────────────────────────────────────────
        Commands::Register { provider, key, list, remove } => {
            cli::register::run_register(provider.as_deref(), key.as_deref(), list, remove);
        }
        Commands::Use { provider } => {
            cli::register::run_use(&provider);
        }

        Commands::Upgrade => {
            cli::upgrade::run_upgrade().await;
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
            let verbose = args.get(1).map(|f| f == "-v" || f == "--verbose").unwrap_or(false);
            cli::run::run_file(file, verbose).await;
        }
    }
}
