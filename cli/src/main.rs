use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use google_photos_web_cli::{
    auth::{self, Session},
    client::{self, PhotosClient, SearchOptions, SearchSort},
    keepalive,
    protocol::SessionExpired,
};
use std::path::PathBuf;

const EXAMPLES: &str = r#"google-photos-web-cli: search, inspect and download your Google Photos from the terminal.
Unofficial (not affiliated with Google). It reuses your browser sign-in; no API key needed.

Usage: google-photos-web-cli [--session-file PATH] <COMMAND> [ARGS]

FIRST-TIME SETUP (sign in once, then check it worked):
  google-photos-web-cli auth chrome                      macOS: import your Chrome sign-in
  google-photos-web-cli auth cookies cookies.txt         any OS: import an exported cookies.txt
  google-photos-web-cli auth status
The session is saved to a private .env in your config directory
(change it with --session-file PATH or the GPHOTOS_SESSION_FILE variable).

COMMANDS:
  Sign in
    auth chrome             Import Chrome cookies (macOS)
    auth cookies FILE       Import a Netscape cookies.txt (any OS)
    auth import FILE        Import a session file made by 'auth export'
    auth export             Export the session to move it to another machine
    auth status             Check the saved session (never prints it)
    auth keepalive          Rotate sign-in cookies; run every ~10 min from cron
    auth logout, logout     Delete the saved session
    auth migrate-keychain   One-time upgrade from version 0.1 (macOS)
  Read (never changes your library)
    search [QUERY]          Search by text, people, place, dates; optionally download
    albums                  List albums available for adding photos
    people                  List face groups (people) and their IDs
    inspect PHOTO_ID        Show a photo's metadata and detected faces
    download PHOTO_ID       Save the full-size original
  Change (writes to your library)
    album-add PHOTO_ID...   Add photos to an existing album
    tag PHOTO_ID            Tag a detected face with an existing person

EXAMPLES:
  google-photos-web-cli search "birthday cake"
  google-photos-web-cli search "Paris October 2026" --limit 50
  google-photos-web-cli search --person "Jane Doe" --from 2026-10-01 --to 2026-10-31
  google-photos-web-cli search beach --all --json > results.json
  google-photos-web-cli albums --json
  google-photos-web-cli album-add AF1QipExamplePhotoId --album "Artwork" --dry-run
  google-photos-web-cli people --name Jane
  google-photos-web-cli inspect AF1QipExamplePhotoId --json
  google-photos-web-cli download AF1QipExamplePhotoId --dir ./photos
  google-photos-web-cli tag AF1QipExamplePhotoId --face 0 --person "Jane Doe" --dry-run

MORE HELP:  google-photos-web-cli <command> --help   (or: help <command>, e.g. help search)
OUTPUT:     readable text by default; --json on search, people, inspect, download,
            albums, album-add, tag and auth status gives JSON on stdout.
SAFETY:     Add --dry-run to preview album-add and tag. Album additions require --yes.
            Downloads never overwrite files. 'auth export' prints credentials:
            treat the result like a password. Sessions can expire or be revoked by
            Google; if a command says so, sign in again (auth chrome / auth cookies).
Exit code 0 = success, 1 = runtime error, 2 = bad command line."#;

#[derive(Parser)]
#[command(name = "google-photos-web-cli", version, about = "Search, inspect and download your Google Photos from the terminal", arg_required_else_help = true, help_template = "{after-help}", after_help = EXAMPLES)]
struct Cli {
    /// Session file to use [default: <config dir>/google-photos-web-cli/.env]
    #[arg(long, global = true, env = "GPHOTOS_SESSION_FILE")]
    session_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Import, check, refresh, or remove the locally saved session
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth chrome\n  google-photos-web-cli auth cookies cookies.txt\n  google-photos-web-cli auth status\n  google-photos-web-cli auth export --output session.env\n\nSESSION LIFETIME AND REFRESH:\n\nNo single guaranteed expiry exists: GPHOTOS_SESSION packages several cookies.\nGoogle can revoke it before their expiry dates. There is no supported OAuth\nrefresh token for this method. Normal commands persist returned cookie updates\nto the selected .env file, but cannot renew a revoked Google login.\n'auth keepalive' every ~10 minutes rotates the short-lived sign-in cookies so a\nsession used only by this CLI can last for months (docs/SESSION_REFRESH.md).\n\nOn macOS, search retries once after refreshing an expired session saved by\ngphotos auth chrome. Chrome must still be signed into the same account.\n\nIf automatic refresh is unavailable or fails:\n  1. Sign in / complete any verification at photos.google.com in your browser.\n  2. macOS: google-photos-web-cli auth chrome; any OS: google-photos-web-cli auth cookies /path/to/cookies.txt\n  3. google-photos-web-cli auth status\n  4. google-photos-web-cli auth export --output new-session.env\n     On another machine: google-photos-web-cli auth import new-session.env\n\nExport destinations must be new files; existing exports are not overwritten.\nRun 'google-photos-web-cli auth <subcommand> --help' for details on each."
    )]
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Search Google's text, people, places, dates, and content; paginate long lists
    #[command(after_help = r#"Examples:
  google-photos-web-cli search "birthday cake"
  google-photos-web-cli search "Paris October 2026"
  google-photos-web-cli search "Jane Doe and John Roe at the beach"
  google-photos-web-cli search "birthday cake" --download ./photos

Enter one phrase with names, places, dates, or objects. Google interprets it
like a search in the browser. --location and --tag add text to that same query.

ADVANCED SEARCH OPTIONS:
  google-photos-web-cli search --person "Jane Doe" --person "John Roe"
  google-photos-web-cli search --from 2026-10-01 --to 2026-10-31
  google-photos-web-cli search beach --person "Jane Doe" --from 2026-10-01

