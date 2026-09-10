//! What a blob is, from its header alone. [`inspect`] names the kind and the format and reads the
//! sizes a caller would otherwise have to load the blob to learn: nothing is decoded, nothing is
//! verified, so it is total on any bytes and instant on any size. A blob from before 1.0 is named
//! as such, with the type to rebuild.

use crate::IndexError;

/// Which structure wrote a blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BlobKind {
    StringIndex,
    PerfectHashIndex,
    CompactHashIndex,
    ClosedHashIndex,
    /// A standalone minimal perfect hash, the region the two hash indexes embed.
    Mphf,
    Overlay,
}

/// What [`inspect`] reads out of a blob's header.
///
/// Every field comes from the framing and none is checked against the contents: a blob that
/// inspects cleanly may still fail to load, and the sizes are what the header claims. The checksums
/// are the loaders' business.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BlobInfo {
    pub kind: BlobKind,
    /// The four-byte magic as text — `"BMP6"` — which is the format version.
    pub format: String,
    /// The whole blob, in bytes.
    pub bytes: u64,
    /// Keys the blob holds; for an overlay, the live keys. `None` only for an overlay whose base
    /// this crate did not write, since its key count is inside a header this crate cannot read.
    pub keys: Option<u64>,
    /// `CompactHashIndex`: the fingerprint width.
    pub fingerprint_bits: Option<u32>,
    /// The minimal perfect hash's region, for the kinds that hold one; `8 * mph_bytes / keys` is
    /// its bits per key.
    pub mph_bytes: Option<u64>,
    /// `PerfectHashIndex`: the key arena; `CompactHashIndex`: the fingerprint table.
    pub arena_bytes: Option<u64>,
    /// Keys in the hash indexes' collision side table.
    pub side_entries: Option<u64>,
    /// What an overlay carries on top of its base.
    pub overlay: Option<OverlayInfo>,
}

/// An overlay's own sections, and its base inspected in turn.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OverlayInfo {
    /// [`OverlayBase::BASE_TAG`](crate::OverlayBase::BASE_TAG) of the base.
    pub base_tag: u8,
    /// The base region inspected, when the tag is one of this crate's.
    pub base: Option<Box<BlobInfo>>,
    /// Keys added on top of the base.
    pub additions: u64,
    /// Ids retired by a removal.
    pub retired: u64,
}

const TRUNCATED: IndexError =
    IndexError::Format("blob truncated: a length in the header runs past the end");

/// A blob by ranges, so that one parser serves a slice and a file read in pieces.
trait Source {
    fn len(&self) -> u64;
    fn read(&mut self, at: u64, len: usize) -> Result<Vec<u8>, IndexError>;
}

impl Source for &[u8] {
    fn len(&self) -> u64 {
        <[u8]>::len(self) as u64
    }

    fn read(&mut self, at: u64, len: usize) -> Result<Vec<u8>, IndexError> {
        let at = usize::try_from(at).map_err(|_| TRUNCATED)?;
        let end = at
            .checked_add(len)
            .filter(|&e| e <= <[u8]>::len(self))
            .ok_or(TRUNCATED)?;
        Ok(self[at..end].to_vec())
    }
}

struct FileSource {
    file: std::fs::File,
    len: u64,
}

impl Source for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read(&mut self, at: u64, len: usize) -> Result<Vec<u8>, IndexError> {
        use std::io::{Read, Seek, SeekFrom};
        self.file.seek(SeekFrom::Start(at))?;
        let mut buf = vec![0u8; len];
        match self.file.read_exact(&mut buf) {
            Ok(()) => Ok(buf),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Err(TRUNCATED),
            Err(e) => Err(e.into()),
        }
    }
}

/// A region of a source: the whole blob, or the base inside an overlay.
struct Window<'a> {
    src: &'a mut dyn Source,
    start: u64,
    len: u64,
}

impl Window<'_> {
    fn bytes(&mut self, at: u64, n: usize) -> Result<Vec<u8>, IndexError> {
        at.checked_add(n as u64)
            .filter(|&end| end <= self.len)
            .ok_or(TRUNCATED)?;
        self.src.read(self.start + at, n)
    }

    fn u32(&mut self, at: u64) -> Result<u32, IndexError> {
        Ok(u32::from_le_bytes(
            self.bytes(at, 4)?.try_into().expect("4 bytes"),
        ))
    }

    fn u64(&mut self, at: u64) -> Result<u64, IndexError> {
        Ok(u64::from_le_bytes(
            self.bytes(at, 8)?.try_into().expect("8 bytes"),
        ))
    }

    fn sub(&mut self, start: u64, len: u64) -> Result<Window<'_>, IndexError> {
        start
            .checked_add(len)
            .filter(|&end| end <= self.len)
            .ok_or(TRUNCATED)?;
        Ok(Window {
            src: self.src,
            start: self.start + start,
            len,
        })
    }
}

