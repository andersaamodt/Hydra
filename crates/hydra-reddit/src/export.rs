use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::Url;
use zip::ZipArchive;

const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RECORDS: usize = 500_000;
const MAX_BODY_BYTES: usize = 100_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportItemKind {
    Post,
    Comment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportItem {
    pub kind: ExportItemKind,
    pub fullname: String,
    pub permalink: String,
    pub created_at: Option<u64>,
    pub original_date: Option<String>,
    pub subreddit: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub root_fullname: Option<String>,
    pub parent_fullname: Option<String>,
    pub root_permalink: Option<String>,
    pub parent_permalink: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPreview {
    pub posts: usize,
    pub comments: usize,
    pub items: Vec<ExportItem>,
}

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("Reddit export path does not exist")]
    Missing,
    #[error("Reddit export must be a ZIP file or extracted directory")]
    Unsupported,
    #[error("Reddit export is too large")]
    TooLarge,
    #[error("Reddit export contains too many records")]
    TooManyRecords,
    #[error("Reddit export has an invalid archive: {0}")]
    Zip(String),
    #[error("Reddit export has invalid CSV data: {0}")]
    Csv(String),
    #[error("Reddit export contains an invalid {0} record")]
    InvalidRecord(&'static str),
    #[error("Reddit export could not be read: {0}")]
    Io(#[from] std::io::Error),
}

/// Reads only the user's authored `posts.csv` and `comments.csv` files from an
/// official Reddit account-data export. No vote, message, IP, or third-party
/// context files are inspected.
///
/// # Errors
///
/// Returns an error when the path or format is unsupported, the export exceeds
/// safety limits, or its authored-content CSV data is invalid.
pub fn preview_export(path: impl AsRef<Path>) -> Result<ExportPreview, ExportError> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(ExportError::Missing);
    }
    let files = if path.is_dir() {
        read_directory(path)?
    } else if path.extension().and_then(|value| value.to_str()) == Some("zip") {
        read_zip(path)?
    } else {
        return Err(ExportError::Unsupported);
    };
    let mut items = Vec::new();
    if let Some(bytes) = files.get("posts.csv") {
        parse_csv(bytes, ExportItemKind::Post, &mut items)?;
    }
    let posts = items.len();
    if let Some(bytes) = files.get("comments.csv") {
        parse_csv(bytes, ExportItemKind::Comment, &mut items)?;
    }
    if files.is_empty() {
        return Err(ExportError::Unsupported);
    }
    let comments = items.len().saturating_sub(posts);
    items.sort_by_key(|item| item.created_at.unwrap_or_default());
    Ok(ExportPreview {
        posts,
        comments,
        items,
    })
}

fn read_directory(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, ExportError> {
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for name in ["posts.csv", "comments.csv"] {
        let candidate = path.join(name);
        if !candidate.is_file() {
            continue;
        }
        let length = fs::metadata(&candidate)?.len();
        total = total.saturating_add(length);
        if length > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
            return Err(ExportError::TooLarge);
        }
        files.insert(name.to_owned(), fs::read(candidate)?);
    }
    Ok(files)
}

fn read_zip(path: &Path) -> Result<BTreeMap<String, Vec<u8>>, ExportError> {
    let file = File::open(path)?;
    let mut archive = ZipArchive::new(file).map_err(|error| ExportError::Zip(error.to_string()))?;
    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| ExportError::Zip(error.to_string()))?;
        let Some(name) = PathBuf::from(entry.name())
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
        else {
            continue;
        };
        if !matches!(name.as_str(), "posts.csv" | "comments.csv") {
            continue;
        }
        total = total.saturating_add(entry.size());
        if entry.size() > MAX_FILE_BYTES || total > MAX_TOTAL_BYTES {
            return Err(ExportError::TooLarge);
        }
        let mut bytes = Vec::with_capacity(usize::try_from(entry.size()).unwrap_or_default());
        entry
            .by_ref()
            .take(MAX_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_FILE_BYTES {
            return Err(ExportError::TooLarge);
        }
        files.insert(name, bytes);
    }
    Ok(files)
}