Repeat --person and/or --person-id to require every selected face group in the
same photo. Names must resolve uniquely. Use full names to disambiguate them.
Multiple people and person+keyword searches verify exact per-photo face-group
IDs. Photos with unknown tags are excluded.

--from and --to enforce inclusive YYYY-MM-DD capture dates in each photo's
returned timezone. Photos with unknown capture dates are excluded. Date-only
searches use Google's chronological library, positioned at --to.

PAGINATION AND OUTPUT:
  google-photos-web-cli search "birthday cake" --sort recent --all --limit 100 --json
  google-photos-web-cli search "birthday cake" --limit 100 --json --page-token TOKEN

Default: 20 matches, up to 100 candidates when local verification is needed.
--all follows pages automatically: default cap 10000 matches, 1000 pages, 120s
search budget. --limit and --max-seconds override caps. --sort relevant (default
for text) and --sort recent select separate Google result sets. People-only and
date-only searches use recent order. --all works with either sort.

An incomplete search reports why it stopped and how to resume. Keep the same
query, filters, account, and sort when using --page-token. You can change scan
limits, including --all. JSON includes ordering, people, person_match,
candidates_checked, complete, stop_reason, and next_page_token.
--download DIRECTORY downloads displayed matches without changing the library."#)]
    Search(SearchArgs),
    /// List albums available in the Google Photos Add to album picker
    #[command(
        after_help = "Examples:\n  google-photos-web-cli albums\n  google-photos-web-cli albums --json\n\nRead-only. Use an ID with 'album-add --album-id'."
    )]
    Albums {
        /// Print machine-readable JSON with album IDs and titles
        #[arg(long)]
        json: bool,
    },
    /// Add one or more library photos to an existing album
    #[command(after_help = r#"Examples:
  google-photos-web-cli album-add PHOTO_ID --album "Artwork" --dry-run
  google-photos-web-cli album-add PHOTO_ID_1 PHOTO_ID_2 --album-id ALBUM_ID --yes --json

Photo IDs come from 'search'. Look up album IDs with 'albums'.
Use one exact, case-sensitive album title or an album ID. Adding photos
keeps their existing library and album memberships. Members of a shared
destination album can see photos added to it. --yes applies the change."#)]
    AlbumAdd(AlbumAddArgs),
    /// List Google's returned face groups and their IDs (named groups by default)
    #[command(
        after_help = "Examples:\n  google-photos-web-cli people\n  google-photos-web-cli people --name Jane\n  google-photos-web-cli people --include-unnamed --json\n\nUse the IDs with 'search --person-id' and 'tag --person-id'. Read-only."
    )]
    People {
        /// Filter names by case-insensitive substring
        #[arg(long)]
        name: Option<String>,
        /// Include unnamed face groups
        #[arg(long)]
        include_unnamed: bool,
        /// Print machine-readable JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Show full available photo metadata: filename, camera/lens, coordinates, dates, caption, people
    #[command(
        alias = "get",
        after_help = "Examples:\n  google-photos-web-cli inspect AF1QipExamplePhotoId\n  google-photos-web-cli inspect AF1QipExamplePhotoId --json\n  google-photos-web-cli get AF1QipExamplePhotoId --json   # 'get' is an alias\n\nRead-only. PHOTO_ID comes from 'google-photos-web-cli search'.\n\nJSON includes filename, caption (also description), taken_at and uploaded_at\nwith timezone, dimensions, file size, camera make/model, lens when supplied,\nfocal length, aperture, ISO, exposure time, latitude/longitude, location labels,\nGoogle-tagged people, every detected face (index, box, assigned face group),\nalbums, source and storage attributes. Missing fields\nremain null. people_status=not_returned does not mean nobody is in the image.\nFaces are indexed left to right; use the index with 'google-photos-web-cli tag'.\nThis is Google's available metadata, not a promise of every possible EXIF tag."
    )]
    Inspect {
        /// Photo/media ID from 'google-photos-web-cli search' (not the filename or URL)
        id: String,
        /// Print machine-readable JSON (includes face_id values for 'tag')
        #[arg(long)]
        json: bool,
    },
    /// Download full-size media by the ID returned from search
    #[command(
        after_help = "Examples:\n  google-photos-web-cli download AF1QipExamplePhotoId\n  google-photos-web-cli download AF1QipExamplePhotoId --dir ./photos\n  google-photos-web-cli download AF1QipExamplePhotoId --output ./vacation.jpg --json\n\n--output and --dir cannot be combined. Read-only for your library.\n\nPrefers Google's original download URL; otherwise uses its full-size URL.\nJSON reports 'original' or 'full-size', byte count, SHA-256, and saved path.\nNever downloads a thumbnail or overwrites an existing file.\nGoogle's stored version may differ from the file originally uploaded if\nStorage Saver was used. Live Photo motion components are not exported separately."
    )]
    Download(DownloadArgs),
    /// Tag a detected face in a photo with an existing named person (writes to your library)
    #[command(
        after_help = r#"Examples (WRITES to your library unless --dry-run; this CLI has no undo):
  google-photos-web-cli inspect AF1QipExamplePhotoId       # 1. list faces: index, box, tagged person
  google-photos-web-cli tag AF1QipExamplePhotoId --face 1 --person "Jane Doe" --dry-run   # 2. preview
  google-photos-web-cli tag AF1QipExamplePhotoId --face 1 --person "Jane Doe"             # 3. apply
  google-photos-web-cli tag AF1QipExamplePhotoId --face-id FACE_ID --person-id GROUP_ID --json

You must give one of --face/--face-id AND one of --person/--person-id.

Assigns one detected face to an existing face group, like Info > People >
Edit faces > Add > (person) > Done on photos.google.com. --face is the index
shown by 'google-photos-web-cli inspect' (faces sorted left to right); --face-id is the
stable face_id from 'inspect --json'.

