use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use reqwest::Url;
use tracing::{debug, info};

const FALLBACK_ICON: &str = "smartphone";
const LOOKUP_URL: &str = "https://itunes.apple.com/lookup";
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);
const NEGATIVE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

fn builtin_icon(app_id: &str) -> Option<&'static str> {
    if app_id.starts_with("com.apple.WebKit.PushBundle.") {
        return Some("internet-web-browser");
    }
    Some(match app_id {
        "com.apple.MobileSMS" => "dialog-messages",
        "com.apple.mobilephone" | "com.apple.InCallService" => "call-start",
        "com.apple.Preferences" => "preferences-system",
        "com.apple.Passwords" => "preferences-desktop-user-password",
        "com.apple.ScreenTimeNotifications" => "chronometer",
        _ => return None,
    })
}

pub struct Icons {
    client: Option<reqwest::Client>,
    dir: PathBuf,
    country: Option<String>,
    resolved: Mutex<HashMap<String, String>>,
}

impl Icons {
    pub fn new(lookup: bool, country: Option<String>) -> Result<Self> {
        let client = if lookup {
            Some(reqwest::Client::builder().timeout(HTTP_TIMEOUT).build()?)
        } else {
            None
        };
        Ok(Self { client, dir: cache_dir(), country, resolved: Mutex::new(HashMap::new()) })
    }

    pub async fn icon_for(&self, app_id: &str) -> String {
        if let Some(icon) = self.resolved.lock().unwrap().get(app_id) {
            return icon.clone();
        }
        let icon = self.resolve(app_id).await;
        self.resolved.lock().unwrap().insert(app_id.to_owned(), icon.clone());
        icon
    }

    async fn resolve(&self, app_id: &str) -> String {
        if let Some(icon) = builtin_icon(app_id) {
            return icon.to_owned();
        }
        let Some(client) = &self.client else {
            return FALLBACK_ICON.to_owned();
        };
        let stem = sanitize(app_id);
        let png = self.dir.join(format!("{stem}.png"));
        let missing = self.dir.join(format!("{stem}.none"));

        if png.is_file() {
            return file_uri(&png);
        }
        if is_fresh(&missing) {
            return FALLBACK_ICON.to_owned();
        }

        match self.fetch(client, app_id).await {
            Ok(Some(bytes)) => match write(&png, &bytes) {
                Ok(()) => {
                    info!("cached App Store icon for {app_id}");
                    return file_uri(&png);
                }
                Err(err) => debug!("cannot cache icon for {app_id}: {err}"),
            },
            Ok(None) => {
                debug!("{app_id} is not on the App Store");
                let _ = write(&missing, b"");
            }
            Err(err) => debug!("icon lookup for {app_id} failed: {err}"),
        }
        FALLBACK_ICON.to_owned()
    }

    async fn fetch(&self, client: &reqwest::Client, app_id: &str) -> Result<Option<Vec<u8>>> {
        let mut countries = vec![None];
        if self.country.is_some() {
            countries.push(self.country.as_deref());
        }
        for country in countries {
            let mut query = vec![("bundleId", app_id)];
            if let Some(country) = country {
                query.push(("country", country));
            }
            let found: serde_json::Value =
                client.get(LOOKUP_URL).query(&query).send().await?.error_for_status()?.json().await?;
            let artwork = found["results"][0]["artworkUrl512"].as_str().or(found["results"][0]["artworkUrl100"].as_str());
            if let Some(artwork) = artwork {
                let bytes = client.get(png_artwork(artwork)).send().await?.error_for_status()?.bytes().await?;
                return Ok(Some(bytes.to_vec()));
            }
        }
        Ok(None)
    }
}

fn png_artwork(url: &str) -> String {
    match url.rsplit_once('/') {
        Some((base, _)) => format!("{base}/256x256bb.png"),
        None => url.to_owned(),
    }
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    base.join("ancs-linux").join("icons")
}

fn sanitize(app_id: &str) -> String {
    app_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

fn is_fresh(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|modified| SystemTime::now().duration_since(modified).is_ok_and(|age| age < NEGATIVE_TTL))
}

fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

fn file_uri(path: &Path) -> String {
    Url::from_file_path(path).map(String::from).unwrap_or_else(|()| FALLBACK_ICON.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_artwork_to_png() {
        assert_eq!(
            png_artwork("https://is1-ssl.mzstatic.com/image/thumb/a/b/AppIcon.png/512x512bb.jpg"),
            "https://is1-ssl.mzstatic.com/image/thumb/a/b/AppIcon.png/256x256bb.png"
        );
    }

    #[test]
    fn maps_builtins() {
        assert_eq!(builtin_icon("com.apple.MobileSMS"), Some("dialog-messages"));
        assert_eq!(builtin_icon("com.apple.WebKit.PushBundle.ABC"), Some("internet-web-browser"));
        assert_eq!(builtin_icon("com.google.Gmail"), None);
    }

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(sanitize("com.example/../x y"), "com.example_.._x_y");
    }
}