fn parse_csv(
    bytes: &[u8],
    kind: ExportItemKind,
    items: &mut Vec<ExportItem>,
) -> Result<(), ExportError> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(Cursor::new(bytes));
    let headers = reader
        .headers()
        .map_err(|error| ExportError::Csv(error.to_string()))?
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    for row in reader.records() {
        if items.len() >= MAX_RECORDS {
            return Err(ExportError::TooManyRecords);
        }
        let row = row.map_err(|error| ExportError::Csv(error.to_string()))?;
        let values = headers
            .iter()
            .zip(row.iter())
            .map(|(key, value)| (key.as_str(), value.trim()))
            .collect::<BTreeMap<_, _>>();
        items.push(parse_record(kind, &values)?);
    }
    Ok(())
}

fn parse_record(
    kind: ExportItemKind,
    values: &BTreeMap<&str, &str>,
) -> Result<ExportItem, ExportError> {
    let prefix = match kind {
        ExportItemKind::Post => "t3_",
        ExportItemKind::Comment => "t1_",
    };
    let id = required(values, &["id"], "identifier")?;
    let fullname =
        normalize_fullname(id, prefix).ok_or(ExportError::InvalidRecord("identifier"))?;
    let permalink = canonical_permalink(required(values, &["permalink"], "permalink")?)
        .ok_or(ExportError::InvalidRecord("permalink"))?;
    let body = value(values, &["body", "selftext"]).unwrap_or_default();
    if body.len() > MAX_BODY_BYTES {
        return Err(ExportError::InvalidRecord("body"));
    }
    let original_date = value(values, &["date", "created_utc", "created"])
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let created_at = original_date.as_deref().and_then(parse_date);
    let subreddit = value(values, &["subreddit", "community"])
        .filter(|value| !value.is_empty())
        .map(|value| value.trim_start_matches("r/").to_owned());
    let title = value(values, &["title"])
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if title.as_ref().is_some_and(|value| value.len() > 300) {
        return Err(ExportError::InvalidRecord("title"));
    }
    let root_fullname = value(values, &["link", "link_id", "post_id"])
        .and_then(|value| normalize_fullname(value, "t3_"));
    let parent_fullname = value(values, &["parent", "parent_id"]).and_then(|value| {
        normalize_fullname(value, "t1_").or_else(|| normalize_fullname(value, "t3_"))
    });
    let (root_permalink, parent_permalink) = comment_context_urls(
        &permalink,
        root_fullname.as_deref(),
        parent_fullname.as_deref(),
    );
    Ok(ExportItem {
        kind,
        fullname,
        permalink,
        created_at,
        original_date,
        subreddit,
        title,
        body: body.to_owned(),
        root_fullname,
        parent_fullname,
        root_permalink,
        parent_permalink,
    })
}

fn comment_context_urls(
    permalink: &str,
    root_fullname: Option<&str>,
    parent_fullname: Option<&str>,
) -> (Option<String>, Option<String>) {
    let Ok(mut url) = Url::parse(permalink) else {
        return (None, None);
    };
    let mut parts = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let Some(comments_index) = parts.iter().position(|part| part == "comments") else {
        return (None, None);
    };
    if parts.len() < comments_index + 3 {
        return (None, None);
    }
    parts.truncate(comments_index + 3);
    url.set_path(&format!("/{}/", parts.join("/")));
    let root = url.to_string();
    let parent = match parent_fullname {
        Some(parent) if parent.starts_with("t1_") => {
            let mut parent_url = url;
            parent_url.set_path(&format!(
                "/{}/{}/",
                parts.join("/"),
                parent.trim_start_matches("t1_")
            ));
            Some(parent_url.to_string())
        }
        Some(parent) if Some(parent) == root_fullname => Some(root.clone()),
        _ => None,
    };
    (root_fullname.map(|_| root), parent)
}

