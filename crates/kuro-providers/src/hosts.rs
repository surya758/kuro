//! Embed-host allowlist.
//!
//! Provider episode pages carry ad and tracker iframes alongside the real player —
//! frequently *before* it. Taking the first `<iframe>` on the page reliably picks an
//! ad, so embed extraction matches against this list instead.

/// Hosts known to serve video embeds. Matched as a domain suffix, so
/// `geo.dailymotion.com` and `www.dailymotion.com` both match `dailymotion.com`.
const VIDEO_HOSTS: &[&str] = &[
    "archive.org",
    "bigwarp.io",
    "dailymotion.com",
    "dood.watch",
    "doodstream.com",
    "filemoon.sx",
    "lulustream.com",
    "mixdrop.co",
    "mp4upload.com",
    "ok.ru",
    "rumble.com",
    "sendvid.com",
    "streamplay.co.in",
    "streamtape.com",
    "streamwish.to",
    "vidhide.com",
    "vk.com",
    "voe.sx",
    "youtube.com",
    "youtube-nocookie.com",
];

pub fn is_video_host(host: &str) -> bool {
    let host = host.trim_start_matches("www.").to_ascii_lowercase();
    VIDEO_HOSTS
        .iter()
        .any(|known| host == *known || host.ends_with(&format!(".{known}")))
}

/// Human-facing label for an embed host, e.g. `geo.dailymotion.com` → `Dailymotion`.
pub fn host_label(host: &str) -> String {
    let host = host.trim_start_matches("www.").to_ascii_lowercase();

    let base = VIDEO_HOSTS
        .iter()
        .find(|known| host == **known || host.ends_with(&format!(".{known}")))
        .copied()
        .unwrap_or(&host);

    let name = base.split('.').next().unwrap_or(base);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_subdomains_but_not_lookalikes() {
        assert!(is_video_host("rumble.com"));
        assert!(is_video_host("geo.dailymotion.com"));
        assert!(is_video_host("www.dailymotion.com"));
        // A domain that merely *ends with* the text must not match.
        assert!(!is_video_host("notrumble.com"));
        assert!(!is_video_host("sads.adsboosters.xyz"));
    }

    #[test]
    fn labels_are_derived_from_the_base_domain() {
        assert_eq!(host_label("geo.dailymotion.com"), "Dailymotion");
        assert_eq!(host_label("rumble.com"), "Rumble");
    }
}
