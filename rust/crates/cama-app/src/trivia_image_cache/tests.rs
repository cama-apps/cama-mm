use super::*;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use tempfile::tempdir;

#[test]
fn shared_steam_image_root_keeps_trivia_and_scout_under_one_configured_parent() {
    let root = steam_image_cache_root_from(Some(OsString::from("/app/data/cache")));
    assert_eq!(root, PathBuf::from("/app/data/cache"));
    assert_eq!(root.join("trivia"), PathBuf::from("/app/data/cache/trivia"));
    assert_eq!(root.join("scout"), PathBuf::from("/app/data/cache/scout"));
}

#[test]
fn shared_steam_image_root_preserves_local_default_for_empty_configuration() {
    assert_eq!(
        steam_image_cache_root_from(None),
        PathBuf::from(DEFAULT_STEAM_IMAGE_CACHE_ROOT)
    );
    assert_eq!(
        steam_image_cache_root_from(Some(OsString::new())),
        PathBuf::from(DEFAULT_STEAM_IMAGE_CACHE_ROOT)
    );
}

fn one_shot_image_server(body: &'static [u8]) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind image server");
    let address = listener.local_addr().expect("image server address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept image request");
        let mut request = [0_u8; 4_096];
        let _ = stream.read(&mut request).expect("read image request");
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .expect("write image response headers");
        stream.write_all(body).expect("write image response body");
    });
    (format!("http://{address}"), handle)
}

#[test]
fn steam_urls_keep_python_category_and_filename_shape() {
    let cache = TriviaImageCache::new("cache-root").expect("cache");
    assert_eq!(
        cache.path_for_url(
            "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png"
        ),
        PathBuf::from("cache-root/heroes/antimage.png")
    );
}

#[test]
fn other_urls_use_a_deterministic_flat_filename() {
    let cache = TriviaImageCache::new("cache-root").expect("cache");
    let first = cache.path_for_url("https://example.com/icon.webp?version=2");
    let second = cache.path_for_url("https://example.com/icon.webp?version=2");
    assert_eq!(first, second);
    assert_eq!(first.parent(), Some(Path::new("cache-root/other")));
    assert_eq!(
        first.extension().and_then(|value| value.to_str()),
        Some("webp")
    );
    assert_eq!(
        first.file_name().and_then(|value| value.to_str()),
        Some("04ed3ed3e0089da15a08e91c54264afe.webp")
    );
}

#[test]
fn fallback_hash_is_python_compatible_md5() {
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
}

#[test]
fn test_hero_url() {
    let cache = TriviaImageCache::new("cache-root").expect("cache");
    let path = cache.path_for_url(
        "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/heroes/antimage.png",
    );
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("antimage.png")
    );
    assert!(path.components().any(|part| part.as_os_str() == "heroes"));
}

#[test]
fn test_ability_url() {
    let cache = TriviaImageCache::new("cache-root").expect("cache");
    let path = cache.path_for_url(
        "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/abilities/antimage_mana_break.png",
    );
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("antimage_mana_break.png")
    );
    assert!(
        path.components()
            .any(|part| part.as_os_str() == "abilities")
    );
}

#[test]
fn test_item_url() {
    let cache = TriviaImageCache::new("cache-root").expect("cache");
    let path = cache.path_for_url(
        "https://cdn.cloudflare.steamstatic.com/apps/dota2/images/dota_react/items/blink.png",
    );
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("blink.png")
    );
    assert!(path.components().any(|part| part.as_os_str() == "items"));
}

#[test]
fn test_unknown_url_uses_hash() {
    let cache = TriviaImageCache::new("cache-root").expect("cache");
    let path = cache.path_for_url("https://example.com/some/random/image.png");
    assert_eq!(path.parent(), Some(Path::new("cache-root/other")));
    assert_eq!(
        path.extension().and_then(|value| value.to_str()),
        Some("png")
    );
}

#[test]
fn test_returns_existing_path() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let url = "https://cdn.example/apps/dota_react/heroes/test_hero.png";
    let path = cache.path_for_url(url);
    fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
    fs::write(&path, b"\x89PNG fake image data").expect("seed cached image");
    let image = cache.get_or_download(url).expect("read cached image");
    assert_eq!(image.filename, "test_hero.png");
    assert_eq!(image.bytes, b"\x89PNG fake image data");
}

#[test]
fn test_downloads_on_miss() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let (server, handle) = one_shot_image_server(b"\x89PNG fake");
    let url = format!("{server}/apps/dota_react/heroes/new_hero.png");
    let image = cache.get_or_download(&url).expect("download image");
    handle.join().expect("image server");
    assert_eq!(image.filename, "new_hero.png");
    assert_eq!(image.bytes, b"\x89PNG fake");
    assert_eq!(
        fs::read(cache.path_for_url(&url)).expect("persisted cached image"),
        b"\x89PNG fake"
    );
}

#[test]
fn test_returns_none_on_failure() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unused port");
    let address = listener.local_addr().expect("unused address");
    drop(listener);
    let url = format!("http://{address}/apps/dota_react/heroes/bad.png");
    assert!(cache.get_or_download(&url).is_err());
    assert!(!cache.path_for_url(&url).exists());
}

#[test]
fn test_returns_file_from_cache() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let url = "https://cdn.example/apps/dota_react/heroes/cached.png";
    let path = cache.path_for_url(url);
    fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
    fs::write(&path, b"\x89PNG data").expect("seed cached image");
    let image = cache.get_or_download(url).expect("cached attachment");
    assert_eq!(image.filename, "cached.png");
    assert_eq!(image.bytes, b"\x89PNG data");
}

#[test]
fn test_returns_none_when_not_cached() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve unused port");
    let address = listener.local_addr().expect("unused address");
    drop(listener);
    let url = format!("http://{address}/apps/dota_react/heroes/missing.png");
    assert!(cache.get_or_download(&url).is_err());
    assert!(!cache.path_for_url(&url).exists());
}
