mod auth;
mod commands;
mod config;
mod drive;
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
#[derive(Parser)]
#[command(name = "drv", version, about, styles = styles(), arg_required_else_help = true)]
struct Cli {
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
    /// Build the local semantic index (coming in v0.2)
    Index,
    /// Search your Drive semantically (coming in v0.2)
    Search {
        /// What to look for
        #[allow(dead_code)]
        query: Vec<String>,
    },
    /// Ask Claude about the contents of a folder (coming in v0.2)
    Prompt {
        /// What to ask
        #[allow(dead_code)]
        question: Vec<String>,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Sign in to Google (opens your browser)
    Login {
        /// Use your own OAuth client ID instead of the built-in one
        #[arg(long, requires = "client_secret")]
        client_id: Option<String>,
        /// OAuth client secret matching --client-id
        #[arg(long, requires = "client_id")]
        client_secret: Option<String>,
    },
    /// Show the signed-in account and storage usage
    Status,
    /// Remove stored Google credentials
    Logout,
    /// Store an Anthropic API key (used by search/prompt in v0.2)
    Claude,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Auth { command } => match command {
            AuthCommand::Login { client_id, client_secret } => {
                commands::auth_login(client_id, client_secret)
            }
            AuthCommand::Status => commands::auth_status(),
            AuthCommand::Logout => commands::auth_logout(),
            AuthCommand::Claude => commands::auth_claude(),
        },
        Command::Ls { path, recursive, long } => commands::ls(path.as_deref(), recursive, long),
        Command::Share { path, email, role, notify } => {
            commands::share(&path, &email, &role, notify)
        }
        Command::Cp { path, new_name, to } => {
            commands::cp(&path, new_name.as_deref(), to.as_deref())
        }
        Command::Upload { files, to } => commands::upload(&files, to.as_deref()),
        Command::Download { paths, out } => commands::download(&paths, out.as_deref()),
        Command::Index | Command::Search { .. } | Command::Prompt { .. } => {
            println!(
                "{} this command ships in v0.2 (local semantic index + Claude).",
                "coming soon:".yellow().bold()
            );
            Ok(())
        }
    };

    if let Err(err) = result {
        eprintln!("{} {:#}", "error:".red().bold(), err);
        std::process::exit(1);
    }
}
