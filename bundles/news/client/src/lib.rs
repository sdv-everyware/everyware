//! The news client of the news bundle: the top stories of Hacker News (its public API,
//! hacker-news.firebaseio.com) and, for each story with a link, the article's own description
//! and picture as the page announces them (Open Graph: `og:description`, `og:image`; the plain
//! `description` and `twitter:image` when those are missing), and the picture made into the
//! thumbnail the screen shows (JPEG or PNG decoded, cropped to 4:3, 120 x 90 RGB; a WebP
//! picture is not decoded and the story keeps its placeholder). A crate of its own so the
//! parsing has a host test without SDV.

use everyware_https::Url;
use std::error::Error;

/// The error of a fetch.
pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const API: &str = "https://hacker-news.firebaseio.com/v0";
/// The list of the top stories is a few kilobytes, an item less than one.
const MAX_JSON: usize = 256 * 1024;
/// A page is read up to this size; the head with the meta tags comes first.
const MAX_PAGE: usize = 1536 * 1024;
/// The summary the feed carries, at most (bytes, cut at a word).
pub const SUMMARY_MAX: usize = 300;
/// A picture is read up to this size.
const MAX_PICTURE: usize = 4 * 1024 * 1024;
/// The thumbnail: the image slot of a list item (lines three).
pub const THUMB_W: u32 = 120;
pub const THUMB_H: u32 = 90;

/// A story of Hacker News.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Story {
    pub id: u64,
    pub title: String,
    /// The link; empty for a story without one (Ask HN).
    pub url: String,
    /// The host of the link, without "www.".
    pub site: String,
    pub by: String,
    pub points: u32,
    pub comments: u32,
    /// Unix time.
    pub time: i64,
}

/// What a page announces about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub description: Option<String>,
    /// Resolved against the page's URL; https only.
    pub image: Option<String>,
}

/// The ids of the top stories, best first.
pub async fn top_ids() -> Result<Vec<u64>> {
    let body = everyware_https::get_text(&format!("{API}/topstories.json"), MAX_JSON).await?;
    Ok(serde_json::from_str(&body)?)
}

/// A story by its id; None for an item that is not a live story.
pub async fn story(id: u64) -> Result<Option<Story>> {
    let body = everyware_https::get_text(&format!("{API}/item/{id}.json"), MAX_JSON).await?;
    Ok(parse_story(&body))
}

/// The description and the picture of the page at `url`.
pub async fn page_meta(url: &str) -> Result<Meta> {
    let response = everyware_https::get(url, MAX_PAGE).await?;
    if !response.content_type.is_empty() && !response.content_type.contains("html") {
        return Ok(Meta::default());
    }
    let html = String::from_utf8_lossy(&response.body);
    Ok(parse_meta(&html, &Url::parse(&response.url)?))
}

/// The thumbnail of the picture at `url`: RGB, `THUMB_W` x `THUMB_H`.
pub async fn picture(url: &str) -> Result<Vec<u8>> {
    let response = everyware_https::get(url, MAX_PICTURE).await?;
    thumbnail(&response.body)
}

/// A picture (JPEG, PNG) cropped to the thumbnail's aspect around its centre and scaled down:
/// RGB, `THUMB_W` x `THUMB_H`, row by row.
pub fn thumbnail(bytes: &[u8]) -> Result<Vec<u8>> {
    let img = image::load_from_memory(bytes)?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("an empty picture".into());
    }
    // the largest THUMB_W:THUMB_H rectangle in the middle
    let (cw, ch) = if w * THUMB_H > h * THUMB_W { (h * THUMB_W / THUMB_H, h) } else { (w, w * THUMB_H / THUMB_W) };
    let cropped = img.crop_imm((w - cw) / 2, (h - ch) / 2, cw.max(1), ch.max(1));
    let small = cropped.resize_exact(THUMB_W, THUMB_H, image::imageops::FilterType::Triangle);
    Ok(small.to_rgb8().into_raw())
}

/// An item of the API (JSON) as a story.
pub fn parse_story(json: &str) -> Option<Story> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if v["type"] != "story" || v["dead"] == true || v["deleted"] == true {
        return None;
    }
    let url = v["url"].as_str().unwrap_or("").to_string();
    Some(Story {
        id: v["id"].as_u64()?,
        title: v["title"].as_str()?.to_string(),
        site: site_of(&url),
        url,
        by: v["by"].as_str().unwrap_or("").to_string(),
        points: v["score"].as_u64().unwrap_or(0) as u32,
        comments: v["descendants"].as_u64().unwrap_or(0) as u32,
        time: v["time"].as_i64().unwrap_or(0),
    })
}

/// "example.org" of "https://www.example.org/a"; empty for no URL.
pub fn site_of(url: &str) -> String {
    let rest = url.split_once("://").map_or("", |(_, r)| r);
    let host = rest.split(['/', '?', '#', ':']).next().unwrap_or("").to_ascii_lowercase();
    host.strip_prefix("www.").map(str::to_string).unwrap_or(host)
}

