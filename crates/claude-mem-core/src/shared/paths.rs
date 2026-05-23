//! Path normalisation + direct-child detection.
//!
//! Port of `src/shared/path-utils.ts` (`normalizePath`, `isDirectChild`).
//!
//! The TS side exists so that `SessionSearch`'s "folder CLAUDE.md" path
//! lookup matches a file like `app/api/router.py` against a folder stored
//! as `/Users/dev/project/app/api` — issue #794. The Rust port keeps the
//! same algorithm verbatim.

/// Collapse backslashes → forward slashes, run of `//` → `/`, trailing `/`
/// stripped, Windows UNC `\server\share` → `/server/share`.
pub fn normalize_path(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }

    let mut s = raw.replace('\\', "/");

    // Collapse `//+` → `/` (simple scan; no regex needed).
    let mut out = String::with_capacity(s.len());
    let mut prev = '\0';
    for c in s.chars() {
        if !(c == '/' && prev == '/') {
            out.push(c);
        }
        prev = c;
    }
    s = out;

    // Strip trailing `/` (but keep a lone `/` as-is).
    if s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// Is `file_path` a direct child of `folder_path`?
///
/// "Direct child" = the file's parent directory equals the folder. Mixed
/// absolute/relative forms are accepted via a suffix match against the
/// file's dirname (fix for issue #794).
///
/// Returns false for unrelated paths, empty inputs, subdirectories, and
/// partial-segment matches (`components` does not match `components-old`).
pub fn is_direct_child(file_path: &str, folder_path: &str) -> bool {
    if file_path.is_empty() || folder_path.is_empty() {
        return false;
    }
    let file = normalize_path(file_path);
    let folder = normalize_path(folder_path);

    // File's parent directory within the normalised path.
    let file_dir = match file.rfind('/') {
        None => return false, // single-segment file, no parent
        Some(i) => &file[..i],
    };

    if file_dir.is_empty() {
        // Root-level file: only match a root folder ("/") — which we don't
        // support in practice, so `false`.
        return false;
    }

    // Either:
    //   - exact match (both same path form → "app/api" == "app/api")
    //   - folder ends with "/" + file_dir (absolute-folder / relative-file
    //     mixed case; fix for issue #794).
    // The segment-boundary check rejects "components-old" → "components":
    // the char immediately before the file_dir suffix in folder must be '/'.
    if file_dir == folder {
        return true;
    }
    let needle = format!("/{}", file_dir);
    folder.ends_with(&needle)
}