fn required<'a>(
    values: &'a BTreeMap<&str, &str>,
    keys: &[&str],
    label: &'static str,
) -> Result<&'a str, ExportError> {
    value(values, keys)
        .filter(|value| !value.is_empty())
        .ok_or(ExportError::InvalidRecord(label))
}

fn value<'a>(values: &'a BTreeMap<&str, &str>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| values.get(key).copied())
}

fn normalize_fullname(value: &str, prefix: &str) -> Option<String> {
    let value = value.trim();
    let candidate = if value.starts_with("t1_") || value.starts_with("t3_") {
        value.to_owned()
    } else {
        format!("{prefix}{value}")
    };
    let (actual_prefix, id) = candidate.split_at(3);
    if (actual_prefix == "t1_" || actual_prefix == "t3_")
        && !id.is_empty()
        && id.len() <= 32
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        Some(candidate)
    } else {
        None
    }
}

fn canonical_permalink(value: &str) -> Option<String> {
    let candidate = if value.starts_with('/') {
        format!("https://www.reddit.com{value}")
    } else {
        value.to_owned()
    };
    let mut url = Url::parse(&candidate).ok()?;
    if url.scheme() != "https"
        || !matches!(
            url.host_str(),
            Some("reddit.com" | "www.reddit.com" | "old.reddit.com")
        )
    {
        return None;
    }
    url.set_host(Some("www.reddit.com")).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string())
}

fn parse_date(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().or_else(|| {
        OffsetDateTime::parse(value, &Rfc3339)
            .ok()?
            .unix_timestamp()
            .try_into()
            .ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_only_authored_post_and_comment_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("posts.csv"),
            "id,permalink,date,subreddit,title,body\nabc,/r/science/comments/abc/title/,2024-01-01T00:00:00Z,science,Title,Body\n",
        )
        .unwrap();
        fs::write(
            root.path().join("comments.csv"),
            "id,permalink,date,subreddit,link,parent,body\ndef,/r/science/comments/abc/title/def/,2024-01-02T00:00:00Z,science,t3_abc,t1_parent,Reply\n",
        )
        .unwrap();
        fs::write(root.path().join("messages.csv"), "body\nprivate\n").unwrap();
        let preview = preview_export(root.path()).unwrap();
        assert_eq!((preview.posts, preview.comments), (1, 1));
        assert_eq!(preview.items[0].fullname, "t3_abc");
        assert_eq!(
            preview.items[1].parent_fullname.as_deref(),
            Some("t1_parent")
        );
        assert_eq!(
            preview.items[1].root_permalink.as_deref(),
            Some("https://www.reddit.com/r/science/comments/abc/title/")
        );
        assert_eq!(
            preview.items[1].parent_permalink.as_deref(),
            Some("https://www.reddit.com/r/science/comments/abc/title/parent/")
        );
        assert!(preview.items.iter().all(|item| item.body != "private"));
    }

    #[test]
    fn zip_import_ignores_every_non_authored_file_and_never_extracts_paths() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("reddit-export.zip");
        let file = File::create(&path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("account/posts.csv", options).unwrap();
        archive.write_all(b"id,permalink,subreddit,title,body\nabc,/r/science/comments/abc/title/,science,Title,Body\n").unwrap();
        archive.start_file("../messages.csv", options).unwrap();
        archive.write_all(b"body\nprivate\n").unwrap();
        archive.start_file("ip_logs.csv", options).unwrap();
        archive.write_all(b"ip\n127.0.0.1\n").unwrap();
        archive.finish().unwrap();

        let preview = preview_export(path).unwrap();
        assert_eq!((preview.posts, preview.comments), (1, 0));
        assert_eq!(preview.items[0].body, "Body");
        assert!(!root.path().join("messages.csv").exists());
    }
}
