//! Turns the user's "source" string into something the WebView can navigate to.
//!
//! Local content is never loaded through `file://`. Instead the containing
//! folder is mapped onto a virtual HTTPS host, which gives local pages a real
//! secure origin: ES modules, `fetch()` of sibling files, service workers and
//! `localStorage` all behave exactly as they would on a web server.

use std::path::{Path, PathBuf};

/// Virtual host that local folders are mapped to. `.example` is reserved by
/// RFC 2606, so it can never collide with (or leak requests to) a real site.
pub const VIRTUAL_HOST: &str = "livecanvas.example";

/// A local folder to expose to the page as `https://{host}/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMapping {
    pub host: &'static str,
    pub folder: PathBuf,
}

/// The outcome of resolving a source string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Navigation {
    /// The URL to navigate to.
    pub url: String,
    /// Present when the URL points into a mapped local folder.
    pub mapping: Option<FolderMapping>,
}

/// Resolves a URL, an HTML file path, or a folder containing `index.html`.
pub fn resolve(source: &str) -> Result<Navigation, String> {
    let source = source.trim().trim_matches('"').trim();
    if source.is_empty() {
        return Err("no source given".into());
    }

    let lower = source.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower == "about:blank" {
        return Ok(Navigation { url: source.to_owned(), mapping: None });
    }
    if let Some(rest) = lower.strip_prefix("file://") {
        // `file:///C:/dir/page.html` -> `C:/dir/page.html`; then resolve as a path.
        let start = source.len() - rest.len();
        let path = percent_decode(source[start..].trim_start_matches('/'));
        return resolve_path(Path::new(&path));
    }
    if let Some((scheme, _)) = lower.split_once("://") {
        return Err(format!("unsupported URL scheme `{scheme}://`"));
    }

    let path = Path::new(source);
    if path.exists() {
        return resolve_path(path);
    }
    // A bare `page.html` that does not exist is a mistyped file name, not a web host.
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    if !source.contains('/') && matches!(extension.as_str(), "html" | "htm") {
        return Err(format!("file `{source}` does not exist"));
    }
    if looks_like_host(source) {
        return Ok(Navigation { url: format!("https://{source}"), mapping: None });
    }
    Err(format!("`{source}` is neither an existing file or folder nor a URL"))
}

fn resolve_path(path: &Path) -> Result<Navigation, String> {
    // `absolute` (unlike `canonicalize`) does not produce `\\?\` verbatim
    // paths, which WebView2's folder mapping does not understand.
    let path = std::path::absolute(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (folder, file) = if path.is_dir() {
        if !path.join("index.html").is_file() {
            return Err(format!("folder `{}` does not contain index.html", path.display()));
        }
        (path, "index.html".to_owned())
    } else if path.is_file() {
        let name = path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
        let folder = path.parent().map(Path::to_path_buf);
        match (folder, name) {
            (Some(folder), Some(name)) => (folder, name),
            _ => return Err(format!("`{}` is not a usable file path", path.display())),
        }
    } else {
        return Err(format!("`{}` does not exist", path.display()));
    };
    Ok(Navigation {
        url: format!("https://{VIRTUAL_HOST}/{}", percent_encode(&file)),
        mapping: Some(FolderMapping { host: VIRTUAL_HOST, folder }),
    })
}

/// `youtube.com/watch?v=..` style input: no scheme, but clearly a web address.
fn looks_like_host(s: &str) -> bool {
    let host = s.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(name, _port)| name);
    let valid_label = |l: &str| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    !s.contains(char::is_whitespace)
        && !s.contains('\\')
        && (host == "localhost" || (host.contains('.') && host.split('.').all(valid_label)))
}

/// Percent-encodes one URL path segment (RFC 3986 unreserved characters pass through).
fn percent_encode(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Decodes `%XX` escapes; malformed escapes are kept literally.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(value)) => {
                out.push(value);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Creates a unique scratch folder containing `index.html` and `my page.html`.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("livecanvas-src-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("index.html"), "<!doctype html>").unwrap();
        fs::write(dir.join("my page.html"), "<!doctype html>").unwrap();
        dir
    }

    #[test]
    fn urls_pass_through_unchanged() {
        let nav = resolve("  https://www.youtube.com/watch?v=abc  ").unwrap();
        assert_eq!(nav.url, "https://www.youtube.com/watch?v=abc");
        assert_eq!(nav.mapping, None);
    }

    #[test]
    fn bare_hosts_get_https() {
        assert_eq!(resolve("youtube.com/feed").unwrap().url, "https://youtube.com/feed");
        assert_eq!(resolve("localhost:3000").unwrap().url, "https://localhost:3000");
    }

    #[test]
    fn folder_maps_to_virtual_host() {
        let dir = scratch("folder");
        let nav = resolve(dir.to_str().unwrap()).unwrap();
        assert_eq!(nav.url, format!("https://{VIRTUAL_HOST}/index.html"));
        assert_eq!(nav.mapping.unwrap().folder, dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn file_maps_parent_and_encodes_name() {
        let dir = scratch("file");
        let quoted = format!("\"{}\"", dir.join("my page.html").display());
        let nav = resolve(&quoted).unwrap();
        assert_eq!(nav.url, format!("https://{VIRTUAL_HOST}/my%20page.html"));
        assert_eq!(nav.mapping.unwrap().folder, dir);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn folder_without_index_is_rejected() {
        let dir = scratch("noindex");
        fs::remove_file(dir.join("index.html")).unwrap();
        assert!(resolve(dir.to_str().unwrap()).unwrap_err().contains("index.html"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_nonsense_and_foreign_schemes() {
        assert!(resolve("").is_err());
        assert!(resolve("not a url or path").is_err());
        assert!(resolve("javascript://alert(1)").is_err());
        assert!(resolve("ftp://example.com/x").is_err());
    }

    #[test]
    fn missing_html_file_is_not_mistaken_for_a_host() {
        assert!(resolve("no-such-page.html").unwrap_err().contains("does not exist"));
        // With a path component it is a web address again.
        assert_eq!(resolve("example.com/page.html").unwrap().url, "https://example.com/page.html");
    }

    #[test]
    fn percent_coding_round_trips() {
        assert_eq!(percent_encode("a b&c.html"), "a%20b%26c.html");
        assert_eq!(percent_decode("a%20b%26c.html"), "a b&c.html");
        assert_eq!(percent_decode("100%zz"), "100%zz");
    }
}
