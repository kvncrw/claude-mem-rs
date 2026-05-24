//! Folder-level CLAUDE.md generation and cleanup.

use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const START_TAG: &str = "<claude-mem-context>";
const END_TAG: &str = "</claude-mem-context>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeMdOptions {
    pub dry_run: bool,
    pub project_root: PathBuf,
    pub db_path: Option<PathBuf>,
    pub project: Option<String>,
    pub target_file: Option<String>,
    pub limit: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeMdReport {
    pub dry_run: bool,
    pub scanned: usize,
    pub written: usize,
    pub cleaned: usize,
    pub deleted: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    pub files: Vec<PathBuf>,
}

pub fn generate(options: ClaudeMdOptions) -> Result<ClaudeMdReport> {
    let root = options.project_root.canonicalize().with_context(|| {
        format!(
            "project root does not exist or is not readable: {}",
            options.project_root.display()
        )
    })?;
    let db_path = options.db_path.unwrap_or_else(default_db_path);
    let target_file = target_filename(options.target_file);
    let project = options.project.clone().unwrap_or_else(|| {
        root.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_owned()
    });
    let mut report = ClaudeMdReport {
        dry_run: options.dry_run,
        ..Default::default()
    };
    if !db_path.exists() {
        return Ok(report);
    }
    let conn = Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let folders = tracked_folders(&root);
    report.scanned = folders.len();
    for folder in folders {
        if should_skip_folder(&root, &folder) {
            report.skipped += 1;
            continue;
        }
        match observations_for_folder(&conn, &project, &root, &folder, options.limit) {
            Ok(rows) if rows.is_empty() => report.skipped += 1,
            Ok(rows) => {
                let content = format_observations(&rows);
                let path = folder.join(&target_file);
                if !options.dry_run {
                    write_tagged(&path, &content)?;
                }
                report.written += 1;
                report.files.push(path);
            }
            Err(error) => report.errors.push(format!("{}: {error}", folder.display())),
        }
    }
    Ok(report)
}

pub fn clean(options: ClaudeMdOptions) -> Result<ClaudeMdReport> {
    let root = options.project_root.canonicalize().with_context(|| {
        format!(
            "project root does not exist or is not readable: {}",
            options.project_root.display()
        )
    })?;
    let target_file = target_filename(options.target_file);
    let mut report = ClaudeMdReport {
        dry_run: options.dry_run,
        ..Default::default()
    };
    for path in find_context_files(&root, &target_file) {
        report.scanned += 1;
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                report.errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        let stripped = strip_tagged(&content).trim().to_owned();
        if stripped.is_empty() {
            if !options.dry_run {
                fs::remove_file(&path)?;
            }
            report.deleted += 1;
        } else {
            if !options.dry_run {
                fs::write(&path, format!("{stripped}\n"))?;
            }
            report.cleaned += 1;
        }
        report.files.push(path);
    }
    Ok(report)
}

pub fn print_report(action: &str, report: &ClaudeMdReport) {
    println!("claude-mem-rs {action}");
    if report.dry_run {
        println!("mode: dry-run");
    }
    println!(
        "scanned={} written={} cleaned={} deleted={} skipped={} errors={}",
        report.scanned,
        report.written,
        report.cleaned,
        report.deleted,
        report.skipped,
        report.errors.len()
    );
    for path in &report.files {
        println!("  {}", path.display());
    }
    for error in &report.errors {
        eprintln!("  failed: {error}");
    }
}

fn tracked_folders(root: &Path) -> BTreeSet<PathBuf> {
    if let Ok(output) = Command::new("git")
        .arg("ls-files")
        .current_dir(root)
        .output()
    {
        if output.status.success() {
            let mut folders = BTreeSet::new();
            let text = String::from_utf8_lossy(&output.stdout);
            for file in text.lines().filter(|line| !line.trim().is_empty()) {
                let mut dir = root.join(file).parent().map(Path::to_path_buf);
                while let Some(folder) = dir {
                    if folder == root || !folder.starts_with(root) {
                        break;
                    }
                    folders.insert(folder.clone());
                    dir = folder.parent().map(Path::to_path_buf);
                }
            }
            return folders;
        }
    }
    let mut folders = BTreeSet::new();
    walk_dirs(root, root, 10, &mut folders);
    folders
}

