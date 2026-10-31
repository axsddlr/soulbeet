//! Reads tracks back out of soulbeet's per-folder beets libraries.

use super::runner::{run_beet, RunEnd, RunLimits, RunSpec, Stream};
use super::BeetsSettings;
use shared::library::{DuplicateGroup, DuplicateReport, LibraryTrack};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::Path;
use tracing::{info, warn};

/// Starts every `beet ls` row, so text a plugin prints before it (a prompt
/// without a newline) cannot be read as part of the row.
const ROW_MARKER: &str = "@@sb@@";
const FIELD_SEP: &str = "|||";
const LS_FORMAT: &str = "@@sb@@$path|||$artist|||$title|||$album|||$albumartist";

/// Optional directory that holds every library db instead of the music
/// folders, since SQLite locking is unreliable on SMB/NFS shares.
const LIBRARY_DIR_ENV: &str = "BEETS_LIBRARY_DIR";

pub(crate) fn library_db(root: &Path) -> std::path::PathBuf {
    let dir = std::env::var_os(LIBRARY_DIR_ENV)
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from);
    let db = library_db_in(root, dir.as_deref());
    if dir.is_some() {
        prepare_db(root, &db);
    }
    db
}

/// The library db for `root`: `<root>/.beets_library.db`, or with `dir` set a
/// file in `dir` named after `root` (sanitized last component plus a stable
/// hash of the full path, so two folders never share a db).
pub(crate) fn library_db_in(root: &Path, dir: Option<&Path>) -> std::path::PathBuf {
    let Some(dir) = dir else {
        return root.join(".beets_library.db");
    };
    // FNV-1a 64: stable across processes and releases, unlike DefaultHasher.
    let hash = root
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    let name: String = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = if name.is_empty() { "root" } else { &name };
    dir.join(format!("{name}-{hash:016x}.db"))
}

type Linker = fn(&Path, &Path) -> std::io::Result<()>;

fn link_file(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::hard_link(from, to)
}

fn migrate_with(_legacy: &Path, _new: &Path, _link: Linker) -> bool {
    false
}

fn choose_db(_root: &Path, db: &Path, _link: Linker) -> std::path::PathBuf {
    db.to_path_buf()
}

/// Creates the db's directory and copies the legacy db over once.
fn prepare_db(root: &Path, db: &Path) {
    if let Some(parent) = db.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!("Could not create {:?}: {e}", parent);
        }
    }
    migrate_legacy_db(&root.join(".beets_library.db"), db);
}