/// The meta tags of a page: og:description (else description) and og:image (else
/// twitter:image), the image resolved against `base`.
pub fn parse_meta(html: &str, base: &Url) -> Meta {
    let mut og_desc = None;
    let mut desc = None;
    let mut og_image = None;
    let mut tw_image = None;
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<meta") {
        let start = at + i;
        let Some(len) = lower[start..].find('>') else { break };
        let tag = &html[start..start + len];
        at = start + len;
        let key = attr(tag, "property").or_else(|| attr(tag, "name")).map(|k| k.to_ascii_lowercase());
        let Some(content) = attr(tag, "content") else { continue };
        let content = decode_entities(&content);
        match key.as_deref() {
            Some("og:description") => og_desc = og_desc.or(Some(content)),
            Some("description") => desc = desc.or(Some(content)),
            Some("og:image") | Some("og:image:secure_url") => og_image = og_image.or(Some(content)),
            Some("twitter:image") | Some("twitter:image:src") => tw_image = tw_image.or(Some(content)),
            _ => {}
        }
        // the meta tags are in the head
        if lower[at..].trim_start().starts_with("</head") {
            break;
        }
    }
    let image = og_image.or(tw_image).and_then(|i| base.join(&i).ok()).map(|u| u.to_url());
    let description = og_desc.or(desc).map(|d| summary(&d, SUMMARY_MAX)).filter(|d| !d.is_empty());
    Meta { description, image }
}

/// The value of attribute `name` in a tag (`name="v"`, `name='v'` or `name=v`).
fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let pos = from + i;
        from = pos + name.len();
        // a whole attribute name, not the end of another ("og:name")
        if pos > 0 && !lower.as_bytes()[pos - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        return Some(match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next()?.to_string(),
            _ => rest.split(|c: char| c.is_whitespace() || c == '/' || c == '>').next()?.to_string(),
        });
    }
    None
}

/// The entities a meta content uses (`&amp;`, `&quot;`, `&#39;`, `&#x27;` …).
pub fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest[..rest.len().min(10)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let c = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The whitespace collapsed and the text cut at a word to `max` bytes, with an ellipsis.
pub fn summary(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.len() <= max {
        return flat;
    }
    let mut cut = max.saturating_sub('…'.len_utf8());
    while !flat.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &flat[..cut];
    let head = head.rfind(' ').map_or(head, |i| &head[..i]);
    format!("{}…", head.trim_end_matches([',', ';', ':', '.', ' ']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_story_of_the_api() {
        let s = parse_story(r#"{"by":"alice","descendants":45,"id":42,"score":120,"time":1762430000,"title":"A title","type":"story","url":"https://www.example.org/post"}"#).unwrap();
        assert_eq!((s.id, s.title.as_str(), s.site.as_str(), s.points, s.comments), (42, "A title", "example.org", 120, 45));
        assert!(parse_story(r#"{"id":1,"type":"comment","text":"x"}"#).is_none());
        assert!(parse_story(r#"{"id":2,"type":"story","title":"t","dead":true}"#).is_none());
        let ask = parse_story(r#"{"id":3,"type":"story","title":"Ask HN: x","by":"b","time":1}"#).unwrap();
        assert_eq!((ask.url.as_str(), ask.site.as_str()), ("", ""));
    }

    #[test]
    fn the_meta_of_a_page() {
        let base = Url::parse("https://example.org/blog/post").unwrap();
        let html = r#"<html><head><meta charset="utf-8"><meta name="description" content="plain">
            <meta property="og:description" content="Tom &amp; Jerry&#39;s   story">
            <meta content="/img/cover.jpg" property="og:image" /></head><body><meta property="og:image" content="late.jpg"></body>"#;
        let m = parse_meta(html, &base);
        assert_eq!(m.description.as_deref(), Some("Tom & Jerry's story"));
        assert_eq!(m.image.as_deref(), Some("https://example.org/img/cover.jpg"));
        let m = parse_meta(r#"<meta name="twitter:image" content="http://insecure.org/a.png"><meta name=description content=short>"#, &base);
        assert_eq!((m.description.as_deref(), m.image), (Some("short"), None));
    }

    #[test]
    fn a_thumbnail_of_a_picture() {
        // a wide red picture with a blue middle: the crop keeps the middle
        let mut img = image::RgbImage::from_pixel(400, 150, image::Rgb([255, 0, 0]));
        for x in 150..250 {
            for y in 0..150 {
                img.put_pixel(x, y, image::Rgb([0, 0, 255]));
            }
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let rgb = thumbnail(&png).unwrap();
        assert_eq!(rgb.len(), (THUMB_W * THUMB_H * 3) as usize);
        let mid = ((THUMB_H / 2 * THUMB_W + THUMB_W / 2) * 3) as usize;
        assert_eq!(&rgb[mid..mid + 3], &[0, 0, 255]);
        assert!(thumbnail(b"not a picture").is_err());
    }

    #[test]
    fn a_summary_is_cut_at_a_word() {
        assert_eq!(summary("  a   b\n c ", 300), "a b c");
        let s = summary(&"word ".repeat(100), 30);
        assert!(s.len() <= 30 && s.ends_with('…'), "{s}");
        assert_eq!(summary("äöü äöü äöü", 12), "äöü…");
    }
}
