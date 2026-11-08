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

/// The smallest byte string the cache accepts as a complete PNG: the
/// signature followed by an `IEND` chunk.
const PNG_FIXTURE: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xaeB`\x82";

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
    fs::write(&path, PNG_FIXTURE).expect("seed cached image");
    let image = cache.get_or_download(url).expect("read cached image");
    assert_eq!(image.filename, "test_hero.png");
    assert_eq!(image.bytes, PNG_FIXTURE);
}

#[test]
fn test_downloads_on_miss() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let (server, handle) = one_shot_image_server(PNG_FIXTURE);
    let url = format!("{server}/apps/dota_react/heroes/new_hero.png");
    let image = cache.get_or_download(&url).expect("download image");
    handle.join().expect("image server");
    assert_eq!(image.filename, "new_hero.png");
    assert_eq!(image.bytes, PNG_FIXTURE);
    assert_eq!(
        fs::read(cache.path_for_url(&url)).expect("persisted cached image"),
        PNG_FIXTURE
    );
}

#[test]
fn test_truncated_cached_file_is_downloaded_again() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let (server, handle) = one_shot_image_server(PNG_FIXTURE);
    let url = format!("{server}/apps/dota_react/heroes/cut_short.png");
    let path = cache.path_for_url(&url);
    fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
    fs::write(&path, &PNG_FIXTURE[..12]).expect("seed a write that stopped part way");

    let image = cache.get_or_download(&url).expect("re-download the image");
    handle.join().expect("image server");

    assert_eq!(image.bytes, PNG_FIXTURE);
    assert_eq!(fs::read(&path).expect("replaced cached image"), PNG_FIXTURE);
    let leftovers = fs::read_dir(path.parent().expect("cache parent"))
        .expect("list cache directory")
        .count();
    assert_eq!(leftovers, 1, "no temporary download file is left behind");
}

#[test]
fn test_non_image_download_is_not_cached() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let (server, handle) = one_shot_image_server(b"<html>temporarily unavailable</html>");
    let url = format!("{server}/apps/dota_react/heroes/error_page.png");

    let error = cache
        .get_or_download(&url)
        .expect_err("a non-image body must not be served");
    handle.join().expect("image server");

    assert!(matches!(error, TriviaImageCacheError::InvalidImage { .. }));
    assert!(!cache.path_for_url(&url).exists());
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
    fs::write(&path, PNG_FIXTURE).expect("seed cached image");
    let image = cache.get_or_download(url).expect("cached attachment");
    assert_eq!(image.filename, "cached.png");
    assert_eq!(image.bytes, PNG_FIXTURE);
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

#[test]
fn warm_urls_caches_each_url_once() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let (server, handle) = one_shot_image_server(PNG_FIXTURE);
    let url = format!("{server}/apps/dota_react/heroes/renders/new_hero.png");
    let urls = BTreeSet::from([url.clone()]);
    let first = cache.warm_urls(urls.clone()).expect("first warm");
    handle.join().expect("image server");
    assert_eq!((first.newly_cached, first.total_urls), (1, 1));
    assert_eq!(
        fs::read(cache.path_for_url(&url)).expect("cached render"),
        PNG_FIXTURE
    );
    // The one-shot server is gone; a second warm must not need it.
    let second = cache.warm_urls(urls).expect("second warm");
    assert_eq!((second.newly_cached, second.total_urls), (0, 1));
}