fn info(kind: BlobKind, format: String, bytes: u64, keys: u64) -> BlobInfo {
    BlobInfo {
        kind,
        format,
        bytes,
        keys: Some(keys),
        fingerprint_bits: None,
        mph_bytes: None,
        arena_bytes: None,
        side_entries: None,
        overlay: None,
    }
}

/// `whole - parts`, or the truncation error when the parts do not fit.
fn rest(whole: u64, parts: [u64; 3]) -> Result<u64, IndexError> {
    parts
        .iter()
        .try_fold(whole, |acc, &p| acc.checked_sub(p))
        .ok_or(TRUNCATED)
}

fn parse(w: &mut Window) -> Result<BlobInfo, IndexError> {
    let magic = w
        .bytes(0, 4)
        .map_err(|_| IndexError::Format("not a lexindex blob: shorter than a magic"))?;
    let format = String::from_utf8_lossy(&magic).into_owned();
    let bytes = w.len;
    match &magic[..] {
        b"BIX4" => {
            // `[magic 4][fst]`; the transducer's footer is `[len u64][root u64][check u32]` from
            // its third format on, and `[len u64][root u64]` before that.
            let version = w.u64(4)?;
            let footer = if version >= 3 { 20 } else { 16 };
            let keys = w.u64(bytes.checked_sub(footer).ok_or(TRUNCATED)?)?;
            Ok(info(BlobKind::StringIndex, format, bytes, keys))
        }
        b"BMP5" | b"BMP6" => {
            // `[magic 4][n u64][mph_len u64][side_len u32][payload u64][check u32]`
            w.bytes(0, 36)?;
            let (n, mph, side) = (w.u64(4)?, w.u64(12)?, u64::from(w.u32(20)?));
            let mut i = info(BlobKind::PerfectHashIndex, format, bytes, n);
            i.mph_bytes = Some(mph);
            i.side_entries = Some(side);
            i.arena_bytes = Some(rest(bytes, [36, mph, side * 12])?);
            Ok(i)
        }
        b"BCH6" => {
            // `[magic 4][n u64][fp_bits u32][mph_len u64][side_len u32][payload u64][check u32]`
            w.bytes(0, 40)?;
            let (n, fp, mph, side) = (w.u64(4)?, w.u32(12)?, w.u64(16)?, u64::from(w.u32(24)?));
            let mut i = info(BlobKind::CompactHashIndex, format, bytes, n);
            i.fingerprint_bits = Some(fp);
            i.mph_bytes = Some(mph);
            i.side_entries = Some(side);
            i.arena_bytes = Some(rest(bytes, [40, mph, side * 20])?);
            Ok(i)
        }
        b"BCL1" => {
            // `[magic 4][n u64][mph_len u64][side_len u32][payload u64][check u32]`
            w.bytes(0, 36)?;
            let (n, mph, side) = (w.u64(4)?, w.u64(12)?, u64::from(w.u32(20)?));
            let mut i = info(BlobKind::ClosedHashIndex, format, bytes, n);
            i.mph_bytes = Some(mph);
            i.side_entries = Some(side);
            // Nothing follows the side table; a blob shorter than its header claims is truncated.
            rest(bytes, [36, mph, side * 20])?;
            Ok(i)
        }
        b"MPH1" | b"MPH2" => {
            // `[magic 4][version u16][reserved u16][n u64]…`
            let n = w.u64(8)?;
            let mut i = info(BlobKind::Mphf, format, bytes, n);
            i.mph_bytes = Some(bytes);
            Ok(i)
        }
        b"OVL2" => {
            // `[magic 4][tag u8][base len u64][additions u64][addition bytes u64][tombstone
            // words u64][payload u64][check u32]`, then the three sections in that order.
            w.bytes(0, 49)?;
            let tag = w.bytes(4, 1)?[0];
            let (base_len, additions, added_bytes, words) =
                (w.u64(5)?, w.u64(13)?, w.u64(21)?, w.u64(29)?);
            let tail = rest(bytes, [49, base_len, added_bytes])?;
            if tail != words.checked_mul(8).ok_or(TRUNCATED)? {
                return Err(IndexError::Format(
                    "overlay tombstone length disagrees with the blob",
                ));
            }
            let retired = popcount(w, 49 + base_len + added_bytes, words)?;
            overlay(w, format, bytes, tag, 49, base_len, additions, retired)
        }
        b"OVL1" => {
            // `[magic 4][tag u8][base len u64][additions u64]`, then the base, the additions
            // (`u32` length, bytes), the tombstone word count as a `u64`, and the words.
            w.bytes(0, 21)?;
            let tag = w.bytes(4, 1)?[0];
            let (base_len, additions) = (w.u64(5)?, w.u64(13)?);
            let mut at = 21u64.checked_add(base_len).ok_or(TRUNCATED)?;
            for _ in 0..additions {
                let len = u64::from(w.u32(at)?);
                at = at.checked_add(4 + len).ok_or(TRUNCATED)?;
            }
            let words = w.u64(at)?;
            if rest(bytes, [at, 8, 0])? != words.checked_mul(8).ok_or(TRUNCATED)? {
                return Err(IndexError::Format(
                    "overlay tombstone length disagrees with the blob",
                ));
            }
            let retired = popcount(w, at + 8, words)?;
            overlay(w, format, bytes, tag, 21, base_len, additions, retired)
        }
        b"BMP1" | b"BMP2" | b"BMP3" | b"BMP4" => Err(IndexError::Format(
            "a PerfectHashIndex blob from lexindex < 1.0, whose perfect hash this version cannot \
             read; rebuild it from its keys with PerfectHashIndex::build",
        )),
        b"BCH1" | b"BCH2" | b"BCH3" | b"BCH4" | b"BCH5" => Err(IndexError::Format(
            "a CompactHashIndex blob from lexindex < 1.0, whose perfect hash this version cannot \
             read and which stores no keys; rebuild it from its keys with CompactHashIndex::build",
        )),
        _ => Err(IndexError::Format("not a lexindex blob: unknown magic")),
    }
}