fn walk_dirs(root: &Path, dir: &Path, depth: usize, folders: &mut BTreeSet<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || ignored_dir(&path) {
            continue;
        }
        if path != root {
            folders.insert(path.clone());
        }
        walk_dirs(root, &path, depth - 1, folders);
    }
}

fn observations_for_folder(
    conn: &Connection,
    project: &str,
    root: &Path,
    folder: &Path,
    limit: i64,
) -> Result<Vec<FolderObservation>> {
    let relative = folder
        .strip_prefix(root)
        .map_err(|_| anyhow!("folder escapes project root"))?
        .to_string_lossy()
        .replace('\\', "/");
    let absolute = folder.to_string_lossy().replace('\\', "/");
    let pattern = format!("%\"{relative}/%");
    let absolute_pattern = format!("%\"{absolute}/%");
    let mut stmt = conn.prepare(
        "SELECT id, title, type, created_at, created_at_epoch,
                COALESCE(discovery_tokens,0), files_read, files_modified
           FROM observations
          WHERE project = ?1
            AND (files_read LIKE ?2 OR files_modified LIKE ?2
              OR files_read LIKE ?3 OR files_modified LIKE ?3)
          ORDER BY created_at_epoch DESC, id DESC
          LIMIT ?4",
    )?;
    let rows = stmt.query_map((&project, &pattern, &absolute_pattern, limit * 3), |row| {
        Ok(FolderObservation {
            id: row.get(0)?,
            title: row
                .get::<_, Option<String>>(1)?
                .unwrap_or_else(|| "Untitled".into()),
            kind: row.get(2)?,
            created_at: row.get(3)?,
            created_at_epoch: row.get(4)?,
            tokens: row.get(5)?,
            file: relevant_file(
                row.get(6)?,
                row.get(7)?,
                &[relative.as_str(), absolute.as_str()],
            ),
        })
    })?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|row| row.file.is_some())
        .take(limit.max(0) as usize)
        .collect())
}

#[derive(Debug, Clone)]
struct FolderObservation {
    id: i64,
    title: String,
    kind: String,
    created_at: String,
    created_at_epoch: i64,
    tokens: i64,
    file: Option<String>,
}

fn relevant_file(
    read: Option<String>,
    modified: Option<String>,
    folders: &[&str],
) -> Option<String> {
    parse_files(modified)
        .into_iter()
        .chain(parse_files(read))
        .find(|file| folders.iter().any(|folder| is_direct_child(file, folder)))
        .and_then(|file| Path::new(&file).file_name()?.to_str().map(str::to_owned))
}

fn parse_files(value: Option<String>) -> Vec<String> {
    value
        .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok())
        .unwrap_or_default()
}

fn is_direct_child(file: &str, folder: &str) -> bool {
    let normalized = file.replace('\\', "/");
    let Some(parent) = Path::new(&normalized).parent() else {
        return false;
    };
    parent.to_string_lossy().replace('\\', "/") == folder
}