--person must match exactly one named face group (full or unique partial name,
same rules as 'search --person'); --person-id must be an ID from 'google-photos-web-cli people'.
A face that is already tagged is refused unless --force; a face already tagged
with the same person is left unchanged. New people cannot be created here.
--dry-run prints the exact request without sending it. Nothing is retried
automatically, and the change is confirmed from Google's response."#
    )]
    Tag(TagArgs),
    /// Remove the CLI's saved session (does not sign Chrome out)
    #[command(
        after_help = "Examples:\n  google-photos-web-cli logout\n  google-photos-web-cli --session-file ./work.env logout\n\nDeletes only the saved session file contents (same as 'auth logout'). Exported copies stay valid and your browser stays signed in."
    )]
    Logout,
}
#[derive(Subcommand)]
enum AuthCommand {
    /// Import Chrome cookies on macOS; save a portable session .env
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth chrome\n  google-photos-web-cli auth chrome --profile \"Profile 1\" --account 1\n\nSign in at photos.google.com in Chrome first. Choose the Chrome profile\ndirectory (Default, Profile 1, etc.). --account is the N in /u/N/ when a\nprofile has multiple Google accounts; it is independent of --profile.\nmacOS may ask you to allow access to Chrome Safe Storage.\nThe CLI validates Google Photos access before saving its private .env file.\nRe-run this command to refresh an expired session. If Chrome's encryption\nor device-bound sessions prevent reuse, cookie import may also fail; the\nCLI reports the failure rather than trying to bypass browser protections."
    )]
    Chrome {
        /// Chrome profile directory, e.g. Default or 'Profile 1'
        #[arg(long, default_value = "Default")]
        profile: String,
        /// Google account index within that browser profile (/u/N/)
        #[arg(long, default_value_t = 0)]
        account: u32,
    },
    /// Import an explicitly exported Netscape cookies.txt file
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth cookies cookies.txt\n  google-photos-web-cli auth cookies /path/to/cookies.txt --account 1\n\nExport cookies.txt from a signed-in photos.google.com tab with a browser extension.\n\nImports only Google Photos-compatible domains and unexpired cookies.\nThe source file is not deleted. Treat it as a password; don't commit or share\nit. The CLI saves the validated session in a private .env file on any platform."
    )]
    Cookies {
        /// Path to the Netscape-format cookies.txt file (required)
        file: PathBuf,
        /// Google account index within the browser profile (/u/N/)
        #[arg(long, default_value_t = 0)]
        account: u32,
    },
    /// Import an exported session .env file or a file containing just its token
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth import session.env\n  google-photos-web-cli --session-file ./other.env auth import session.env\n\nUse on a second machine with a file made by 'auth export --output'. Replaces the saved session. Treat the file like a password."
    )]
    Import {
        /// Path to the exported session .env (or a file holding just the token) (required)
        file: PathBuf,
    },
    /// Export one portable session value (explicitly outputs credentials)
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth export --output session.env\n  google-photos-web-cli auth export --token | wl-copy\n\nChoose exactly one of --output or --token. The output contains your login cookies: never commit or share it. --output refuses to overwrite an existing file.",
        group=clap::ArgGroup::new("destination").required(true).args(["output","token"]))]
    Export {
        /// Write GPHOTOS_SESSION=... to a new private .env file
        #[arg(long, conflicts_with = "token")]
        output: Option<PathBuf>,
        /// Print just the copyable session value; pipe directly to a clipboard
        #[arg(long)]
        token: bool,
    },
    /// One-time migration of a 0.1 macOS Keychain session to portable .env storage
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth migrate-keychain\n\nOnly needed if you used version 0.1 on macOS. Copies the old Keychain session into the portable .env file."
    )]
    MigrateKeychain,
    /// Rotate Google's short-lived sign-in cookies, save them, then verify the session
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth keepalive\n  google-photos-web-cli auth keepalive --quiet\n  google-photos-web-cli --session-file ./work.env auth keepalive\n  crontab: */10 * * * * timeout 120 google-photos-web-cli auth keepalive --quiet\n\nPOSTs https://accounts.google.com/RotateCookies with the saved cookies (the\nrefresh Google's own pages perform), saves the renewed __Secure-1PSIDTS /\nSIDCC cookies atomically to the session file, then runs the 'auth status'\ncheck. Run it every ~10 minutes, and no more than once a minute (HTTP 429).\n\nOnly for a session that nothing else uses: a copy shared with a signed-in\nbrowser fights it, and one side is signed out within minutes. Sign in once\nwith a dedicated browser profile, import, then close that browser.\nNeeds a session file; refuses GPHOTOS_SESSION from the environment.\nExits non-zero (message on stderr, never cookie values) if Google rejects\nthe session; then sign in again. See docs/SESSION_REFRESH.md."
    )]
    Keepalive {
        /// Print nothing on success (errors still go to stderr); for cron
        #[arg(long, short)]
        quiet: bool,
    },
    /// Validate the saved session; display its file path, never its value
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth status\n  google-photos-web-cli auth status --json\n  google-photos-web-cli --session-file ./work.env auth status\n\nExits non-zero if there is no valid session; then run 'auth chrome' or 'auth cookies FILE'."
    )]
    Status {
        /// Print machine-readable JSON
        #[arg(long)]
        json: bool,
    },
    /// Remove the session from the selected local .env file
    #[command(
        after_help = "Examples:\n  google-photos-web-cli auth logout\n\nSame as top-level 'logout'. Exported copies stay valid and your browser stays signed in."
    )]
    Logout,
}
#[derive(Args)]
struct SearchArgs {
    /// Google Photos search text; quote multiple words (optional if --person/--person-id/--from/--to is given)
    query: Option<String>,
    /// Named tagged person (unique full or partial face-group name)
    #[arg(long, help_heading = "Advanced filters")]
    person: Vec<String>,
    /// Exact face-group ID from 'google-photos-web-cli people'
    #[arg(long, help_heading = "Advanced filters")]
    person_id: Vec<String>,
    /// Location phrase sent unchanged to Google's search (e.g. Philly)
    #[arg(long)]
    location: Option<String>,
    /// Content/tag phrase sent to Google's search; repeat to add terms
    #[arg(long)]
    tag: Vec<String>,
    /// Inclusive capture-date lower bound, YYYY-MM-DD
    #[arg(long, help_heading = "Advanced filters")]
    from: Option<String>,
    /// Inclusive capture-date upper bound, YYYY-MM-DD
    #[arg(long, help_heading = "Advanced filters")]
    to: Option<String>,
    /// Select the Google result set (default: relevant for text, recent otherwise)
    #[arg(long, value_enum)]
    sort: Option<SearchSort>,
    /// Follow Google result pages automatically (default cap: 10000 matches)
    #[arg(long)]
    all: bool,
    /// Search time budget including retries; partial results include a cursor
    #[arg(long,default_value_t=120,value_parser=clap::value_parser!(u64).range(1..=86400))]
    max_seconds: u64,
    /// Maximum matches (default 20, or 10000 with --all)
    #[arg(long,value_parser=clap::value_parser!(u32).range(1..=10000))]
    limit: Option<u32>,
    /// Continue the same search using next_page_token from JSON output
    #[arg(long)]
    page_token: Option<String>,
    /// Download each displayed result into this directory
    #[arg(long, value_name = "DIRECTORY")]
    download: Option<PathBuf>,
    /// Print machine-readable JSON (progress and errors go to stderr)
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
struct DownloadArgs {
    /// Photo/media ID from google-photos-web-cli search (not the filename or URL)
    id: String,
    /// Exact output filename; existing files are never overwritten
    #[arg(short, long, conflicts_with = "dir")]
    output: Option<PathBuf>,
    /// Directory for the original filename (defaults to current directory)
    #[arg(long)]
    dir: Option<PathBuf>,
    /// Print machine-readable JSON (source kind, bytes, SHA-256, saved path)
    #[arg(long)]
    json: bool,
}
#[derive(Args)]
#[command(group = clap::ArgGroup::new("album_target").required(true).args(["album", "album_id"]))]
#[command(group = clap::ArgGroup::new("album_action").required(true).args(["dry_run", "yes"]))]
struct AlbumAddArgs {
    /// One or more photo IDs from 'search'
    #[arg(required = true, num_args = 1..)]
    photo_ids: Vec<String>,
    /// Exact, case-sensitive title of a uniquely named album
    #[arg(long)]
    album: Option<String>,
    /// Exact destination ID from 'albums'
    #[arg(long)]
    album_id: Option<String>,
    /// Show the destination ID and photo IDs without adding anything
    #[arg(long)]
    dry_run: bool,
    /// Add photos to the specified album
    #[arg(long)]
    yes: bool,
    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,
}

fn run_album_add(mut args: AlbumAddArgs) -> Result<()> {
    for id in &args.photo_ids {
        client::check_id(id)?;
    }
    let mut seen = std::collections::HashSet::new();
    args.photo_ids.retain(|id| seen.insert(id.clone()));

    let album_id = if let Some(id) = args.album_id {
        client::check_id(&id).context("Invalid album ID")?;
        if args.yes {
            with_saved(|client| client.add_to_album(&id, &args.photo_ids))?;
        }
        id
    } else {
        let title = args.album.context("Choose --album or --album-id")?;
        with_saved(|client| {
            let mut matches = client
                .albums()?
                .into_iter()
                .filter(|album| album.title.as_deref() == Some(title.as_str()));
            let album = matches
                .next()
                .with_context(|| format!("No album titled {title:?} in the album picker"))?;
            if matches.next().is_some() {
                bail!("More than one album is titled {title:?}. Run 'albums' and use --album-id");
            }
            if args.yes {
                client.add_to_album(&album.id, &args.photo_ids)?;
            }
            Ok(album.id)
        })?
    };

    if args.json {
        print_json(&serde_json::json!({
            "status": if args.dry_run { "dry_run" } else { "added" },
            "album_id": album_id,
            "photo_ids": args.photo_ids,
        }))?;
    } else if args.dry_run {
        println!("Would add these photos to album {album_id}:");
        for id in args.photo_ids {
            println!("{id}");
        }
    } else {
        println!(
            "Google Photos confirmed {} photo(s) in album {album_id}.",
            args.photo_ids.len()
        );
    }
    Ok(())
}

#[derive(Args)]
#[command(group = clap::ArgGroup::new("target_face").required(true).args(["face", "face_id"]))]
#[command(group = clap::ArgGroup::new("target_person").required(true).args(["person", "person_id"]))]
struct TagArgs {
    /// Photo/media ID from google-photos-web-cli search
    id: String,
    /// Face index from 'google-photos-web-cli inspect' (0 = leftmost face)
    #[arg(long)]
    face: Option<usize>,
    /// Exact face_id from 'google-photos-web-cli inspect --json'
    #[arg(long)]
    face_id: Option<String>,
    /// Existing named person (unique full or partial face-group name)
    #[arg(long)]
    person: Option<String>,
    /// Exact face-group ID from 'google-photos-web-cli people'
    #[arg(long)]
    person_id: Option<String>,
    /// Replace an existing tag on that face
    #[arg(long)]
    force: bool,
    /// Print the request that would be sent; change nothing
    #[arg(long)]
    dry_run: bool,
    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,
}
fn run_tag(client: &mut PhotosClient, args: &TagArgs) -> Result<()> {
    let photo = client.photo_faces(&args.id)?;
    let face = match (&args.face, &args.face_id) {
        (Some(n), _) => photo.faces.get(*n).with_context(|| {
            format!(
                "Photo has {} detected face(s); --face must be below that. Run 'google-photos-web-cli inspect {}'",
                photo.faces.len(),
                args.id
            )
        })?,
        (None, Some(face_id)) => photo
            .faces
            .iter()
            .find(|f| f.face_id == *face_id)
            .with_context(|| {
                format!(
                    "No detected face {face_id:?} in this photo. Run 'google-photos-web-cli inspect {} --json'",
                    args.id
                )
            })?,
        (None, None) => bail!("Choose --face N or --face-id ID"),
    };
    let person = client.resolve_person(args.person.as_deref(), args.person_id.as_deref())?;
    let label = |p: &google_photos_web_cli::protocol::Person| {
        p.name
            .clone()
            .unwrap_or_else(|| format!("unnamed face group {}", p.id))
    };
    let status = match &face.person {
        Some(current) if current.id == person.id => "already_tagged",
        Some(current) if !args.force => bail!(
            "Face {} is already tagged as {}. Re-run with --force to replace it",
            face.index,
            label(current)
        ),
        _ if args.dry_run => "dry_run",
        _ => "tagged",
    };
    let payload = google_photos_web_cli::protocol::tag_face_payload(
        &photo.media_key,
        &face.face_id,
        &person.id,
    );
    if status == "tagged" {
        eprintln!(
            "Tagging face {} in {} as {}…",
            face.index,
            photo.filename.as_deref().unwrap_or(&photo.id),
            label(&person)
        );
        client.tag_face(&photo, &face.face_id, &person.id)?;
    }
    if args.json {
        let mut value = serde_json::json!({
            "status": status,
            "photo_id": photo.id,
            "filename": photo.filename,
            "face": face,
            "person": person,
            "previous_person": face.person,
        });
        if status == "dry_run" {
            value["request"] = serde_json::json!({"rpcid": "k9FTge", "payload": payload});
        }
        print_json(&value)?;
        return Ok(());
    }
    match status {
        "already_tagged" => println!(
            "Face {} is already tagged as {}; nothing changed.",
            face.index,
            label(&person)
        ),
        "dry_run" => {
            println!(
                "Dry run: would tag face {} ({}) as {} ({}).",
                face.index,
                face.face_id,
                label(&person),
                person.id
            );
            if let Some(current) = &face.person {
                println!("This replaces the current tag: {}.", label(current));
            }
            println!("RPC: k9FTge");
            println!("f.req payload: {payload}");
        }
        _ => println!(
            "Tagged face {} as {}. Check with 'google-photos-web-cli inspect {}'.",
            face.index,
            label(&person),
            photo.id
        ),
    }
    Ok(())
}
fn print_json(value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}
fn with_saved<T>(run: impl FnOnce(&mut PhotosClient) -> Result<T>) -> Result<T> {
    run_and_save(PhotosClient::connect(auth::load()?)?, run)
}

fn run_and_save<T>(
    mut client: PhotosClient,
    run: impl FnOnce(&mut PhotosClient) -> Result<T>,
) -> Result<T> {
    let result = run(&mut client);
    // Even a failed search may have rotated session cookies.
    let saved = client.save();
    if result.is_err() {
        if saved.is_err() {
            eprintln!("Warning: could not save updated session in the private .env file");
        }
        result
    } else {
        saved.context(
            "Command finished, but updated session could not be saved in the private .env file",
        )?;
        result
    }
}
fn with_search_session<T>(mut search: impl FnMut(&mut PhotosClient) -> Result<T>) -> Result<T> {
    let session = auth::load()?;
    let target = session.chrome_refresh_target();
    let result =
        PhotosClient::connect(session).and_then(|client| run_and_save(client, &mut search));
    let Some((profile, account, expected_email)) = target else {
        return result;
    };
    if !result
        .as_ref()
        .is_err_and(|error| error.is::<SessionExpired>())
    {
        return result;
    }

    eprintln!("Google Photos session expired. Refreshing from the saved Chrome profile and retrying once…");
    let store = auth::import_chrome(&profile).context("Could not refresh from Chrome")?;
    let mut client = PhotosClient::connect(Session::new(&store, account, Some(profile))?)?;
    if !client
        .account_email()
        .is_some_and(|email| email.eq_ignore_ascii_case(&expected_email))
    {
        bail!("Chrome is signed into a different Google Photos account. Sign into the original account and run 'google-photos-web-cli auth chrome'");
    }
    client.enable_chrome_refresh();
    client.save()?;
    run_and_save(client, search)
}
fn run(cli: Cli) -> Result<()> {
    if let Some(path) = cli.session_file {
        std::env::set_var("GPHOTOS_SESSION_FILE", path);
    }
    match cli.command {
        Command::Auth { command } => {
            match command {
                AuthCommand::Chrome { profile, account } => {
                    eprintln!("Importing Chrome session; macOS may prompt for Chrome Safe Storage access…");
                    let store = auth::import_chrome(&profile)?;
                    let session = Session::new(&store, account, Some(profile))?;
                    let mut client = PhotosClient::connect(session)?;
                    client.enable_chrome_refresh();
                    client.save()?;
                    println!(
                        "Google Photos session saved to {}. Account: {} (index {}).",
                        auth::session_path()?.display(),
                        client.account_email().unwrap_or("signed in"),
                        client.account_index()
                    );
                }
                AuthCommand::Cookies { file, account } => {
                    let session = Session::new(&auth::import_netscape(&file)?, account, None)?;
                    let client = PhotosClient::connect(session)?;
                    client.save()?;
                    println!(
                        "Google Photos session saved to {}.",
                        auth::session_path()?.display()
                    );
                }
                AuthCommand::Import { file } => {
                    let client = PhotosClient::connect(Session::import_file(&file)?)?;
                    client.save()?;
                    println!(
                        "Session imported and verified. Saved to {}.",
                        auth::session_path()?.display()
                    );
                }
                AuthCommand::MigrateKeychain => {
                    let client = PhotosClient::connect(auth::import_legacy_keychain()?)?;
                    client.save()?;
                    println!("Session migrated to {}.", auth::session_path()?.display());
                }
                AuthCommand::Export { output, token } => {
                    let session = auth::load()?;
                    if token {
                        println!("{}", session.portable_token()?);
                    } else if let Some(path) = output {
                        session.export(&path)?;
                        println!("Private session exported to {}. Copy it securely to the other machine and run 'google-photos-web-cli auth import FILE'.",path.display());
                    }
                }
                AuthCommand::Status { json } => with_saved(|client| {
                    if json {
                        print_json(
                            &serde_json::json!({"authenticated":true,"account":client.account_email(),"account_index":client.account_index(),"storage":if std::env::var_os("GPHOTOS_SESSION").is_some(){"environment"}else{"session_env_file"},"session_file":if std::env::var_os("GPHOTOS_SESSION").is_some(){None}else{Some(auth::session_path()?)}}),
                        )?;
                    } else {
                        println!(
                            "Authenticated: {} (account index {}). Session file: {}.",
                            client.account_email().unwrap_or("Google Photos"),
                            client.account_index(),
                            auth::session_path()?.display()
                        );
                    }
                    Ok(())
                })?,
                AuthCommand::Keepalive { quiet } => {
                    let (session, report) = keepalive::rotate(&auth::load()?)?;
                    session.save().context(
                        "Google rotated the cookies, but the session file could not be updated",
                    )?;
                    let client = PhotosClient::connect(session).context(
                        "Cookies were rotated and saved, but the Google Photos check failed",
                    )?;
                    client.save()?;
                    if !quiet {
                        let rotated = if report.rotated_psidts() {
                            format!("{} renewed", keepalive::PSIDTS)
                        } else {
                            format!("Google did not return a new {}", keepalive::PSIDTS)
                        };
                        println!(
                            "Session kept alive: {rotated}; {} cookie(s) updated. Authenticated: {} (account index {}). Session file: {}.",
                            report.changed.len(),
                            client.account_email().unwrap_or("Google Photos"),
                            client.account_index(),
                            auth::session_path()?.display()
                        );
                    }
                }
                AuthCommand::Logout => {
                    auth::logout()?;
                    println!("Local session file cleared. Chrome and exported copies remain signed in. Unset GPHOTOS_SESSION too if you supplied it in your shell.");
                }
            }
        }
        Command::Logout => {
            auth::logout()?;
            println!("Local session file cleared. Chrome and exported copies remain signed in. Unset GPHOTOS_SESSION too if you supplied it in your shell.");
        }
        Command::Search(args) => {
            let dates = google_photos_web_cli::search::DateRange::parse(
                args.from.as_deref(),
                args.to.as_deref(),
            )?;
            let query = google_photos_web_cli::search::compose_query(
                args.query.as_deref(),
                args.location.as_deref(),
                &args.tag,
            );
            if query.is_empty()
                && args.person.is_empty()
                && args.person_id.is_empty()
                && !dates.active()
            {
                bail!("Provide a search query, --person NAME, --location PLACE, --tag TERM, or --from/--to dates. See 'google-photos-web-cli help search'");
            }
            let count = args.person.len() + args.person_id.len();
            if count > 1 || (count > 0 && (!query.is_empty() || dates.active())) {
                eprintln!("Verifying that every selected person is tagged in each result…");
            }
            with_search_session(|client| {
                let result = client.search_people(
                    &query,
                    &args.person,
                    &args.person_id,
                    SearchOptions {
                        limit: args.limit.unwrap_or(if args.all { 10000 } else { 20 }) as usize,
                        all: args.all,
                        sort: args.sort,
                        max_seconds: args.max_seconds,
                        dates: dates.clone(),
                    },
                    args.page_token.as_deref(),
                )?;
                if let Some(token) = &result.next_page_token {
                    let reason = match result.stop_reason.as_str() {
                        "result_limit" => "reached the result limit",
                        "candidate_limit" => "reached the candidate scan limit",
                        "time_limit" => "reached the time budget",
                        "page_limit" => "reached the page limit",
                        other => other,
                    };
                    eprintln!(
                        "Search incomplete: {reason} after checking {} candidate(s).",
                        result.candidates_checked
                    );
                    eprintln!(
                        "Resume with the same query and filters. Keep sort order ({}), and set --page-token {token}.",
                        result.ordering
                    );
                    if result.stop_reason == "candidate_limit" {
                        eprintln!("Add --all to scan beyond the default candidate limit.");
                    }
                }
                let mut downloads = Vec::new();
                let mut failures = Vec::new();
                if let Some(dir) = &args.download {
                    for item in &result.items {
                        eprintln!(
                            "Downloading {}…",
                            item.filename.as_deref().unwrap_or(&item.id)
                        );
                        match client.download(&item.id, None, dir) {
                            Ok(saved) => downloads.push(saved),
                            Err(e) => failures
                                .push(serde_json::json!({"id":item.id,"error":format!("{e:#}")})),
                        }
                    }
                }
                if args.json {
                    let mut value = serde_json::to_value(&result)?;
                    if args.download.is_some() {
                        value["downloads"] = serde_json::to_value(&downloads)?;
                        value["download_errors"] = serde_json::to_value(&failures)?;
                    }
                    print_json(&value)?;
                } else {
                    println!("{} result(s) for {:?}", result.items.len(), result.query);
                    if !result.people.is_empty() {
                        println!(
                            "Tagged people (ALL required): {}",
                            result
                                .people
                                .iter()
                                .map(|p| p.name.as_deref().unwrap_or(&p.id))
                                .collect::<Vec<_>>()
                                .join(" AND ")
                        );
                    }
                    println!("Order: {}", result.ordering);
                    for item in &result.items {
                        println!(
                            "{}  {}  {}  {}",
                            item.id,
                            item.taken_at.as_deref().unwrap_or("date unknown"),
                            item.media_type,
                            item.filename.as_deref().unwrap_or("filename unavailable")
                        );
                    }
                    if result.people_metadata_unavailable > 0 {
                        eprintln!(
                            "Skipped {} result(s) whose people metadata was unavailable.",
                            result.people_metadata_unavailable
                        );
                    }
                    if result.date_metadata_unavailable > 0 {
                        eprintln!(
                            "Skipped {} result(s) with unavailable capture dates.",
                            result.date_metadata_unavailable
                        );
                    }
                    for saved in &downloads {
                        println!(
                            "Saved {} ({} bytes, {})",
                            saved.path.display(),
                            saved.bytes,
                            saved.representation
                        );
                    }
                    for error in &failures {
                        eprintln!("Download failed for {}: {}", error["id"], error["error"]);
                    }
                }
                if !failures.is_empty() {
                    bail!(
                        "{} download(s) failed; successful downloads were retained",
                        failures.len()
                    );
                }
                Ok(())
            })?;
        }
        Command::People {
            name,
            include_unnamed,
            json,
        } => with_saved(|client| {
            let people = client.people()?;
            let returned = people.len();
            let people = people
                .into_iter()
                .filter(|p| include_unnamed || p.name.is_some())
                .filter(|p| {
                    name.as_deref().is_none_or(|q| {
                        p.name
                            .as_deref()
                            .is_some_and(|n| n.to_lowercase().contains(&q.to_lowercase()))
                    })
                })
                .collect::<Vec<_>>();
            if json {
                print_json(
                    &serde_json::json!({"people":people,"groups_returned_by_google":returned,"coverage":"groups_returned_by_google"}),
                )?;
            } else {
                for p in people {
                    println!("{}  {}", p.id, p.name.as_deref().unwrap_or("unnamed"));
                }
            }
            Ok(())
        })?,
        Command::Inspect { id, json } => {
            client::check_id(&id)?;
            with_saved(|client| {
                let info = client.inspect(&id)?;
                if json {
                    print_json(&info)?;
                } else {
                    print_inspection(&info);
                }
                Ok(())
            })?;
        }
        Command::Albums { json } => with_saved(|client| {
            let albums = client.albums()?;
            if json {
                print_json(&albums)?;
            } else {
                for album in albums {
                    println!(
                        "{}\t{}",
                        album.id,
                        album.title.as_deref().unwrap_or("(untitled)")
                    );
                }
            }
            Ok(())
        })?,
        Command::AlbumAdd(args) => run_album_add(args)?,
        Command::Tag(args) => {
            client::check_id(&args.id)?;
            with_saved(|client| run_tag(client, &args))?;
        }
        Command::Download(args) => {
            client::check_id(&args.id)?;
            with_saved(|client| {
                let result = client.download(
                    &args.id,
                    args.output.as_deref(),
                    args.dir.as_deref().unwrap_or(std::path::Path::new(".")),
                )?;
                if args.json {
                    print_json(&result)?;
                } else {
                    println!(
                        "Saved {} ({} bytes, {}).",
                        result.path.display(),
                        result.bytes,
                        result.representation
                    );
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}
fn print_inspection(info: &google_photos_web_cli::protocol::Inspection) {
    println!("Photo: {}", info.id);
    println!(
        "Filename: {}",
        info.filename.as_deref().unwrap_or("unknown")
    );
    println!("Taken: {}", info.taken_at.as_deref().unwrap_or("unknown"));
    println!(
        "Uploaded: {}",
        info.uploaded_at.as_deref().unwrap_or("unknown")
    );
    println!(
        "Dimensions: {} × {}",
        info.width
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into()),
        info.height
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into())
    );
    println!(
        "File size: {}",
        info.size_bytes
            .map(|v| format!("{v} bytes"))
            .unwrap_or_else(|| "unknown".into())
    );
    if let Some(camera) = &info.camera {
        println!(
            "Camera: {} {}",
            camera.make.as_deref().unwrap_or(""),
            camera.model.as_deref().unwrap_or("")
        );
        println!(
            "Camera settings: {}",
            serde_json::to_string(camera).unwrap_or_default()
        );
    } else {
        println!("Camera: unknown");
    }
    if let Some(location) = &info.location {
        println!("Location: {}", location.labels.join(", "));
        if let (Some(lat), Some(lon)) = (location.latitude, location.longitude) {
            println!("Coordinates: {lat}, {lon}");
        }
    } else {
        println!("Location: unknown");
    }
    if info.people_status == "available" {
        println!(
            "Tagged people: {}",
            if info.people.is_empty() {
                "none returned".into()
            } else {
                info.people
                    .iter()
                    .map(|p| {
                        format!(
                            "{} ({})",
                            p.name.as_deref().unwrap_or("unnamed face group"),
                            p.id
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
    } else {
        println!("Tagged people: unavailable (Google returned no face-group metadata)");
    }
    if info.faces_status == "available" {
        if info.faces.is_empty() {
            println!("Faces: none detected");
        } else {
            println!("Faces (left to right; use the index with 'google-photos-web-cli tag'):");
            for face in &info.faces {
                let place = match &face.box_px {
                    Some(b) => format!("x={} y={} {}×{} px", b.x, b.y, b.width, b.height),
                    None => format!(
                        "left={:.3} top={:.3} right={:.3} bottom={:.3}",
                        face.bbox.left, face.bbox.top, face.bbox.right, face.bbox.bottom
                    ),
                };
                let who = match &face.person {
                    Some(p) => format!(
                        "{} ({})",
                        p.name.as_deref().unwrap_or("unnamed face group"),
                        p.id
                    ),
                    None => "untagged".into(),
                };
                println!("  [{}] {place}  {who}", face.index);
            }
        }
    } else {
        println!("Faces: unavailable (Google returned no face list)");
    }
    if let Some(description) = &info.description {
        println!("Caption: {description}");
    }
    if !info.albums.is_empty() {
        println!(
            "Albums: {}",
            info.albums
                .iter()
                .map(|a| a.title.as_deref().unwrap_or("untitled"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!("Source: {}", info.source.join(", "));
    println!(
        "Storage quality: {}",
        info.storage_quality.as_deref().unwrap_or("unknown")
    );
    println!("Link: {}", info.url);
}
/// Command path (e.g. "auth export") named on the command line, for error hints.
fn command_path_from_args(args: &[String]) -> String {
    use clap::CommandFactory;
    let mut cmd = Cli::command();
    let mut path = Vec::new();
    for arg in args.iter().skip(1).filter(|a| !a.starts_with('-')) {
        match cmd.find_subcommand(arg).cloned() {
            Some(sub) => {
                path.push(sub.get_name().to_string());
                cmd = sub;
            }
            None => break,
        }
    }
    path.join(" ")
}

fn parse_cli() -> Cli {
    match Cli::try_parse() {
        Ok(cli) => cli,
        Err(err) => {
            let _ = err.print();
            if err.use_stderr() && std::env::args().len() > 1 {
                let args: Vec<String> = std::env::args().collect();
                let path = command_path_from_args(&args);
                let sep = if path.is_empty() { "" } else { " " };
                eprintln!("\nHelp with examples: google-photos-web-cli {path}{sep}--help");
            }
            std::process::exit(err.exit_code());
        }
    }
}

fn main() {
    if let Err(err) = run(parse_cli()) {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    use clap::CommandFactory;

    fn walk(cmd: &clap::Command, path: &str, out: &mut Vec<(String, clap::Command)>) {
        out.push((path.to_string(), cmd.clone()));
        for sub in cmd.get_subcommands().filter(|c| c.get_name() != "help") {
            walk(sub, &format!("{path} {}", sub.get_name()), out);
        }
    }

    #[test]
    fn every_command_documents_every_argument_and_has_examples() {
        let mut root = Cli::command();
        root.build();
        let mut all = Vec::new();
        walk(&root, "google-photos-web-cli", &mut all);
        assert!(all.len() > 10);
        for (path, cmd) in all {
            assert!(cmd.get_about().is_some(), "{path}: missing description");
            for arg in cmd
                .get_arguments()
                .filter(|a| a.get_id() != "help" && a.get_id() != "version")
            {
                let help = arg.get_help().map(|h| h.to_string()).unwrap_or_default();
                assert!(
                    !help.trim().is_empty(),
                    "{path}: argument {:?} has no help text",
                    arg.get_id()
                );
            }
            if path.ends_with("google-photos-web-cli") {
                continue;
            }
            let after = cmd
                .get_after_help()
                .map(|h| h.to_string())
                .unwrap_or_default();
            assert!(after.contains("Examples"), "{path}: no Examples section");
            assert!(
                after.contains("google-photos-web-cli"),
                "{path}: examples must be copy-pasteable invocations"
            );
            assert!(
                cmd.get_long_about().is_none() && cmd.get_after_long_help().is_none(),
                "{path}: -h and --help would differ"
            );
            assert!(
                cmd.get_arguments().all(|a| a.get_long_help().is_none()),
                "{path}: an argument has separate long help"
            );
        }
    }

    #[test]
    fn top_level_help_has_examples_and_lists_every_command_once() {
        let mut root = Cli::command();
        root.build();
        let text = root.render_help().to_string();
        assert_eq!(text, root.render_long_help().to_string());
        assert!(text.contains("EXAMPLES:"));
        assert!(text.contains("FIRST-TIME SETUP"));
        assert!(text.contains("SAFETY:"));
        assert!(
            !text.contains("Commands:\n"),
            "framework command list would duplicate ours"
        );
        assert!(
            text.lines().count() <= 70,
            "top-level help should fit about one screen"
        );
        for sub in root.get_subcommands().filter(|c| c.get_name() != "help") {
            assert!(
                text.contains(sub.get_name()),
                "top-level help omits {}",
                sub.get_name()
            );
            for nested in sub.get_subcommands().filter(|c| c.get_name() != "help") {
                assert!(
                    text.contains(&format!("{} {}", sub.get_name(), nested.get_name())),
                    "top-level help omits {} {}",
                    sub.get_name(),
                    nested.get_name()
                );
            }
        }
    }

    #[test]
    fn error_hint_points_at_the_deepest_named_command() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            command_path_from_args(&a(&["x", "auth", "export"])),
            "auth export"
        );
        assert_eq!(
            command_path_from_args(&a(&["x", "--json", "tag", "ID"])),
            "tag"
        );
        assert_eq!(command_path_from_args(&a(&["x", "bogus"])), "");
    }

    #[test]
    fn one_search_command_accepts_all_search_tactics_and_long_list_controls() {
        let cli = Cli::try_parse_from([
            "google-photos-web-cli",
            "search",
            "birthday",
            "--person",
            "Alice Example",
            "--person",
            "Bob Example",
            "--location",
            "Philly",
            "--tag",
            "cake",
            "--from",
            "2026-01-01",
            "--to",
            "2026-12-31",
            "--all",
            "--limit",
            "500",
            "--max-seconds",
            "300",
            "--json",
        ])
        .unwrap();
        let Command::Search(args) = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(args.person, vec!["Alice Example", "Bob Example"]);
        assert_eq!(args.location.as_deref(), Some("Philly"));
        assert_eq!(args.tag, vec!["cake"]);
        assert!(args.all);
        assert_eq!(args.limit, Some(500));
        assert_eq!(args.max_seconds, 300);
    }
    #[test]
    fn free_form_browser_query_is_a_single_unchanged_argument() {
        let query = "Alice and Bob in Philadelphia October 2026";
        let cli = Cli::try_parse_from(["google-photos-web-cli", "search", query]).unwrap();
        let Command::Search(args) = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(args.query.as_deref(), Some(query));
    }
}
