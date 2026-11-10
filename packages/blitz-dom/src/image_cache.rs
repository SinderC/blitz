//! Decoded images, kept by URL so elements showing the same image share one decode.

use std::collections::HashMap;

use crate::node::ImageData;

/// Bytes of decoded images kept at most, per document. A decoded image takes four
/// bytes a pixel, so a long page or list of images would otherwise keep gigabytes.
///
/// Elements hold their own copy of their image, so dropping one here costs only a new
/// fetch and decode when an element asks for it again. It doesn't bound memory
/// outright: an image dropped while an element still shows it is decoded a second time
/// if another element asks for it, and an image larger than this is never kept.
const MAX_BYTES: usize = 128 * 1024 * 1024;

/// What an SVG counts for: it is drawn when painted, not kept as pixels. A rough
/// figure; a complex one's tree can take more.
#[cfg(feature = "svg")]
const SVG_BYTES: usize = 64 * 1024;

/// Decoded images by URL, the least recently used dropped past [`MAX_BYTES`].
#[derive(Default)]
pub(crate) struct ImageCache {
    images: HashMap<String, Entry>,
    bytes: usize,
    /// Bumped by every use, to find the least recently used.
    clock: u64,
}

struct Entry {
    image: ImageData,
    bytes: usize,
    used: u64,
}

impl ImageCache {
    /// The image at `url`, if cached, now the most recently used.
    pub(crate) fn get(&mut self, url: &str) -> Option<&ImageData> {
        self.clock += 1;
        let entry = self.images.get_mut(url)?;
        entry.used = self.clock;
        Some(&entry.image)
    }

    /// Keeps `image` as the one at `url`, dropping the least recently used images
    /// while the cache is over its size. An image larger than the whole cache isn't
    /// kept.
    pub(crate) fn insert(&mut self, url: String, image: ImageData) {
        let bytes = size(&image);
        if let Some(old) = self.images.remove(&url) {
            self.bytes -= old.bytes;
        }
        if bytes > MAX_BYTES {
            return;
        }
        while self.bytes + bytes > MAX_BYTES {
            let Some(oldest) = self
                .images
                .iter()
                .min_by_key(|(_, e)| e.used)
                .map(|(url, _)| url.clone())
            else {
                break;
            };
            if let Some(e) = self.images.remove(&oldest) {
                self.bytes -= e.bytes;
            }
        }
        self.clock += 1;
        self.bytes += bytes;
        self.images.insert(
            url,
            Entry {
                image,
                bytes,
                used: self.clock,
            },
        );
    }
}

fn size(image: &ImageData) -> usize {
    match image {
        ImageData::Raster(raster) => raster.data.len(),
        #[cfg(feature = "svg")]
        ImageData::Svg(_) => SVG_BYTES,
        ImageData::None => 0,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::node::RasterImageData;

    fn image(bytes: usize) -> ImageData {
        ImageData::Raster(RasterImageData::new(1, 1, Arc::new(vec![0; bytes])))
    }

    #[test]
    fn the_least_recently_used_image_goes_first_past_the_size() {
        let mut cache = ImageCache::default();
        let third = MAX_BYTES / 3;
        cache.insert("a".into(), image(third));
        cache.insert("b".into(), image(third));
        cache.insert("c".into(), image(third));
        assert!(cache.get("a").is_some(), "a is now the latest used");

        cache.insert("d".into(), image(third));

        assert!(cache.get("b").is_none());
        assert!(cache.get("a").is_some() && cache.get("c").is_some());
        assert!(cache.get("d").is_some());
        assert!(cache.bytes <= MAX_BYTES);
    }

    #[test]
    fn an_image_larger_than_the_cache_is_not_kept() {
        let mut cache = ImageCache::default();
        cache.insert("a".into(), image(10));
        cache.insert("huge".into(), image(MAX_BYTES + 1));
        assert!(cache.get("huge").is_none());
        assert!(cache.get("a").is_some());
    }
}