/// Set bits over `words` tombstone words at `at`.
fn popcount(w: &mut Window, at: u64, words: u64) -> Result<u64, IndexError> {
    let n = usize::try_from(words.checked_mul(8).ok_or(TRUNCATED)?).map_err(|_| TRUNCATED)?;
    Ok(w.bytes(at, n)?
        .chunks_exact(8)
        .map(|c| u64::from(u64::from_le_bytes(c.try_into().expect("8 bytes")).count_ones()))
        .sum())
}

#[allow(clippy::too_many_arguments)]
fn overlay(
    w: &mut Window,
    format: String,
    bytes: u64,
    tag: u8,
    header: u64,
    base_len: u64,
    additions: u64,
    retired: u64,
) -> Result<BlobInfo, IndexError> {
    let base = match tag {
        1..=3 => Some(Box::new(parse(&mut w.sub(header, base_len)?)?)),
        _ => None,
    };
    let keys = base
        .as_ref()
        .and_then(|b| b.keys)
        .map(|k| k + additions - retired);
    Ok(BlobInfo {
        kind: BlobKind::Overlay,
        format,
        bytes,
        keys,
        fingerprint_bits: None,
        mph_bytes: None,
        arena_bytes: None,
        side_entries: None,
        overlay: Some(OverlayInfo {
            base_tag: tag,
            base,
            additions,
            retired,
        }),
    })
}

/// What `bytes` is, from its header: the kind, the format and the sizes, without decoding or
/// verifying any of it. See [`BlobInfo`].
///
/// ```
/// # use lexindex::{inspect, BlobKind, StringIndex};
/// let blob = StringIndex::build(["apple", "banana"])?.to_bytes();
/// let info = inspect(&blob)?;
/// assert_eq!((info.kind, info.format.as_str(), info.keys), (BlobKind::StringIndex, "BIX4", Some(2)));
/// # Ok::<(), lexindex::IndexError>(())
/// ```
///
/// A blob from before 1.0 is an error that names the type to rebuild; bytes that are not a
/// lexindex blob at all, or a header whose lengths run past the end, are errors too. Total on
/// any input, like the loaders.
pub fn inspect(bytes: &[u8]) -> Result<BlobInfo, IndexError> {
    let mut src: &[u8] = bytes;
    let len = Source::len(&src);
    parse(&mut Window {
        src: &mut src,
        start: 0,
        len,
    })
}