/// Copies `legacy` to `new` when only the legacy file exists. Copies rather
/// than moves so a rollback still finds it, and never overwrites `new`.
///
/// The copy goes to a temp sibling and is then hard-linked into place, so a
/// concurrent caller sees either no `new` or a complete one, never a partial
/// file, and an existing `new` is never replaced.
pub(crate) fn migrate_legacy_db(legacy: &Path, new: &Path) -> bool {
    if new.exists() || !legacy.is_file() {
        return false;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut tmp_name = new.as_os_str().to_owned();
    tmp_name.push(format!(".tmp-{}-{nanos}", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp_name);

    let result = std::fs::copy(legacy, &tmp).and_then(|_| std::fs::hard_link(&tmp, new));
    let _ = std::fs::remove_file(&tmp);
    match result {
        Ok(()) => {
            info!("Copied beets library {:?} to {:?}", legacy, new);
            true
        }
        // Someone else migrated first; theirs stays.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => {
            warn!("Could not copy beets library {:?} to {:?}: {e}", legacy, new);
            false
        }
    }
}

fn parse_row(row: &str, library: &str) -> Option<LibraryTrack> {
    let mut fields = row.split(FIELD_SEP);
    let mut next = || fields.next().map(str::to_string);
    Some(LibraryTrack {
        path: next()?,
        artist: next()?,
        title: next()?,
        album: next()?,
        album_artist: next()?,
        library_path: library.to_string(),
    })
}

/// Every track in the library under `root`. A folder without a library db has
/// no tracks.
pub(crate) async fn query_library(
    s: &BeetsSettings,
    root: &Path,
) -> Result<Vec<LibraryTrack>, String> {
    let db = library_db(root);
    if !tokio::fs::try_exists(&db).await.unwrap_or(false) {
        return Ok(Vec::new());
    }
    let library = db.to_string_lossy().into_owned();

    let mut retried = false;
    loop {
        let mut rows = Vec::new();
        let mut locked = false;
        let mut on_line = |stream: Stream, line: &str| match stream {
            Stream::Stdout => {
                if let Some((_, row)) = line.rsplit_once(ROW_MARKER) {
                    rows.push(row.to_string());
                }
            }
            Stream::Stderr => locked |= line.contains("database is locked"),
        };
        // beets 2.10+ stores paths relative to the library directory, so -d
        // must name it or they resolve under the config's `directory`.
        let args: Vec<OsString> = vec![
            "-c".into(),
            s.config.clone().into(),
            "-l".into(),
            db.clone().into(),
            "-d".into(),
            root.into(),
            "ls".into(),
            "-f".into(),
            LS_FORMAT.into(),
        ];
        let spec = RunSpec {
            program: &s.beet,
            args,
            limits: RunLimits::LIBRARY_LS,
        };
        let outcome = run_beet(spec, &mut on_line).await;

        if locked && !retried {
            retried = true;
            continue;
        }
        return match outcome.end {
            RunEnd::Exited(status) if status.success() => Ok(rows
                .iter()
                .filter_map(|row| parse_row(row, &library))
                .collect()),
            RunEnd::Exited(status) => Err(format!("beet ls failed ({status}): {}", outcome.tail)),
            RunEnd::Idle | RunEnd::Total => Err("beet ls timed out".to_string()),
            RunEnd::SpawnFailed(e) => Err(format!("Could not start beets: {e}")),
        };
    }
}

/// Tracks with the same artist and title in more than one of `roots`.
pub(crate) async fn find_duplicates_across_libraries(
    s: &BeetsSettings,
    roots: &[&Path],
) -> Result<DuplicateReport, String> {
    let mut all_tracks: Vec<LibraryTrack> = Vec::new();
    let mut libraries_scanned: Vec<String> = Vec::new();

    for root in roots {
        info!("Scanning library: {:?}", library_db(root));
        match query_library(s, root).await {
            Ok(tracks) => {
                info!("Found {} tracks in {:?}", tracks.len(), root);
                all_tracks.extend(tracks);
                libraries_scanned.push(root.to_string_lossy().to_string());
            }
            Err(e) => {
                warn!("Failed to scan library {:?}: {}", root, e);
            }
        }
    }

    // Grouped by (artist, title), compared case-insensitively.
    let mut track_groups: HashMap<(String, String), Vec<LibraryTrack>> = HashMap::new();
    for track in all_tracks {
        let key = (
            track.artist.to_lowercase().trim().to_string(),
            track.title.to_lowercase().trim().to_string(),
        );
        track_groups.entry(key).or_default().push(track);
    }

    let duplicates: Vec<DuplicateGroup> = track_groups
        .into_iter()
        .filter(|(_, tracks)| {
            let unique_libs: HashSet<_> = tracks.iter().map(|t| &t.library_path).collect();
            unique_libs.len() > 1
        })
        .map(|((artist, title), tracks)| {
            // Shown with the first track's original case.
            let display_artist = tracks.first().map(|t| t.artist.clone()).unwrap_or(artist);
            let display_title = tracks.first().map(|t| t.title.clone()).unwrap_or(title);
            DuplicateGroup {
                artist: display_artist,
                title: display_title,
                tracks,
            }
        })
        .collect();

    let total_duplicate_tracks: usize = duplicates.iter().map(|g| g.tracks.len()).sum();

    Ok(DuplicateReport {
        duplicates,
        total_duplicate_tracks,
        libraries_scanned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_dir_gives_the_legacy_path() {
        assert_eq!(
            library_db_in(Path::new("/music/a"), None),
            Path::new("/music/a/.beets_library.db")
        );
    }

    #[test]
    fn set_dir_puts_the_db_under_it() {
        let db = library_db_in(Path::new("/music/a"), Some(Path::new("/data/beets")));
        assert_eq!(db.parent(), Some(Path::new("/data/beets")));
        assert!(db.extension().is_some_and(|e| e == "db"));
        assert!(db.file_name().unwrap().to_string_lossy().starts_with("a-"));
    }

    #[test]
    fn different_folders_get_different_names() {
        let d = Some(Path::new("/data/beets"));
        // Same last component, different parents.
        assert_ne!(
            library_db_in(Path::new("/music/a/Music"), d),
            library_db_in(Path::new("/music/b/Music"), d)
        );
    }

    #[test]
    fn the_same_folder_gets_the_same_name() {
        let d = Some(Path::new("/data/beets"));
        assert_eq!(
            library_db_in(Path::new("/music/a"), d),
            library_db_in(Path::new("/music/a"), d)
        );
        // Pinned so an algorithm change cannot silently orphan existing dbs.
        assert_eq!(
            library_db_in(Path::new("/music/a"), d),
            Path::new("/data/beets/a-8cae68d89bf919ed.db")
        );
    }

    #[test]
    fn weird_characters_are_sanitized() {
        let db = library_db_in(Path::new("/music/My M\u{fc}sic: \"x\"?*"), Some(Path::new("/d")));
        let name = db.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
            "{name}"
        );
        assert_eq!(db.parent(), Some(Path::new("/d")));
        let root = library_db_in(Path::new("/"), Some(Path::new("/d")));
        assert!(root.file_name().unwrap().to_string_lossy().ends_with(".db"));
    }

    #[test]
    fn migration_copies_the_legacy_db_and_keeps_it() {
        let t = tempfile::tempdir().unwrap();
        let legacy = t.path().join("legacy.db");
        let new = t.path().join("new.db");
        std::fs::write(&legacy, b"old").unwrap();
        assert!(migrate_legacy_db(&legacy, &new));
        assert_eq!(std::fs::read(&new).unwrap(), b"old");
        assert!(legacy.exists());
    }

    #[test]
    fn migration_leaves_no_temp_file_behind() {
        let t = tempfile::tempdir().unwrap();
        let legacy = t.path().join("legacy.db");
        let new = t.path().join("new.db");
        std::fs::write(&legacy, b"old").unwrap();
        assert!(migrate_legacy_db(&legacy, &new));
        let mut names: Vec<_> = std::fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["legacy.db", "new.db"]);
        assert!(!names.iter().any(|n| n.contains(".tmp-")));
    }

    #[test]
    fn a_failed_migration_leaves_no_temp_file_behind() {
        let t = tempfile::tempdir().unwrap();
        let legacy = t.path().join("legacy.db");
        std::fs::write(&legacy, b"old").unwrap();
        // The target directory does not exist, so the copy fails.
        assert!(!migrate_legacy_db(&legacy, &t.path().join("missing/new.db")));
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_migration_keeps_the_legacy_db() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("music");
        std::fs::create_dir(&root).unwrap();
        let legacy = root.join(".beets_library.db");
        std::fs::write(&legacy, b"old").unwrap();
        // The new db's parent is a file, so it can be neither created nor copied to.
        let blocker = t.path().join("blocker");
        std::fs::write(&blocker, b"").unwrap();
        let db = blocker.join("x.db");
        assert_eq!(choose_db(&root, &db, link_file), legacy);
    }

    #[test]
    fn a_successful_migration_uses_the_new_db() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("music");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join(".beets_library.db"), b"old").unwrap();
        let db = t.path().join("dir").join("x.db");
        assert_eq!(choose_db(&root, &db, link_file), db);
        assert_eq!(std::fs::read(&db).unwrap(), b"old");
    }

    #[test]
    fn no_legacy_db_uses_the_new_path() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("x.db");
        assert_eq!(choose_db(&t.path().join("music"), &db, link_file), db);
    }

    #[test]
    fn migration_falls_back_to_rename_when_linking_fails() {
        fn no_links(_: &Path, _: &Path) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }
        let t = tempfile::tempdir().unwrap();
        let legacy = t.path().join("legacy.db");
        let new = t.path().join("new.db");
        std::fs::write(&legacy, b"old").unwrap();
        assert!(migrate_with(&legacy, &new, no_links));
        assert_eq!(std::fs::read(&new).unwrap(), b"old");
        assert!(legacy.exists());
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 2);
    }

    #[test]
    fn migration_never_overwrites_an_existing_db() {
        let t = tempfile::tempdir().unwrap();
        let legacy = t.path().join("legacy.db");
        let new = t.path().join("new.db");
        std::fs::write(&legacy, b"old").unwrap();
        std::fs::write(&new, b"mine").unwrap();
        assert!(!migrate_legacy_db(&legacy, &new));
        assert_eq!(std::fs::read(&new).unwrap(), b"mine");
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 2);
    }

    #[test]
    fn migration_without_a_legacy_db_does_nothing() {
        let t = tempfile::tempdir().unwrap();
        let new = t.path().join("new.db");
        assert!(!migrate_legacy_db(&t.path().join("none.db"), &new));
        assert!(!new.exists());
    }
}
