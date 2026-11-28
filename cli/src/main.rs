use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use google_photos_web_cli::{
    auth::{self, Session},
    client::{self, PhotosClient, SearchOptions, SearchSort},
    dates, keepalive,
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
    upload FILE...          Upload photos/videos from this computer
    album-create NAME       Create an album (or reuse the one with that name)
    album-add PHOTO_ID...   Add photos to an existing album
    album-rename NEW_TITLE  Rename an album (--album TITLE or --album-id ID)
    tag PHOTO_ID            Tag a detected face with an existing person
    set-date PHOTO_ID...    Change when photos were taken (or a whole --album)

EXAMPLES:
  google-photos-web-cli search "birthday cake"
  google-photos-web-cli search "Paris October 2026" --limit 50
  google-photos-web-cli search --person "Jane Doe" --from 2026-10-01 --to 2026-10-31
  google-photos-web-cli search beach --all --json > results.json
  google-photos-web-cli albums --json
  google-photos-web-cli album-add AF1QipExamplePhotoId --album "Artwork" --dry-run
  google-photos-web-cli upload IMG_0001.jpg clip.mp4 --album "Trip" --create-album
  google-photos-web-cli album-create "Trip 2026" --json
  google-photos-web-cli album-rename --album "Trip" "Trip 2026" --dry-run
  google-photos-web-cli people --name Jane
  google-photos-web-cli inspect AF1QipExamplePhotoId --json
  google-photos-web-cli download AF1QipExamplePhotoId --dir ./photos
  google-photos-web-cli tag AF1QipExamplePhotoId --face 0 --person "Jane Doe" --dry-run
  google-photos-web-cli set-date --album "Trip" --date 2026-11-22T10:00 --keep-spacing --dry-run

MORE HELP:  google-photos-web-cli <command> --help   (or: help <command>, e.g. help search)
OUTPUT:     readable text by default; --json on search, people, inspect, download,
            albums, album-add/-create/-rename, upload, tag, set-date, auth status.
SAFETY:     Add --dry-run to preview upload, album-create/-add/-rename, tag, set-date.
            Album additions require --yes. Uploads skip files already in the library.
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
        after_help = "Examples:\n  google-photos-web-cli albums\n  google-photos-web-cli albums --json\n\nRead-only. Use an ID with 'album-add --album-id'.\nJSON adds alt_id when Google gives an album a second ID (as in its /album/ID link)."
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
    /// Create an album by exact name, or reuse the one that already has it; optionally add photos
    #[command(after_help = r#"Examples:
  google-photos-web-cli album-create "Trip 2026" --dry-run
  google-photos-web-cli album-create "Trip 2026"
  google-photos-web-cli album-create "Trip 2026" PHOTO_ID_1 PHOTO_ID_2 --json

WRITES to your library unless --dry-run (this CLI has no undo).
Never makes a duplicate: if exactly one album already has this exact,
case-sensitive title it is reused; if several do, nothing changes.
Photo IDs come from 'search' or 'upload'. JSON reports album_id, title,
created (true/false) and the added photo_ids."#)]
    AlbumCreate(AlbumCreateArgs),
    /// Rename an album, like editing its title on photos.google.com (writes to your library)
    #[command(after_help = r#"Examples:
  google-photos-web-cli album-rename --album "Family Photos 2026" "Family Photos 2025" --dry-run
  google-photos-web-cli album-rename --album-id ALBUM_ID "Family Photos 2025"
  google-photos-web-cli album-rename --album-id ALBUM_ID "Trip" --allow-duplicate --json

WRITES to your library unless --dry-run (this CLI has no undo; rename back to
the old title printed here). Choose the album with --album (exact,
case-sensitive title; refused if several albums have it) or --album-id (an ID
from 'albums', or the ID in a photos.google.com/album/ID link).
Refused if another album already has exactly NEW_TITLE, unless
--allow-duplicate. An album that already has NEW_TITLE is left unchanged.
After renaming, the album list is read again and the new title checked;
exit 1 if it did not change. Members of a shared album see the new title.
JSON reports status (renamed, unchanged, dry_run), album_id, old_title, title."#)]
    AlbumRename(AlbumRenameArgs),
    /// Upload photos and videos from this computer to your library (writes to your library)
    #[command(after_help = r#"Examples:
  google-photos-web-cli upload IMG_0001.jpg --dry-run
  google-photos-web-cli upload IMG_0001.jpg IMG_0002.heic clip.mp4
  google-photos-web-cli upload ./trip/*.jpg --album "Trip 2026" --create-album --json
  google-photos-web-cli upload scan.png --album-id ALBUM_ID

WRITES to your library unless --dry-run (this CLI has no undo). Works like
"Import photos from your computer" on photos.google.com: the same web session,
no API key. Accepts JPEG, PNG, GIF, WebP, HEIC/AVIF, BMP, TIFF/raw, MP4, MOV,
AVI, MKV/WebM, WMV and MPEG (checked by content, not extension).

Safe to re-run: each file's SHA-1 is checked against your library first and a
file already there is not uploaded again; it is reported as a duplicate with
the existing media key. Google may also return an existing item at commit.
Each file reports its media key (the PHOTO_ID other commands take) and
duplicate true/false; one failed file does not stop the others, but the exit
code is 1. Rate limits or an expired session stop the remaining files.

Dates: Google uses the photo's own capture date (EXIF); for a file without
one it uses the file's modified time. Storage quality follows your account's
Google Photos backup setting.

--album/--album-id add every uploaded or duplicate file to that album after the
uploads (exact, case-sensitive title). --create-album creates the --album
title if no album has it yet (never a duplicate). --dry-run hashes the files
and checks for duplicates and the album, but uploads and creates nothing."#)]
    Upload(UploadArgs),
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
    /// Change when photos were taken, like Info > Edit date & time (writes to your library)
    #[command(
        after_help = r#"Examples (WRITES to your library unless --dry-run; this CLI has no undo):
  google-photos-web-cli set-date PHOTO_ID --date 2026-11-22T10:00 --dry-run
  google-photos-web-cli set-date PHOTO_ID_1 PHOTO_ID_2 --date "2026-11-22 10:00" --tz America/New_York
  google-photos-web-cli set-date PHOTO_ID --date 2025-11-29T18:54:30-05:00 --json
  google-photos-web-cli set-date --album "Trip 2026" --date 2026-11-22T10:00 --keep-spacing
  google-photos-web-cli set-date --album-id ALBUM_ID --shift +1y --dry-run
  google-photos-web-cli set-date PHOTO_ID --shift -2h30m

Choose photos (PHOTO_ID... from 'search'/'upload', or every item of one album
with --album/--album-id) and exactly one of --date or --shift.

--date YYYY-MM-DD[THH:MM[:SS]] gives every photo that wall-clock time (a date
alone means 00:00). Time zone: an offset written in the date (-05:00, Z), else
--offset, else --tz (daylight saving applied for that date), else each photo
keeps its current offset. --keep-spacing moves the earliest photo to --date and
the others by the same amount, like editing several photos at once in the web
app. --shift moves each photo's own clock: +1y, -1y2mo, +3d, -2h30m, +90s
(units y, mo, w, d, h, m, s); offsets stay as they are.

Google stores whole seconds. Photos already at the new time are skipped.
Every change is re-read afterwards: each photo is listed with its old time, new
time and status (changed, unchanged, would_change, failed); any failure exits 1.
Undo by setting the old time printed here (it includes the offset)."#
    )]
    SetDate(SetDateArgs),
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

#[derive(Args)]
struct AlbumCreateArgs {
    /// Exact, case-sensitive album title; reused if exactly one album has it
    name: String,
    /// Optional photo IDs from 'search' or 'upload' to add to the album
    photo_ids: Vec<String>,
    /// Show whether the album exists and would be created; change nothing
    #[arg(long)]
    dry_run: bool,
    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
#[command(group = clap::ArgGroup::new("album_target").required(true).args(["album", "album_id"]))]
struct AlbumRenameArgs {
    /// The album's new title (used exactly as given)
    new_title: String,
    /// Exact, case-sensitive current title of a uniquely named album
    #[arg(long)]
    album: Option<String>,
    /// Album ID from 'albums' (or from a photos.google.com/album/ID link)
    #[arg(long)]
    album_id: Option<String>,
    /// Rename even if another album already has NEW_TITLE
    #[arg(long)]
    allow_duplicate: bool,
    /// Show the album and the old and new titles; change nothing
    #[arg(long)]
    dry_run: bool,
    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,
}

fn run_album_rename(args: AlbumRenameArgs) -> Result<()> {
    use google_photos_web_cli::protocol::{plan_album_rename, AlbumTarget};
    if args.new_title.trim().is_empty() {
        bail!("The new album title cannot be empty");
    }
    if let Some(id) = &args.album_id {
        client::check_id(id).context("Invalid album ID")?;
    }
    let target = match (&args.album_id, &args.album) {
        (Some(id), _) => AlbumTarget::Id(id),
        (None, Some(title)) => AlbumTarget::Title(title),
        (None, None) => bail!("Choose --album or --album-id"),
    };
    let new_title = args.new_title.as_str();
    let (album, status) = with_saved(|client| {
        let album = plan_album_rename(&client.albums()?, target, new_title, args.allow_duplicate)?;
        if album.title.as_deref() == Some(new_title) {
            return Ok((album, "unchanged"));
        }
        if args.dry_run {
            return Ok((album, "dry_run"));
        }
        client.rename_album(&album.id, new_title)?;
        let now = client
            .albums()?
            .into_iter()
            .find(|a| a.id == album.id)
            .with_context(|| {
                format!(
                    "Album {} is missing from 'albums' after the rename; check it in the browser",
                    album.id
                )
            })?;
        if now.title.as_deref() != Some(new_title) {
            bail!(
                "Google Photos accepted the request, but album {} is still titled {:?}, not {new_title:?}",
                album.id,
                now.title.as_deref().unwrap_or("(untitled)")
            );
        }
        Ok((album, "renamed"))
    })?;
    let old = album.title.as_deref();
    if args.json {
        return print_json(&serde_json::json!({
            "status": status,
            "album_id": album.id,
            "alt_id": album.alt_id,
            "old_title": old,
            "title": new_title,
        }));
    }
    let old = old.map_or("(untitled)".to_owned(), |t| format!("{t:?}"));
    match status {
        "unchanged" => println!(
            "Album {} is already titled {new_title:?}; nothing changed.",
            album.id
        ),
        "dry_run" => println!(
            "Dry run: would rename album {} from {old} to {new_title:?}.",
            album.id
        ),
        _ => println!(
            "Renamed album {} from {old} to {new_title:?} (checked in the album list).",
            album.id
        ),
    }
    Ok(())
}

fn run_album_create(mut args: AlbumCreateArgs) -> Result<()> {
    if args.name.trim().is_empty() {
        bail!("Album title cannot be empty");
    }
    for id in &args.photo_ids {
        client::check_id(id)?;
    }
    let mut seen = std::collections::HashSet::new();
    args.photo_ids.retain(|id| seen.insert(id.clone()));
    let (album_id, created) = with_saved(|client| {
        if args.dry_run {
            return Ok((client.find_album(&args.name)?, false));
        }
        let (id, created) = client.find_or_create_album(&args.name)?;
        if !args.photo_ids.is_empty() {
            client.add_to_album(&id, &args.photo_ids)?;
        }
        Ok((Some(id), created))
    })?;
    let status = match (args.dry_run, &album_id, created) {
        (true, Some(_), _) => "dry_run_exists",
        (true, None, _) => "dry_run_would_create",
        (false, _, true) => "created",
        (false, _, false) => "exists",
    };
    if args.json {
        return print_json(&serde_json::json!({
            "status": status,
            "album_id": album_id,
            "title": args.name,
            "created": created,
            "photo_ids": args.photo_ids,
        }));
    }
    match (status, album_id) {
        ("dry_run_would_create", _) => println!("Dry run: would create album {:?}.", args.name),
        (_, Some(id)) if status == "dry_run_exists" => {
            println!(
                "Dry run: album {:?} already exists ({id}); it would be reused.",
                args.name
            )
        }
        (_, Some(id)) => println!(
            "{} album {:?}: {id}",
            if created {
                "Created"
            } else {
                "Reusing existing"
            },
            args.name
        ),
        _ => {}
    }
    if !args.photo_ids.is_empty() {
        if args.dry_run {
            println!("Would add {} photo(s).", args.photo_ids.len());
        } else {
            println!(
                "Google Photos confirmed {} photo(s) in the album.",
                args.photo_ids.len()
            );
        }
    }
    Ok(())
}

#[derive(Args)]
struct UploadArgs {
    /// Photo or video files to upload
    #[arg(required = true, num_args = 1.., value_name = "FILE")]
    files: Vec<PathBuf>,
    /// Add the uploads to the album with this exact, case-sensitive title
    #[arg(long, conflicts_with = "album_id")]
    album: Option<String>,
    /// Add the uploads to this album ID from 'albums'
    #[arg(long)]
    album_id: Option<String>,
    /// Create the --album title if no album has it yet (never a duplicate)
    #[arg(long, requires = "album")]
    create_album: bool,
    /// Hash files and check duplicates and the album; upload and create nothing
    #[arg(long)]
    dry_run: bool,
    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,
}

#[derive(serde::Serialize)]
struct UploadReport {
    path: PathBuf,
    filename: Option<String>,
    status: &'static str,
    media_key: Option<String>,
    duplicate: Option<bool>,
    media_type: Option<&'static str>,
    bytes: Option<u64>,
    taken_at: Option<String>,
    uploaded_at: Option<String>,
    url: Option<String>,
    error: Option<String>,
}

impl UploadReport {
    fn failed(path: PathBuf, error: &anyhow::Error) -> Self {
        Self {
            path,
            filename: None,
            status: "failed",
            media_key: None,
            duplicate: None,
            media_type: None,
            bytes: None,
            taken_at: None,
            uploaded_at: None,
            url: None,
            error: Some(format!("{error:#}")),
        }
    }
}

fn upload_one(
    client: &mut PhotosClient,
    file: &google_photos_web_cli::upload::LocalFile,
    dry_run: bool,
) -> Result<UploadReport> {
    let started = chrono::Utc::now().timestamp_millis();
    let existing = client.find_by_hash(&file.sha1)?.into_iter().next();
    let (status, stored, duplicate) = match existing {
        Some(found) => ("duplicate", Some(found), Some(true)),
        None if dry_run => ("would_upload", None, Some(false)),
        None => {
            if !dry_run {
                eprintln!("Uploading {} ({} bytes)…", file.path.display(), file.size);
            }
            let token = client.upload_bytes(file)?;
            let stored = client.commit_upload(&token, file)?;
            // Google returns the existing item when it already had the file.
            let old = stored
                .uploaded_ms
                .is_some_and(|ms| ms < started - 10 * 60 * 1000);
            (
                if old { "duplicate" } else { "uploaded" },
                Some(stored),
                Some(old),
            )
        }
    };
    Ok(UploadReport {
        path: file.path.clone(),
        filename: Some(file.name.clone()),
        status,
        url: stored
            .as_ref()
            .map(|s| format!("https://photos.google.com/photo/{}", s.media_key)),
        media_key: stored.as_ref().map(|s| s.media_key.clone()),
        duplicate,
        media_type: Some(file.media_type),
        bytes: Some(file.size),
        taken_at: stored.as_ref().and_then(|s| s.taken_at.clone()),
        uploaded_at: stored.and_then(|s| s.uploaded_at),
        error: None,
    })
}

fn run_upload(args: UploadArgs) -> Result<()> {
    if let Some(id) = &args.album_id {
        client::check_id(id).context("Invalid album ID")?;
    }
    if args.album.as_deref().is_some_and(|t| t.trim().is_empty()) {
        bail!("Album title cannot be empty");
    }
    let mut seen = std::collections::HashSet::new();
    let mut prepared = Vec::new();
    let mut reports = Vec::new();
    for path in &args.files {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
        if !seen.insert(key) {
            continue;
        }
        match google_photos_web_cli::upload::prepare(path) {
            Ok(file) => prepared.push(file),
            Err(e) => reports.push(UploadReport::failed(path.clone(), &e)),
        }
    }
    if prepared.is_empty() {
        for r in &reports {
            eprintln!("{}: {}", r.path.display(), r.error.as_deref().unwrap_or(""));
        }
        bail!("No uploadable files; nothing was sent");
    }
    let (album_id, album_created, album_error) = with_saved(|client| {
        // Resolve the album before uploading so a bad name fails before any write.
        let mut album_id = match (&args.album_id, &args.album) {
            (Some(id), _) => Some(id.clone()),
            (None, Some(title)) => match client.find_album(title)? {
                Some(id) => Some(id),
                None if args.create_album => None,
                None => bail!("No album titled {title:?} in the album picker. Add --create-album to create it, or run 'albums'"),
            },
            (None, None) => None,
        };
        let mut stop: Option<String> = None;
        for file in &prepared {
            if let Some(reason) = &stop {
                reports.push(UploadReport::failed(
                    file.path.clone(),
                    &anyhow::anyhow!("not attempted: {reason}"),
                ));
                continue;
            }
            match upload_one(client, file, args.dry_run) {
                Ok(report) => reports.push(report),
                Err(e) => {
                    if e.is::<SessionExpired>()
                        || e.is::<google_photos_web_cli::protocol::RateLimit>()
                    {
                        stop = Some(format!("{e}"));
                    }
                    reports.push(UploadReport::failed(file.path.clone(), &e));
                }
            }
        }
        let keys: Vec<String> = reports.iter().filter_map(|r| r.media_key.clone()).collect();
        let mut created = false;
        // An album failure must not hide which files were uploaded: report it after the files.
        let mut album_error = None;
        if !args.dry_run && !keys.is_empty() {
            let add = || -> Result<()> {
                if let (None, Some(title)) = (&album_id, &args.album) {
                    let (id, made) = client.find_or_create_album(title)?;
                    album_id = Some(id);
                    created = made;
                }
                if let Some(id) = &album_id {
                    let mut unique = std::collections::HashSet::new();
                    let keys: Vec<String> = keys
                        .into_iter()
                        .filter(|k| unique.insert(k.clone()))
                        .collect();
                    client.add_to_album(id, &keys)?;
                }
                Ok(())
            };
            album_error = add().err().map(|e| format!("{e:#}"));
        }
        Ok((album_id, created, album_error))
    })?;
    let failed = reports.iter().filter(|r| r.status == "failed").count();
    let album = (args.album.is_some() || args.album_id.is_some()).then(|| {
        serde_json::json!({
            "id": album_id,
            "title": args.album,
            "created": album_created,
            "would_create": args.dry_run && album_id.is_none(),
            "error": album_error,
        })
    });
    if args.json {
        print_json(&serde_json::json!({
            "status": if args.dry_run { "dry_run" } else if failed > 0 { "partial" } else { "done" },
            "album": album,
            "files": reports,
        }))?;
    } else {
        for r in &reports {
            match r.status {
                "failed" => println!(
                    "FAILED     {}  {}",
                    r.path.display(),
                    r.error.as_deref().unwrap_or("")
                ),
                status => println!(
                    "{:<10} {}  {}  {}",
                    status,
                    r.media_key.as_deref().unwrap_or("-"),
                    r.taken_at.as_deref().unwrap_or("date pending"),
                    r.path.display()
                ),
            }
        }
        match (&album_id, &args.album) {
            (None, Some(title)) if args.dry_run => {
                println!("Dry run: would create album {title:?}.")
            }
            _ if album_error.is_some() => {}
            (Some(id), _) if args.dry_run => println!("Dry run: would add to album {id}."),
            (Some(id), title) => println!(
                "{} album {}{id}.",
                if album_created {
                    "Added to new"
                } else {
                    "Added to"
                },
                title
                    .as_deref()
                    .map(|t| format!("{t:?} "))
                    .unwrap_or_default()
            ),
            _ => {}
        }
    }
    if let Some(error) = album_error {
        bail!("Files were processed, but adding them to the album failed: {error}. Re-run the same command (duplicates are not uploaded again) or use 'album-add'");
    }
    if failed > 0 {
        bail!(
            "{failed} of {} file(s) failed; the others were processed",
            reports.len()
        );
    }
    Ok(())
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
#[command(group = clap::ArgGroup::new("change").required(true).args(["date", "shift"]))]
struct SetDateArgs {
    /// Photo IDs from 'search' or 'upload' (or use --album/--album-id)
    #[arg(value_name = "PHOTO_ID", required_unless_present_any = ["album", "album_id"], conflicts_with_all = ["album", "album_id"])]
    photo_ids: Vec<String>,
    /// Change every item in the album with this exact, case-sensitive title
    #[arg(long, conflicts_with = "album_id")]
    album: Option<String>,
    /// Change every item in this album ID from 'albums'
    #[arg(long)]
    album_id: Option<String>,
    /// New time: YYYY-MM-DD[THH:MM[:SS]], optionally ending in Z or an offset like -05:00
    #[arg(long, value_name = "DATETIME")]
    date: Option<String>,
    /// IANA time zone for --date, e.g. America/New_York (daylight saving applied)
    #[arg(long, value_name = "ZONE", conflicts_with = "offset")]
    tz: Option<String>,
    /// Fixed UTC offset for --date, e.g. -05:00 or +0530
    #[arg(long, value_name = "OFFSET", allow_hyphen_values = true)]
    offset: Option<String>,
    /// Move the earliest photo to --date and the others by the same amount
    #[arg(long, requires = "date")]
    keep_spacing: bool,
    /// Move each photo's time by this much instead: +1y, -1y2mo, +3d, -2h30m (y, mo, w, d, h, m, s)
    #[arg(long, value_name = "AMOUNT", allow_hyphen_values = true, conflicts_with_all = ["tz", "offset", "keep_spacing"])]
    shift: Option<String>,
    /// Show old and new times without changing anything
    #[arg(long)]
    dry_run: bool,
    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,
}

#[derive(serde::Serialize)]
struct DateReport {
    id: String,
    filename: Option<String>,
    old_taken_at: Option<String>,
    new_taken_at: Option<String>,
    status: &'static str,
    error: Option<String>,
}

fn date_plan(args: &SetDateArgs) -> Result<(dates::Plan, dates::Zone)> {
    if let Some(shift) = &args.shift {
        return Ok((
            dates::Plan::Shift(dates::parse_shift(shift)?),
            dates::Zone::Keep,
        ));
    }
    let (local, written) =
        dates::parse_date(args.date.as_deref().context("Choose --date or --shift")?)?;
    let zone = match (written, &args.offset, &args.tz) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            bail!("--date already has an offset; drop --offset/--tz or the offset in --date")
        }
        (Some(offset), _, _) => dates::Zone::Fixed(offset),
        (None, Some(offset), _) => dates::Zone::Fixed(dates::parse_offset(offset)?),
        (None, None, Some(tz)) => dates::Zone::Named(dates::parse_zone(tz)?),
        (None, None, None) => dates::Zone::Keep,
    };
    let plan = if args.keep_spacing {
        dates::Plan::KeepSpacing(local)
    } else {
        dates::Plan::Set(local)
    };
    Ok((plan, zone))
}

fn run_set_date(mut args: SetDateArgs) -> Result<()> {
    for id in &args.photo_ids {
        client::check_id(id)?;
    }
    if let Some(id) = &args.album_id {
        client::check_id(id).context("Invalid album ID")?;
    }
    if args.album.as_deref().is_some_and(|t| t.trim().is_empty()) {
        bail!("Album title cannot be empty");
    }
    let (plan, zone) = date_plan(&args)?;
    let mut seen = std::collections::HashSet::new();
    args.photo_ids.retain(|id| seen.insert(id.clone()));

    let reports = with_saved(|client| {
        let ids = match (&args.album_id, &args.album) {
            (Some(id), _) => client.album_item_ids(id)?,
            (None, Some(title)) => {
                let id = client.find_album(title)?.with_context(|| {
                    format!("No album titled {title:?} in the album picker. Run 'albums'")
                })?;
                client.album_item_ids(&id)?
            }
            (None, None) => args.photo_ids.clone(),
        };
        if ids.is_empty() {
            bail!("The album is empty; nothing to change");
        }
        let mut reports = Vec::new();
        let mut items = Vec::new();
        for (id, read) in ids.iter().zip(client.dated_items(&ids)?) {
            match read {
                Ok(item) => items.push(item),
                Err(e) => reports.push(DateReport {
                    id: id.clone(),
                    filename: None,
                    old_taken_at: None,
                    new_taken_at: None,
                    status: "failed",
                    error: Some(format!("{e:#}")),
                }),
            }
        }
        // An album item ID and a library ID can name the same photo: change it once.
        let mut keys = std::collections::HashSet::new();
        items.retain(|item| keys.insert(item.dedup_key.clone()));
        // Plan before writing anything so a bad date (e.g. in a DST gap) changes nothing.
        let targets = dates::plan(&items, &plan, zone)?;
        let changes: Vec<(&dates::DatedItem, dates::Target)> = items
            .iter()
            .zip(targets.iter().copied())
            .filter(|(item, t)| !t.matches(item))
            .collect();
        let mut after = std::collections::HashMap::new();
        if !args.dry_run && !changes.is_empty() {
            client.set_dates(&changes)?;
            let changed: Vec<String> = changes.iter().map(|(i, _)| i.id.clone()).collect();
            for (id, read) in changed.iter().zip(client.dated_items(&changed)?) {
                after.insert(id.clone(), read);
            }
        }
        for (item, target) in items.iter().zip(targets) {
            let (status, error) = if target.matches(item) {
                ("unchanged", None)
            } else if args.dry_run {
                ("would_change", None)
            } else {
                match after.get(&item.id) {
                    Some(Ok(now)) if target.matches(now) => ("changed", None),
                    Some(Ok(now)) => (
                        "failed",
                        Some(format!("Google still reports {}", now.taken_at())),
                    ),
                    Some(Err(e)) => ("failed", Some(format!("could not re-read: {e:#}"))),
                    None => ("failed", Some("not re-read".to_owned())),
                }
            };
            reports.push(DateReport {
                id: item.id.clone(),
                filename: item.filename.clone(),
                old_taken_at: Some(item.taken_at()),
                new_taken_at: Some(target.taken_at()),
                status,
                error,
            });
        }
        Ok(reports)
    })?;
    let failed = reports.iter().filter(|r| r.status == "failed").count();
    if args.json {
        print_json(&serde_json::json!({
            "status": if args.dry_run { "dry_run" } else if failed > 0 { "partial" } else { "done" },
            "items": reports,
        }))?;
    } else {
        for r in &reports {
            println!(
                "{:<12} {}  {}  {} -> {}{}",
                r.status,
                r.id,
                r.filename.as_deref().unwrap_or("-"),
                r.old_taken_at.as_deref().unwrap_or("?"),
                r.new_taken_at.as_deref().unwrap_or("?"),
                r.error
                    .as_deref()
                    .map(|e| format!("  ({e})"))
                    .unwrap_or_default()
            );
        }
        if args.dry_run {
            println!("Dry run: nothing was changed.");
        }
    }
    if failed > 0 {
        bail!("{failed} of {} photo(s) failed", reports.len());
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
        Command::AlbumCreate(args) => run_album_create(args)?,
        Command::AlbumRename(args) => run_album_rename(args)?,
        Command::Upload(args) => run_upload(args)?,
        Command::SetDate(args) => run_set_date(args)?,
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

    #[test]
    fn album_rename_takes_one_target_and_the_new_title() {
        let cli = Cli::try_parse_from([
            "google-photos-web-cli",
            "album-rename",
            "--album-id",
            "AF1QipSyntheticAlbum",
            "Family Photos 2025",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        let Command::AlbumRename(args) = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(args.album_id.as_deref(), Some("AF1QipSyntheticAlbum"));
        assert_eq!(args.new_title, "Family Photos 2025");
        assert!(args.dry_run && args.json && !args.allow_duplicate && args.album.is_none());
        let cli = Cli::try_parse_from([
            "google-photos-web-cli",
            "album-rename",
            "--album",
            "Old",
            "New",
            "--allow-duplicate",
        ])
        .unwrap();
        let Command::AlbumRename(args) = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(args.album.as_deref(), Some("Old"));
        assert!(args.allow_duplicate);
        for bad in [
            vec!["album-rename", "New"],
            vec!["album-rename", "--album", "Old"],
            vec![
                "album-rename",
                "--album",
                "Old",
                "--album-id",
                "AF1QipX",
                "New",
            ],
            vec!["album-rename", "--album", "Old", "New", "Extra"],
        ] {
            let argv = std::iter::once("google-photos-web-cli").chain(bad.clone());
            assert!(Cli::try_parse_from(argv).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn set_date_accepts_photos_or_an_album_and_hyphenated_values() {
        let cli = Cli::try_parse_from([
            "google-photos-web-cli",
            "set-date",
            "--album",
            "Family Photos 2026",
            "--date",
            "2026-11-22T10:00",
            "--offset",
            "-05:00",
            "--keep-spacing",
            "--dry-run",
            "--json",
        ])
        .unwrap();
        let Command::SetDate(args) = cli.command else {
            panic!("wrong command")
        };
        assert_eq!(args.album.as_deref(), Some("Family Photos 2026"));
        assert_eq!(args.offset.as_deref(), Some("-05:00"));
        assert!(args.keep_spacing && args.dry_run && args.json && args.photo_ids.is_empty());
        let (plan, zone) = date_plan(&args).unwrap();
        assert!(matches!(plan, dates::Plan::KeepSpacing(_)));
        assert!(matches!(zone, dates::Zone::Fixed(o) if o.local_minus_utc() == -18000));

        let cli = Cli::try_parse_from([
            "google-photos-web-cli",
            "set-date",
            "AF1QipSyntheticPhoto",
            "--shift",
            "-2h30m",
        ])
        .unwrap();
        let Command::SetDate(args) = cli.command else {
            panic!("wrong command")
        };
        let (plan, _) = date_plan(&args).unwrap();
        assert!(matches!(plan, dates::Plan::Shift(s) if s.seconds == -9000));

        let cli = Cli::try_parse_from([
            "google-photos-web-cli",
            "set-date",
            "AF1QipSyntheticPhoto",
            "--date",
            "2025-11-29T18:54:30-05:00",
            "--tz",
            "America/New_York",
        ])
        .unwrap();
        let Command::SetDate(args) = cli.command else {
            panic!("wrong command")
        };
        assert!(date_plan(&args).is_err(), "two time zones for one date");
    }
}
