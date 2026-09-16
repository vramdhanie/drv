mod auth;
mod claude;
mod commands;
mod config;
mod drive;
mod embed;
mod extract;
mod store;
mod ui;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand};
use colored::Colorize;

fn styles() -> Styles {
    Styles::styled()
        .header(AnsiColor::Yellow.on_default().bold())
        .usage(AnsiColor::Yellow.on_default().bold())
        .literal(AnsiColor::Green.on_default())
        .placeholder(AnsiColor::Cyan.on_default())
}

/// A fast CLI for managing your Google Drive.
///
/// Files and folders are addressed by path ("Projects/Notes/todo.txt").
/// Prefix with "id:" to address by raw Drive file ID instead.
/// Multiple Google accounts are supported: see `drv account` and --account.
#[derive(Parser)]
#[command(name = "drv", version, about, styles = styles(), arg_required_else_help = true)]
struct Cli {
    /// Act on a specific account (default: the active account)
    #[arg(short, long, global = true)]
    account: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Authenticate with Google and manage credentials
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// List and switch between signed-in Google accounts
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Browse your Drive interactively
    ///
    /// Starts at the root: scroll (or type to filter), Enter opens a
    /// folder or shows a file's details, ".." walks back up, Esc exits.
    /// Shared items and shortcuts (links to files stored elsewhere) are
    /// labelled.
    Browse,
    /// List files in a folder (root by default)
    Ls {
        /// Folder path, or "id:<fileId>"
        path: Option<String>,
        /// Recurse into subfolders, printed as a tree
        #[arg(short, long)]
        recursive: bool,
        /// Long listing: size, modified date, file ID
        #[arg(short, long)]
        long: bool,
    },
    /// Share a file or folder with an email address
    Share {
        /// File or folder path, or "id:<fileId>"
        path: String,
        /// Email address to share with
        email: String,
        /// Access level to grant
        #[arg(long, value_parser = ["viewer", "commenter", "editor"], default_value = "viewer")]
        role: String,
        /// Send the recipient a notification email
        #[arg(long)]
        notify: bool,
    },
    /// Duplicate a file (server-side copy)
    Cp {
        /// File path, or "id:<fileId>"
        path: String,
        /// Name for the copy (default: "Copy of <name>")
        new_name: Option<String>,
        /// Destination folder path (default: same folder)
        #[arg(long)]
        to: Option<String>,
    },
    /// Print a file's contents
    ///
    /// Google Docs/Sheets/Slides print their text/CSV export; text files
    /// print as-is; binary files stream only when redirected (real cat
    /// semantics, so `drv cat photo.jpg > p.jpg` copies the file).
    Cat {
        /// File path, or "id:<fileId>"
        path: String,
    },
    /// Edit a file in your editor and save it back to Drive
    ///
    /// Downloads to a temp file, opens $EDITOR (default: vim), and — only
    /// if the content changed — uploads it back in place. Google-native
    /// documents are export-only and can't be edited this way.
    #[command(visible_alias = "vim")]
    Edit {
        /// File path, or "id:<fileId>"
        path: String,
    },
    /// Move or rename a file or folder
    ///
    /// If DEST is an existing folder, SOURCE moves into it keeping its
    /// name; otherwise the last path segment of DEST becomes the new name
    /// (so `drv mv a/report.txt b/` moves, `drv mv report.txt draft.txt`
    /// renames, and `drv mv a/x.txt b/y.txt` does both).
    Mv {
        /// File or folder to move, or "id:<fileId>"
        source: String,
        /// Destination folder or new path
        dest: String,
    },
    /// Move files or folders to the Trash (never permanently deletes)
    Rm {
        /// Drive paths, or "id:<fileId>"
        #[arg(required = true)]
        paths: Vec<String>,
    },
    /// Upload one or more local files
    Upload {
        /// Local file paths
        #[arg(required = true)]
        files: Vec<std::path::PathBuf>,
        /// Destination Drive folder (default: My Drive root)
        #[arg(long)]
        to: Option<String>,
    },
    /// Download one or more files
    Download {
        /// Drive file paths, or "id:<fileId>"
        #[arg(required = true)]
        paths: Vec<String>,
        /// Local directory to save into (default: current directory)
        #[arg(long, short)]
        out: Option<std::path::PathBuf>,
    },
    /// Build or refresh the local semantic index of your Drive's contents
    ///
    /// First run crawls your Drive's metadata and embeds file text with a
    /// local model (downloaded once, runs entirely on-device); later runs
    /// are incremental via the Drive changes feed. Name folders with --in
    /// to limit content indexing to them (the choice persists); without
    /// roots, only Google-native docs and PDFs drive-wide are indexed.
    Index {
        /// Index content only under these folders (repeatable; persisted)
        #[arg(long = "in")]
        folders: Vec<String>,
        /// Clear stored folder roots and index the whole Drive again
        #[arg(long)]
        all: bool,
        /// CPU threads for the embedding model (more = faster but heavier)
        #[arg(long, default_value_t = 2)]
        threads: usize,
        /// Stop cleanly (exit 75, resumable) past this much memory, in GB
        #[arg(long, default_value_t = 3)]
        max_mem: u64,
    },
    /// Semantic search across your indexed Drive
    Search {
        /// What to look for (natural language)
        #[arg(required = true)]
        query: Vec<String>,
        /// Only search within this folder (path or "id:<fileId>")
        #[arg(long = "in")]
        folder: Option<String>,
        /// Maximum results
        #[arg(short = 'n', long, default_value_t = 8)]
        limit: usize,
    },
    /// Describe tasks in natural language; review the plan, then run it
    ///
    /// Claude turns your instruction plus the folder's metadata listing
    /// into a concrete plan (rename / move / copy / share / download /
    /// trash / new folders). The plan is printed and NOTHING runs until
    /// you confirm. Deletion is always the recoverable Trash.
    Do {
        /// What to do, e.g. "trash all empty files" or "prefix each name with its created date"
        #[arg(required = true)]
        instruction: Vec<String>,
        /// Operate within this folder (default: My Drive root)
        #[arg(long = "in")]
        folder: Option<String>,
        /// Skip the confirmation prompt
        #[arg(long)]
        yes: bool,
        /// Show the plan and stop — execute nothing
        #[arg(long, conflicts_with = "yes")]
        dry_run: bool,
    },
    /// Ask Claude a question, answered from your indexed files
    Prompt {
        /// What to ask
        #[arg(required = true)]
        question: Vec<String>,
        /// Only draw on files within this folder (path or "id:<fileId>")
        #[arg(long = "in")]
        folder: Option<String>,
        /// Claude model to use
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign in to Google (opens your browser); repeat for more accounts
    Login {
        /// Use your own OAuth client ID instead of the built-in one
        #[arg(long, requires = "client_secret")]
        client_id: Option<String>,
        /// OAuth client secret matching --client-id
        #[arg(long, requires = "client_id")]
        client_secret: Option<String>,
        /// Alias to store this account under (default: its email address)
        #[arg(long = "as")]
        alias: Option<String>,
    },
    /// Show signed-in accounts and storage usage
    Status,
    /// Remove stored Google credentials for the selected account
    Logout,
    /// Store an Anthropic API key (used by search/prompt)
    Claude,
}

#[derive(Subcommand)]
enum AccountCommand {
    /// List signed-in accounts (● marks the active one)
    List,
    /// Set the account used when --account isn't given
    Use {
        /// Account alias (see `drv account list`)
        alias: String,
    },
}

fn main() {
    // Die quietly when downstream closes the pipe (`drv ls | head`), like
    // every other Unix CLI, instead of panicking on Broken pipe.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let cli = Cli::parse();
    let account = cli.account.as_deref();
    let result = match cli.command {
        Command::Auth { command } => match command {
            AuthCommand::Login { client_id, client_secret, alias } => {
                commands::auth_login(client_id, client_secret, alias)
            }
            AuthCommand::Status => commands::auth_status(account),
            AuthCommand::Logout => commands::auth_logout(account),
            AuthCommand::Claude => commands::auth_claude(),
        },
        Command::Account { command } => match command {
            AccountCommand::List => commands::account_list(),
            AccountCommand::Use { alias } => commands::account_use(&alias),
        },
        Command::Browse => commands::browse(account),
        Command::Ls { path, recursive, long } => {
            commands::ls(account, path.as_deref(), recursive, long)
        }
        Command::Share { path, email, role, notify } => {
            commands::share(account, &path, &email, &role, notify)
        }
        Command::Cp { path, new_name, to } => {
            commands::cp(account, &path, new_name.as_deref(), to.as_deref())
        }
        Command::Cat { path } => commands::cat(account, &path),
        Command::Edit { path } => commands::edit(account, &path),
        Command::Mv { source, dest } => commands::mv(account, &source, &dest),
        Command::Rm { paths } => commands::rm(account, &paths),
        Command::Upload { files, to } => commands::upload(account, &files, to.as_deref()),
        Command::Download { paths, out } => commands::download(account, &paths, out.as_deref()),
        Command::Index { folders, all, threads, max_mem } => {
            commands::index(account, &folders, all, threads, max_mem)
        }
        Command::Search { query, folder, limit } => {
            commands::search(account, &query.join(" "), folder.as_deref(), limit)
        }
        Command::Do { instruction, folder, yes, dry_run } => {
            commands::do_task(account, folder.as_deref(), &instruction.join(" "), yes, dry_run)
        }
        Command::Prompt { question, folder, model } => {
            commands::prompt(account, &question.join(" "), folder.as_deref(), model.as_deref())
        }
    };

    if let Err(err) = result {
        eprintln!("{} {:#}", "error:".red().bold(), err);
        std::process::exit(1);
    }
}