/// [`inspect`] over a file, reading only the header and the footer it needs rather than the
/// file — an index of gigabytes inspects in microseconds.
pub fn inspect_file(path: impl AsRef<std::path::Path>) -> Result<BlobInfo, IndexError> {
    let file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut src = FileSource { file, len };
    parse(&mut Window {
        src: &mut src,
        start: 0,
        len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Overlay, StringIndex};

    fn keys() -> Vec<String> {
        (0..300).map(|i| format!("key-{i:04}")).collect()
    }

    #[test]
    fn a_string_index_inspects_to_its_key_count() {
        let blob = StringIndex::build(keys()).unwrap().to_bytes();
        let i = inspect(&blob).unwrap();
        assert_eq!(i.kind, BlobKind::StringIndex);
        assert_eq!(i.format, "BIX4");
        assert_eq!((i.keys, i.bytes), (Some(300), blob.len() as u64));
        assert_eq!(
            (
                i.mph_bytes,
                i.arena_bytes,
                i.side_entries,
                i.fingerprint_bits
            ),
            (None, None, None, None)
        );
        assert!(i.overlay.is_none());
        let empty = StringIndex::build(Vec::<String>::new()).unwrap().to_bytes();
        assert_eq!(inspect(&empty).unwrap().keys, Some(0));
    }

    #[cfg(feature = "mph")]
    #[test]
    fn the_hash_indexes_inspect_to_sections_that_add_up() {
        let blob = crate::PerfectHashIndex::build(keys())
            .unwrap()
            .to_bytes()
            .unwrap();
        let i = inspect(&blob).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys),
            (BlobKind::PerfectHashIndex, "BMP6", Some(300))
        );
        assert_eq!(
            36 + i.mph_bytes.unwrap() + i.arena_bytes.unwrap() + 12 * i.side_entries.unwrap(),
            i.bytes
        );
        assert!(
            i.arena_bytes.unwrap() >= 300 * 8,
            "the arena holds every key"
        );

        let blob = crate::CompactHashIndex::build(keys(), 2)
            .unwrap()
            .to_bytes()
            .unwrap();
        let i = inspect(&blob).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys),
            (BlobKind::CompactHashIndex, "BCH6", Some(300))
        );
        assert_eq!(i.fingerprint_bits, Some(16));
        assert_eq!(
            40 + i.mph_bytes.unwrap() + i.arena_bytes.unwrap() + 20 * i.side_entries.unwrap(),
            i.bytes
        );
        assert_eq!(i.arena_bytes, Some(300 * 2), "one fingerprint per key");

        let blob = crate::ClosedHashIndex::build(keys()).unwrap().to_bytes();
        let i = inspect(&blob).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys),
            (BlobKind::ClosedHashIndex, "BCL1", Some(300))
        );
        assert_eq!((i.fingerprint_bits, i.arena_bytes), (None, None));
        assert_eq!(
            36 + i.mph_bytes.unwrap() + 20 * i.side_entries.unwrap(),
            i.bytes
        );

        let hashes: Vec<u64> = (1..=300u64)
            .map(|k| k.wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .collect();
        let blob = crate::mphf::Mphf::build(&hashes).unwrap().to_bytes();
        let i = inspect(&blob).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys, i.mph_bytes),
            (BlobKind::Mphf, "MPH2", Some(300), Some(blob.len() as u64))
        );
    }

    #[test]
    fn an_overlay_inspects_its_base_and_its_edits() {
        let idx = StringIndex::build(keys()).unwrap();
        let base_bytes = idx.to_bytes().len() as u64;
        let mut ov = Overlay::new(idx);
        ov.add("added-1");
        ov.add("added-2");
        assert!(ov.remove("key-0000") && ov.remove("added-1"));
        let blob = ov.to_bytes().unwrap();
        let i = inspect(&blob).unwrap();
        assert_eq!(
            (i.kind, i.format.as_str(), i.keys),
            (BlobKind::Overlay, "OVL2", Some(300))
        );
        let o = i.overlay.unwrap();
        assert_eq!((o.base_tag, o.additions, o.retired), (1, 2, 2));
        let base = o.base.unwrap();
        assert_eq!(
            (base.kind, base.keys, base.bytes),
            (BlobKind::StringIndex, Some(300), base_bytes)
        );
    }

    #[test]
    fn a_file_inspects_as_its_bytes_do() {
        let dir = std::env::temp_dir().join(format!("lexindex-inspect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("index.bix");
        let idx = StringIndex::build(keys()).unwrap();
        idx.save(&path).unwrap();
        assert_eq!(
            inspect_file(&path).unwrap(),
            inspect(&idx.to_bytes()).unwrap()
        );
        std::fs::write(&path, b"BMP6 too short").unwrap();
        assert!(
            inspect_file(&path)
                .unwrap_err()
                .to_string()
                .contains("truncated")
        );
        std::fs::write(&path, b"").unwrap();
        assert!(
            inspect_file(&path)
                .unwrap_err()
                .to_string()
                .contains("shorter than a magic")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn what_is_refused_says_why() {
        let msg = |b: &[u8]| inspect(b).unwrap_err().to_string();
        assert!(msg(b"BMP3 and whatever followed").contains("PerfectHashIndex::build"));
        assert!(msg(b"BCH5 and whatever followed").contains("CompactHashIndex::build"));
        assert!(msg(b"nope").contains("unknown magic"));
        assert!(msg(b"BI").contains("shorter than a magic"));
        assert!(msg(b"").contains("shorter than a magic"));
        assert!(msg(b"BMP6 too short").contains("truncated"));
        assert!(msg(b"BIX4").contains("truncated"));
        // A header whose lengths run past the end, whatever the checksums say.
        let mut lying = vec![0u8; 36];
        lying[..4].copy_from_slice(b"BMP6");
        lying[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(msg(&lying).contains("truncated"));
    }
}
