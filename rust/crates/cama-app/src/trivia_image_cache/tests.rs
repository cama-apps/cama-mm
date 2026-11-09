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

/// The smallest byte string the cache reads back as a complete PNG: the
/// signature followed by an `IEND` chunk. It has no pixels, so a download of
/// it is refused.
const PNG_FIXTURE: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\0IEND\xaeB`\x82";

/// A real PNG `width` by `height`: opaque red on the left half, clear on the
/// right.
fn png(width: usize, height: usize) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(width * height * 4);
    for _ in 0..height {
        for x in 0..width {
            let pixel = if x < width / 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            };
            pixels.extend_from_slice(&pixel);
        }
    }
    crate::png_encoder::encode_rgba_png(width, height, &pixels)
}

fn one_shot_image_server(body: &[u8]) -> (String, thread::JoinHandle<()>) {
    one_shot_server(body, true)
}

/// Serves `body` once. Without a `Content-Length` the body runs until the
/// connection closes. A client may hang up early on a body it refuses.
fn one_shot_server(body: &[u8], content_length: bool) -> (String, thread::JoinHandle<()>) {
    let body = body.to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind image server");
    let address = listener.local_addr().expect("image server address");
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept image request");
        let mut request = [0_u8; 4_096];
        let _ = stream.read(&mut request).expect("read image request");
        let length = if content_length {
            format!("Content-Length: {}\r\n", body.len())
        } else {
            String::new()
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\n{length}Connection: close\r\n\r\n"
        )
        .expect("write image response headers");
        let _ = stream.write_all(&body);
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
    let body = png(2, 2);
    let (server, handle) = one_shot_image_server(&body);
    let url = format!("{server}/apps/dota_react/heroes/new_hero.png");
    let image = cache.get_or_download(&url).expect("download image");
    handle.join().expect("image server");
    assert_eq!(image.filename, "new_hero.png");
    assert_eq!(image.bytes, body);
    assert_eq!(
        fs::read(cache.path_for_url(&url)).expect("persisted cached image"),
        body
    );
}

#[test]
fn test_truncated_cached_file_is_downloaded_again() {
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");
    let body = png(2, 2);
    let (server, handle) = one_shot_image_server(&body);
    let url = format!("{server}/apps/dota_react/heroes/cut_short.png");
    let path = cache.path_for_url(&url);
    fs::create_dir_all(path.parent().expect("cache parent")).expect("create cache parent");
    fs::write(&path, &body[..12]).expect("seed a write that stopped part way");

    let image = cache.get_or_download(&url).expect("re-download the image");
    handle.join().expect("image server");

    assert_eq!(image.bytes, body);
    assert_eq!(fs::read(&path).expect("replaced cached image"), body);
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
    let body = png(2, 2);
    let (server, handle) = one_shot_image_server(&body);
    let url = format!("{server}/apps/dota_react/heroes/renders/new_hero.png");
    let urls = BTreeSet::from([url.clone()]);
    let first = cache.warm_urls(urls.clone()).expect("first warm");
    handle.join().expect("image server");
    assert_eq!((first.newly_cached, first.total_urls), (1, 1));
    assert_eq!(
        fs::read(cache.path_for_url(&url)).expect("cached render"),
        body
    );
    // The one-shot server is gone; a second warm must not need it.
    let second = cache.warm_urls(urls).expect("second warm");
    assert_eq!((second.newly_cached, second.total_urls), (0, 1));
}

#[test]
fn images_over_the_reader_limit_are_never_cached() {
    // Complete PNG framing around more bytes than any reader accepts.
    let mut oversized = PNG_FIXTURE[..8].to_vec();
    oversized.resize(MAX_CACHED_IMAGE_BYTES + 1, 0);
    oversized.extend_from_slice(&PNG_FIXTURE[8..]);
    for content_length in [true, false] {
        let root = tempdir().expect("cache directory");
        let cache = TriviaImageCache::new(root.path()).expect("cache");
        let (server, handle) = one_shot_server(&oversized, content_length);
        let url = format!("{server}/apps/dota_react/heroes/renders/huge.png");

        let error = cache
            .get_or_download(&url)
            .expect_err("an oversized image must not be served");
        handle.join().expect("image server");

        assert!(
            matches!(error, TriviaImageCacheError::TooLarge { .. }),
            "{error:?}"
        );
        let path = cache.path_for_url(&url);
        assert!(!path.exists(), "content length sent: {content_length}");
        assert!(!path.with_extension("png.partial").exists());
    }
}

#[test]
fn images_that_look_complete_but_do_not_decode_are_never_cached() {
    let mut garbled = png(4, 4);
    // Ruin the compressed pixels but keep the PNG framing intact.
    let pixels = garbled
        .windows(4)
        .position(|window| window == b"IDAT")
        .expect("pixel chunk")
        + 4;
    for byte in &mut garbled[pixels..pixels + 6] {
        *byte ^= 0xff;
    }
    for body in [PNG_FIXTURE.to_vec(), garbled] {
        let root = tempdir().expect("cache directory");
        let cache = TriviaImageCache::new(root.path()).expect("cache");
        let (server, handle) = one_shot_image_server(&body);
        let url = format!("{server}/apps/dota_react/heroes/corrupt.png");

        let error = cache
            .get_or_download(&url)
            .expect_err("bytes that do not decode must not be served");
        handle.join().expect("image server");

        assert!(
            matches!(error, TriviaImageCacheError::InvalidImage { .. }),
            "{error:?}"
        );
        assert!(!cache.path_for_url(&url).exists());
    }
}

#[test]
fn hero_renders_are_cached_at_most_900_px_with_their_alpha() {
    let large = png(1200, 300);
    let root = tempdir().expect("cache directory");
    let cache = TriviaImageCache::new(root.path()).expect("cache");

    let (server, handle) = one_shot_image_server(&large);
    let render = format!("{server}/apps/dota2/videos/dota_react/heroes/renders/big_hero.png");
    let image = cache.get_or_download(&render).expect("download render");
    handle.join().expect("image server");
    let cached = fs::read(cache.path_for_url(&render)).expect("cached render");
    assert_eq!(image.bytes, cached);
    let decoded = crate::pet_assets::decode_png_raster(&cached).expect("cached render decodes");
    assert_eq!((decoded.width, decoded.height), (900, 225));
    let pixel = |x: usize, y: usize| {
        let at = (y * decoded.width + x) * 4;
        decoded.pixels[at..at + 4].to_vec()
    };
    assert_eq!(pixel(10, 100), [255, 0, 0, 255]);
    assert_eq!(pixel(890, 100)[3], 0, "the clear half stays clear");

    // Portraits are cached exactly as served.
    let (server, handle) = one_shot_image_server(&large);
    let portrait = format!("{server}/apps/dota2/images/dota_react/heroes/big_hero.png");
    let image = cache.get_or_download(&portrait).expect("download portrait");
    handle.join().expect("image server");
    assert_eq!(image.bytes, large);
    assert_eq!(fs::read(cache.path_for_url(&portrait)).unwrap(), large);
}