fn format_observations(rows: &[FolderObservation]) -> String {
    let mut out = vec![
        "# Recent Activity".to_owned(),
        String::new(),
        "<!-- This section is auto-generated by claude-mem-rs. Edit content outside the tags. -->"
            .to_owned(),
        String::new(),
    ];
    let mut current_day = String::new();
    let mut current_file = String::new();
    for row in rows {
        let day = row
            .created_at
            .split('T')
            .next()
            .unwrap_or(row.created_at.as_str())
            .to_owned();
        if day != current_day {
            current_day = day.clone();
            out.push(format!("### {day}"));
            out.push(String::new());
            current_file.clear();
        }
        let file = row.file.as_deref().unwrap_or("General");
        if file != current_file {
            current_file = file.to_owned();
            out.push(format!("**{file}**"));
            out.push("| ID | Epoch | T | Title | Read |".to_owned());
            out.push("|----|------:|---|-------|------|".to_owned());
        }
        out.push(format!(
            "| #{} | {} | {} | {} | ~{} |",
            row.id,
            row.created_at_epoch,
            type_icon(&row.kind),
            row.title.replace('|', "\\|"),
            row.tokens
        ));
    }
    out.join("\n")
}

fn write_tagged(path: &Path, new_content: &str) -> Result<()> {
    let folder = path
        .parent()
        .ok_or_else(|| anyhow!("target path has no parent: {}", path.display()))?;
    if !folder.exists() {
        return Err(anyhow!("folder does not exist: {}", folder.display()));
    }
    let existing = fs::read_to_string(path).unwrap_or_default();
    let final_content = replace_tagged(&existing, new_content);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, final_content)?;
    fs::rename(tmp, path)?;
    Ok(())
}

fn replace_tagged(existing: &str, new_content: &str) -> String {
    if existing.is_empty() {
        return format!("{START_TAG}\n{new_content}\n{END_TAG}\n");
    }
    if let (Some(start), Some(end)) = (existing.find(START_TAG), existing.find(END_TAG)) {
        return format!(
            "{}{START_TAG}\n{new_content}\n{END_TAG}{}\n",
            &existing[..start],
            &existing[end + END_TAG.len()..].trim_end()
        );
    }
    format!(
        "{}\n\n{START_TAG}\n{new_content}\n{END_TAG}\n",
        existing.trim_end()
    )
}

fn strip_tagged(content: &str) -> String {
    let mut out = content.to_owned();
    while let (Some(start), Some(end)) = (out.find(START_TAG), out.find(END_TAG)) {
        out.replace_range(start..end + END_TAG.len(), "");
    }
    out
}

fn find_context_files(root: &Path, target_file: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_context_files(root, target_file, 20, &mut out);
    out
}

fn walk_context_files(dir: &Path, target_file: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !ignored_dir(&path) {
                walk_context_files(&path, target_file, depth - 1, out);
            }
        } else if path.file_name().and_then(|name| name.to_str()) == Some(target_file)
            && fs::read_to_string(&path)
                .map(|content| content.contains(START_TAG))
                .unwrap_or(false)
        {
            out.push(path);
        }
    }
}

fn should_skip_folder(root: &Path, folder: &Path) -> bool {
    folder == root
        || ignored_dir(folder)
        || folder.join(".git").exists()
        || folder.components().any(|component| {
            matches!(
                component.as_os_str().to_str(),
                Some(".git" | "node_modules" | "target" | "build" | "dist" | "res")
            )
        })
}

fn ignored_dir(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            ".git"
                | "node_modules"
                | "target"
                | "build"
                | "dist"
                | ".next"
                | ".cache"
                | ".turbo"
                | "coverage"
                | "__pycache__"
                | ".venv"
                | "venv"
        )
    )
}

fn target_filename(value: Option<String>) -> String {
    value
        .or_else(|| {
            (std::env::var("CLAUDE_MEM_FOLDER_USE_LOCAL_MD").as_deref() == Ok("true"))
                .then(|| "CLAUDE.local.md".to_owned())
        })
        .unwrap_or_else(|| "CLAUDE.md".to_owned())
}

fn default_db_path() -> PathBuf {
    claude_mem_core::shared::platform_paths::default_db_path()
}

fn type_icon(kind: &str) -> &'static str {
    match kind {
        "bugfix" => "R",
        "feature" => "F",
        "refactor" => "M",
        "change" => "C",
        "decision" => "D",
        "session" => "S",
        "prompt" => "P",
        _ => "O",
    }
}
