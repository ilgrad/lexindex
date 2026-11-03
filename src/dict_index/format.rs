//! The blob: header, sections, the public layout types, parse and load.

use super::*;

/// Where a [`DictIndex`] blob's bytes go, section by section — [`DictIndex::sections`].
///
/// The byte fields sum to [`total`](Self::total), which is
/// [`serialized_len`](DictIndex::serialized_len); the three counts are keys and entries, not bytes.
/// The front-coded data is split the way it is written: one header byte an entry, the `(lcp, len)`
/// pair of the entries too wide for that byte, and the symbol-coded suffixes — so a corpus whose
/// cost is its headers can be told from one whose cost is its suffixes, which is the question a
/// change to the format has to answer first.
///
/// ```
/// # use lexindex::DictIndex;
/// let index = DictIndex::build(["apple", "apricot", "banana"])?;
/// let s = index.sections();
/// assert_eq!(s.total() as usize, index.serialized_len());
/// // Three keys in one block: one head, no restarts, two front-coded entries.
/// assert_eq!((s.restarts, s.entries), (0, 2));
/// # Ok::<(), lexindex::IndexError>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DictSections {
    /// The fixed header.
    pub header: u64,
    /// The symbol tables, one per shard of blocks.
    pub tables: u64,
    /// The header codes, two per shard of blocks: what an entry's `(lcp, len)` pair is written as.
    pub header_codes: u64,
    /// The phrase dictionary the whole blob shares, zero where no shard bought phrases.
    pub phrases: u64,
    /// Every block's first key, stored whole.
    pub heads: u64,
    /// Packed: where each block's head ends.
    pub head_ends: u64,
    /// Packed: where each block's data starts.
    pub block_offsets: u64,
    /// Packed: where each microblock's entries start. Empty when a block is one microblock.
    pub micro_offsets: u64,
    /// The character code the keys are spelled in; zero where they are spelled in UTF-8.
    pub chars: u64,
    /// The restarts' coded `(lcp, len)` pairs.
    pub restart_headers: u64,
    /// The `(lcp, len)` varints of the restarts too wide for a one-byte header.
    pub restart_wide: u64,
    /// The restarts' symbol-coded suffixes.
    pub restart_codes: u64,
    /// The front-coded entries' coded `(lcp, len)` pairs, under whichever code the shard took.
    pub entry_headers: u64,
    /// The `(lcp, len)` varints of the entries too wide for a one-byte header.
    pub entry_wide: u64,
    /// The entries' symbol-coded suffixes, which is where most of a blob goes.
    pub entry_codes: u64,
    /// Restarts stored: one per microblock past each block's first.
    pub restarts: u64,
    /// Front-coded entries stored: every key that is neither a block head nor a restart.
    pub entries: u64,
    /// How many of those two needed the wide header, the count the one-byte form is chosen on.
    pub wide: u64,
}

impl DictSections {
    /// The byte fields' sum, which is the blob.
    pub fn total(&self) -> u64 {
        self.header
            + self.tables
            + self.header_codes
            + self.phrases
            + self.heads
            + self.head_ends
            + self.block_offsets
            + self.micro_offsets
            + self.chars
            + self.restart_headers
            + self.restart_wide
            + self.restart_codes
            + self.entry_headers
            + self.entry_wide
            + self.entry_codes
    }
}

/// A block size named for what an index is wanted for, for a caller who would rather not pick the
/// number. Each name keeps its meaning while the block it stands for follows the measurements, and
/// anything between them is still a number: [`DictIndex::build_with_block`] takes `1..=1024`.
///
/// On real words the three store **2.85 / 2.64 / 2.51** bytes a key, answer `id` in 253–254 /
/// 281–283 / 341–342 ns and `key_into` in 141–142 / 198–199 / 307; all three are under
/// `marisa-trie`'s 2.955 floor on that corpus.
///
/// ```
/// use lexindex::{DictIndex, DictProfile};
/// let index = DictIndex::build_with_block(["fig", "kiwi"], DictProfile::Compact.block())?;
/// assert_eq!(index.block(), 1024);
/// # Ok::<(), lexindex::IndexError>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DictProfile {
    /// A block is one microblock, so a lookup scans it whole and no restart is stored to reach
    /// one: the fast end of the published curve.
    Fast,
    /// What [`DictIndex::build`] uses.
    Balanced,
    /// The largest block a blob can name.
    Compact,
}

impl DictProfile {
    /// Keys a block holds under this profile, for the builders that take a block.
    pub const fn block(self) -> usize {
        match self {
            Self::Fast => 32,
            Self::Balanced => DEFAULT_BLOCK,
            Self::Compact => MAX_BLOCK,
        }
    }
}

/// How a [`DictIndex`] lays its keys out: the block, the microblock and whether to try a label
/// dictionary. [`DictIndex::build_with`] takes one; [`DictIndex::build`] uses the default, which
/// is [`DictProfile::Balanced`]'s block with the microblocks the block chooses and no labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct DictLayout {
    /// Keys a block holds, `1..=1024`: what a stored head and its arrays are shared over.
    pub block: usize,
    /// Keys a microblock holds, `1..=block` and dividing it; `None` is the 16 to 32 the block
    /// chooses by itself. See [`DictIndex::build_with_layout`].
    pub micro: Option<usize>,
    /// Try a dictionary of the corpus's trie labels and keep it where the blob comes out smaller.
    /// See [`DictIndex::build_with`].
    pub labels: bool,
}

impl Default for DictLayout {
    fn default() -> Self {
        Self::new(DEFAULT_BLOCK)
    }
}

impl DictLayout {
    /// `block` keys a block, the microblocks the block chooses, no labels.
    pub const fn new(block: usize) -> Self {
        Self {
            block,
            micro: None,
            labels: false,
        }
    }

    /// The same layout with `micro` keys a microblock.
    pub const fn with_micro(self, micro: usize) -> Self {
        Self {
            micro: Some(micro),
            ..self
        }
    }

    /// The same layout, trying a label dictionary or not.
    pub const fn with_labels(self, labels: bool) -> Self {
        Self { labels, ..self }
    }
}

impl DictIndex {
    /// The payload sections in order, each handed to `f` once, from where they are; the symbol
    /// table and the phrases, the two the index does not hold as their serialised bytes, go out
    /// in pieces so that nothing the size of the index is copied.
    fn write_sections(
        &self,
        mut f: impl FnMut(&[u8]) -> Result<(), IndexError>,
    ) -> Result<(), IndexError> {
        f(&self.heads)?;
        for section in self.head_ends.sections() {
            f(section)?;
        }
        f(&self.data)?;
        let mut tables = Vec::with_capacity(codecs_len(&self.codecs));
        write_codecs(&self.codecs, &mut tables);
        f(&tables)?;
        let mut codes = Vec::with_capacity(codes_len(&self.codes));
        write_codes(&self.codes, &mut codes);
        f(&codes)?;
        let mut phrases = Vec::with_capacity(self.phrases.serialized_len());
        self.phrases.write_to(&mut phrases);
        f(&phrases)?;
        for section in self.blocks.sections() {
            f(section)?;
        }
        f(self.micros.section())?;
        if let Some(code) = &self.code {
            let mut chars = Vec::with_capacity(code.serialized_len());
            code.write_to(&mut chars);
            f(&chars)?;
        }
        Ok(())
    }

    /// The character code's bytes, zero for a blob without one.
    fn chars_len(&self) -> usize {
        self.code.as_deref().map_or(0, CharCode::serialized_len)
    }

    fn header(&self) -> [u8; HEADER] {
        let mut hasher = crate::blob::BlockHasher::new();
        self.write_sections(|s| {
            hasher.update(s);
            Ok(())
        })
        .expect("hashing the sections cannot fail");
        let mut h = [0u8; HEADER];
        debug_assert!(!(self.code.is_some() && self.phrases.has_labels()));
        h[0..4].copy_from_slice(if self.code.is_some() {
            CODED_MAGIC
        } else if self.phrases.has_labels() {
            LABELLED_MAGIC
        } else {
            MAGIC
        });
        h[4..12].copy_from_slice(&(self.n as u64).to_le_bytes());
        h[12..16].copy_from_slice(&(self.block as u32).to_le_bytes());
        h[16..24].copy_from_slice(&(self.heads.len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&(self.data.len() as u64).to_le_bytes());
        h[32..36].copy_from_slice(&(codecs_len(&self.codecs) as u32).to_le_bytes());
        h[36..44].copy_from_slice(&hasher.finish().to_le_bytes());
        h[44] = self.head_ends.width() as u8;
        h[45] = self.blocks.width() as u8;
        h[46] = self.head_ends.shift() as u8;
        h[47] = self.micros.width() as u8;
        h[48..50].copy_from_slice(&(self.micro as u16).to_le_bytes());
        h[50..52].copy_from_slice(&(self.shard as u16).to_le_bytes());
        h[52..56].copy_from_slice(&(codes_len(&self.codes) as u32).to_le_bytes());
        h[56..58].copy_from_slice(&(self.g as u16).to_le_bytes());
        h[58..62].copy_from_slice(&(self.phrases.serialized_len() as u32).to_le_bytes());
        h[62..66].copy_from_slice(&(self.chars_len() as u32).to_le_bytes());
        let check = crate::blob::hash_bytes(&h[..CHECKED]) as u32;
        h[CHECKED..HEADER].copy_from_slice(&check.to_le_bytes());
        h
    }

    /// Serialise to `[magic "BDX3" or "BDX4"][n][block][head bytes][data bytes][codec bytes]
    /// [payload][offset widths][micro][shard][header-code bytes][g][dictionary bytes]
    /// [character-code bytes][check]`, then the head keys, the packed head ends, the block data,
    /// the suffix codecs, the header codes, the phrase dictionary, the two start arrays and, in a
    /// `BDX4` blob, the character code. `check` is a hash of the preceding header bytes and
    /// `payload` a hash of everything after it, both verified on load.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_len());
        out.extend_from_slice(&self.header());
        self.write_sections(|s| {
            out.extend_from_slice(s);
            Ok(())
        })
        .expect("appending the sections cannot fail");
        out
    }

    /// Length of the [`to_bytes`](Self::to_bytes) blob in bytes, without producing it.
    pub fn serialized_len(&self) -> usize {
        HEADER
            + codecs_len(&self.codecs)
            + codes_len(&self.codes)
            + self.phrases.serialized_len()
            + self.heads.len()
            + self.head_ends.len()
            + self.blocks.len()
            + self.micros.len()
            + self.data.len()
            + self.chars_len()
    }

    /// Where this index's bytes go, section by section: [`DictSections`].
    ///
    /// Walks every block rather than reading the header, so it costs a pass over the blob. The
    /// sections sum to [`serialized_len`](Self::serialized_len) whatever the blob holds.
    ///
    /// ```
    /// # use lexindex::DictIndex;
    /// let index = DictIndex::build(["apple", "apricot", "avocado", "banana"])?;
    /// let s = index.sections();
    /// assert_eq!(s.total() as usize, index.to_bytes().len());
    /// assert!(s.entry_codes > 0 && s.heads > 0);
    /// # Ok::<(), lexindex::IndexError>(())
    /// ```
    pub fn sections(&self) -> DictSections {
        let mut s = DictSections {
            header: HEADER as u64,
            tables: codecs_len(&self.codecs) as u64,
            header_codes: codes_len(&self.codes) as u64,
            phrases: self.phrases.serialized_len() as u64,
            heads: self.heads.len() as u64,
            head_ends: self.head_ends.len() as u64,
            block_offsets: self.blocks.len() as u64,
            micro_offsets: self.micros.len() as u64,
            chars: self.chars_len() as u64,
            ..DictSections::default()
        };
        for b in 0..self.blocks_len() {
            let micros = self.micros_in(b);
            // A block that is one microblock stores no restart run, and its region is empty.
            if micros > 1 {
                let run = split_run(
                    &self.codes_of(b).1,
                    self.codec_of(b),
                    self.restart_data(b),
                    micros,
                );
                s.restart_headers += run.headers;
                s.restart_wide += run.wide_bytes;
                s.restart_codes += run.codes;
                s.restarts += run.entries;
                s.wide += run.wide;
            }
            for j in 0..micros {
                let run = split_run(
                    &self.codes_of(b).0,
                    self.codec_of(b),
                    self.micro_data(b, j),
                    self.micro_count(b, j),
                );
                s.entry_headers += run.headers;
                s.entry_wide += run.wide_bytes;
                s.entry_codes += run.codes;
                s.entries += run.entries;
                s.wide += run.wide;
            }
        }
        s
    }

    /// Reconstruct from [`DictIndex::to_bytes`] output.
    ///
    /// Safe on arbitrary bytes: the magic, both checksums, every section length, the symbol
    /// table and the four per-block arrays are checked before anything is trusted, so a
    /// crafted blob is at worst *wrong* — a key that is not the one built, a shorter walk —
    /// never out of bounds. The block data itself is read with every access bounded.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::copy_of(bytes), true)
    }

    /// The loader behind every way in. The framing — magic, header checksum, block size, the
    /// section lengths against the blob — is checked always and the symbol table parsed; with
    /// `verify`, the payload checksum and [`check_layout`](Self::check_layout) as well, which
    /// read every section. Without it the sections are borrowed as they are and the accessors
    /// bound what the arrays say, so a mapping loads without touching its pages.
    pub(crate) fn from_shared(blob: SharedBytes, verify: bool) -> Result<Self, IndexError> {
        let bytes: &[u8] = &blob;
        if let Some((_, why)) = LEGACY_MAGIC.iter().find(|(m, _)| bytes.starts_with(*m)) {
            return Err(IndexError::Format(why));
        }
        let (coded, labelled) = match bytes.get(..4) {
            _ if bytes.len() < HEADER => None,
            Some(m) if m == MAGIC => Some((false, false)),
            Some(m) if m == CODED_MAGIC => Some((true, false)),
            Some(m) if m == LABELLED_MAGIC => Some((false, true)),
            _ => None,
        }
        .ok_or(IndexError::Format("bad magic or truncated header"))?;
        let check = u32::from_le_bytes(bytes[CHECKED..HEADER].try_into().unwrap());
        if check != crate::blob::hash_bytes(&bytes[..CHECKED]) as u32 {
            return Err(IndexError::Format("header checksum mismatch"));
        }
        if verify {
            let stored = u64::from_le_bytes(bytes[36..44].try_into().unwrap());
            if stored != crate::blob::hash_block(&bytes[HEADER..]) {
                return Err(IndexError::Format("payload checksum mismatch"));
            }
        }
        let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let n = usize::try_from(u64_at(4))
            .map_err(|_| IndexError::Format("dict: key count out of range"))?;
        let block = u32_at(12) as usize;
        if !(1..=MAX_BLOCK).contains(&block) {
            return Err(IndexError::Format("dict: block size out of range"));
        }
        let heads_len = usize::try_from(u64_at(16))
            .map_err(|_| IndexError::Format("dict: head bytes out of range"))?;
        let data_len = usize::try_from(u64_at(24))
            .map_err(|_| IndexError::Format("dict: block data out of range"))?;
        let table_len = u32_at(32) as usize;
        let (head_width, block_width, shift, micro_width) = (
            u32::from(bytes[44]),
            u32::from(bytes[45]),
            u32::from(bytes[46]),
            u32::from(bytes[47]),
        );
        if head_width > offsets::MAX_WIDTH
            || block_width > offsets::MAX_WIDTH
            || micro_width > offsets::MAX_WIDTH
            || shift >= 32
        {
            return Err(IndexError::Format("dict: offset widths out of range"));
        }
        let micro = u16::from_le_bytes(bytes[48..50].try_into().unwrap()) as usize;
        if !(1..=block).contains(&micro) {
            return Err(IndexError::Format("dict: microblock size out of range"));
        }
        let shard = u16::from_le_bytes(bytes[50..52].try_into().unwrap()) as usize;
        if shard == 0 {
            return Err(IndexError::Format("dict: symbol-table shard out of range"));
        }
        let codes_len = u32_at(52) as usize;
        let g = usize::from(u16::from_le_bytes(bytes[56..58].try_into().unwrap()));
        let phrase_len = u32_at(58) as usize;
        // `BDX3`'s bytes here were reserved and went unread, so they are still not read there.
        let chars_len = if coded { u32_at(62) as usize } else { 0 };
        let nb = n.div_ceil(block);
        let shards = nb.div_ceil(shard).max(1);
        // Every microblock holds at least one key, so the count is bounded by the key count and
        // the product below cannot overflow.
        let per = block.div_ceil(micro);
        let nm = match nb {
            0 => 0,
            _ if per == 1 => 0,
            nb => (nb - 1) * per + (n - (nb - 1) * block).min(block).div_ceil(micro),
        };
        let arrays = arrays_len(nb, nm, head_width, block_width, micro_width, shift)
            .ok_or(IndexError::Format("dict: block count out of range"))?;
        let total = HEADER
            .checked_add(table_len)
            .and_then(|t| t.checked_add(heads_len))
            .and_then(|t| t.checked_add(arrays))
            .and_then(|t| t.checked_add(data_len))
            .and_then(|t| t.checked_add(codes_len))
            .and_then(|t| t.checked_add(phrase_len))
            .and_then(|t| t.checked_add(chars_len));
        if total != Some(bytes.len()) {
            return Err(IndexError::Format(
                "dict: the section lengths do not add up to the blob",
            ));
        }
        let mut at = HEADER;
        let mut take = |len: usize| {
            at += len;
            blob.subslice(at - len, at)
                .expect("the sections add up to the blob")
        };
        let heads = take(heads_len);
        let head_ends = Offsets::new(
            take(offsets::bases_len(nb, shift)),
            take(offsets::deltas_len(nb, head_width)),
            head_width,
            shift,
        );
        let samples = (0..nb)
            .map(|b| sample_at(head_of(&heads, &head_ends, b), g))
            .collect();
        let data = take(data_len);
        let codecs = {
            let section = take(table_len);
            let bad = || IndexError::Format("dict: bad symbol table");
            let mut codecs = Vec::with_capacity(shards);
            let mut at = 0usize;
            for _ in 0..shards {
                let len = section
                    .get(at..at + 4)
                    .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
                    .ok_or_else(bad)?;
                let raw = section.get(at + 4..at + 4 + len).ok_or_else(bad)?;
                codecs.push(Codec::read(raw).ok_or_else(bad)?);
                at += 4 + len;
            }
            if at != section.len() {
                return Err(bad());
            }
            codecs
        };
        let codes = {
            let section = take(codes_len);
            let bad = || IndexError::Format("dict: bad header code");
            let mut codes = Vec::with_capacity(shards);
            let mut rest: &[u8] = &section;
            for _ in 0..shards {
                let (micro_code, tail) = Code::read(rest).ok_or_else(bad)?;
                let (restart_code, tail) = Code::read(tail).ok_or_else(bad)?;
                codes.push((micro_code, restart_code));
                rest = tail;
            }
            if !rest.is_empty() {
                return Err(bad());
            }
            codes
        };
        let phrases = {
            let section = take(phrase_len);
            let bad = || IndexError::Format("dict: bad phrase dictionary");
            if labelled {
                let flat = section
                    .get(..4)
                    .map(|b| u32::from_le_bytes(b.try_into().expect("4 bytes")) as usize)
                    .ok_or_else(bad)?;
                let end = flat.checked_add(4).ok_or_else(bad)?;
                let dict = Dict::read(section.subslice(4, end).ok_or_else(bad)?).ok_or_else(bad)?;
                let labels = crate::labels::Labels::read(
                    section.subslice(end, section.len()).ok_or_else(bad)?,
                    verify,
                )?;
                if dict.phrases() + labels.len() > phrase::IDS_MAX {
                    return Err(IndexError::Format("dict: bad label dictionary"));
                }
                dict.with_labels(labels)
            } else {
                Dict::read(section).ok_or_else(bad)?
            }
        };
        let blocks = Offsets::new(
            take(offsets::bases_len(nb, shift)),
            take(offsets::deltas_len(nb, block_width)),
            block_width,
            shift,
        );
        let micros = offsets::Packed::new(take(offsets::deltas_len(nm, micro_width)), micro_width);
        let code = match coded {
            false => None,
            true => Some(Box::new(
                CharCode::read(&take(chars_len))
                    .ok_or(IndexError::Format("dict: bad character code"))?,
            )),
        };
        let mut idx = Self {
            block,
            micro,
            per,
            n,
            heads,
            head_ends,
            samples,
            ties: None,
            blocks,
            micros,
            data,
            codecs,
            codes,
            shard,
            shard_bits: shard_bits_of(shard),
            phrases,
            g,
            code,
            raw: None,
            mroute: OnceLock::new(),
        };
        if verify {
            idx.check_layout()?;
        }
        idx.ties = Ties::derive(&idx.samples, |b| idx.head(b));
        idx.raw = idx.raw_route();
        Ok(idx)
    }

    /// The array invariants every query relies on: heads and blocks in order and inside their
    /// sections, every sample the one its head gives.
    fn check_layout(&self) -> Result<(), IndexError> {
        let nb = self.blocks_len();
        if nb == 0 {
            return if self.heads.is_empty() && self.data.is_empty() {
                Ok(())
            } else {
                Err(IndexError::Format("dict: an empty index with key bytes"))
            };
        }
        let mut prev = 0;
        for end in (0..nb).map(|b| self.head_end(b)) {
            if end < prev || end > self.heads.len() {
                return Err(IndexError::Format("dict: head table out of order"));
            }
            prev = end;
        }
        if prev != self.heads.len() {
            return Err(IndexError::Format(
                "dict: head table does not cover the head bytes",
            ));
        }
        // The two start arrays interleave: a block opens on its restart run, then its microblocks
        // in order, and the last of them closes where the next block opens. One sweep over both
        // says every span is forward and the last one ends the data.
        let out_of_order = IndexError::Format("dict: block table out of order");
        let mut prev = 0;
        for b in 0..nb {
            let start = self.block_start(b);
            if start < prev {
                return Err(out_of_order);
            }
            prev = start;
            for j in 0..self.micros_in(b) {
                let at = self.micro_start(b, j);
                if at < prev {
                    return Err(out_of_order);
                }
                prev = at;
            }
            let end = self.block_end(b) as u64;
            if end < prev {
                return Err(out_of_order);
            }
            prev = end;
        }
        if self.block_start(0) != 0 || prev != self.data.len() as u64 {
            return Err(out_of_order);
        }
        if self.g > common_head(&self.heads, &self.head_ends, nb) {
            return Err(IndexError::Format(
                "dict: the head prefix is longer than the heads share",
            ));
        }
        Ok(())
    }

    /// The queries the fuzz shim puts to every blob. Short, so the pairwise range probes stay
    /// cheap, and spread over the byte order so a prefix range is sometimes empty, sometimes the
    /// whole index, and sometimes neither. The last three are what a character code spells or
    /// places: one Latin, one Cyrillic and one Chinese character.
    #[cfg(feature = "fuzzing")]
    const FUZZ_PROBES: [&str; 9] = [
        "",
        "\u{0}",
        "a",
        "ab",
        "zzzzzzzzzzzzzzzzz",
        "\u{10FFFF}",
        "\u{e9}",
        "\u{44f}",
        "\u{4e2d}",
    ];

    /// Whether `bytes` loads, and whether what loaded answers without panicking — by the checked
    /// path and by the mapping's, which takes the arrays as they are, both as loaded and with the
    /// mapping's restart words derived. Exists for the libFuzzer target in `fuzz/`; see the
    /// `lexindex::fuzzing` module.
    #[cfg(feature = "fuzzing")]
    pub(crate) fn fuzz_load_and_query(bytes: &[u8]) -> bool {
        let checked = Self::from_bytes(bytes).ok();
        let framed = Self::from_shared(SharedBytes::copy_of(bytes), false).ok();
        assert!(
            checked.is_none() || framed.is_some(),
            "the framing is the checked path's"
        );
        let routed = Self::from_shared(SharedBytes::copy_of(bytes), false).ok();
        if let Some(idx) = &routed {
            idx.route_microblocks();
        }
        for idx in checked.iter().chain(&framed).chain(&routed) {
            let n = idx.len() as u64;
            for probe in Self::FUZZ_PROBES {
                assert!(idx.id(probe).is_none_or(|id| id < n), "{probe:?}");
                assert!(idx.lower_bound(probe) <= n, "{probe:?}");
                let r = idx.prefix_id_range(probe);
                assert!(
                    r.start <= r.end && r.end <= n,
                    "prefix_id_range({probe:?}) = {r:?} over {n} keys"
                );
                assert_eq!(idx.prefix_count(probe), r.end - r.start, "{probe:?}");
                assert!(
                    idx.successor(probe).is_none_or(|(_, id)| id < n),
                    "successor({probe:?}) past {n}"
                );
                assert!(
                    idx.predecessor(probe).is_none_or(|(_, id)| id < n),
                    "predecessor({probe:?}) past {n}"
                );
                for hi in Self::FUZZ_PROBES {
                    assert!(
                        idx.range_count(probe, hi) <= n,
                        "range_count({probe:?}, {hi:?})"
                    );
                }
                assert!(
                    idx.prefix_iter(probe).take(64).all(|(_, id)| id < n),
                    "prefix({probe:?}) past {n}"
                );
                assert!(
                    idx.range_iter(probe, "zzzz").take(64).all(|(_, id)| id < n),
                    "range({probe:?}, ..) past {n}"
                );
                assert!(
                    idx.iter_after(probe).take(64).all(|(_, id)| id < n),
                    "iter_after({probe:?}) past {n}"
                );
            }
            for id in [0, 1, n / 2, n.saturating_sub(1), n, u64::MAX] {
                assert!(id < n || idx.key(id).is_none(), "key({id}) past {n}");
            }
            assert!(idx.iter().take(64).count() as u64 <= n);
        }
        // On a blob that passed every check, the ordered surface must agree with a walk -- the
        // property the unit tests assert on indexes this crate built, here over one a fuzzer did.
        // Two guards keep it from reporting a difference that is not a bug: an index longer than
        // the walk is left alone, and so is one whose block data decodes to something that is not
        // UTF-8, since `iter` is lossy there and `key` refuses, which is a documented difference
        // rather than a disagreement.
        // The restart words are a shortcut over a restart run in order, so where every key walks
        // in order the routed copy must place every probe where the loaded index does: the fixed
        // probes, each key, and each key short of its last character, which falls between two.
        if let (Some(idx), Some(routed)) = (&checked, &routed) {
            let keys: Vec<String> = idx.iter().take(4097).map(|(k, _)| k).collect();
            let whole = keys.len() == idx.len() && keys.len() <= 4096;
            let sorted = keys.windows(2).all(|w| w[0] < w[1]);
            let utf8 = (0..)
                .zip(&keys)
                .all(|(i, k)| idx.key(i).as_ref() == Some(k));
            if whole && sorted && utf8 {
                let between = keys.iter().flat_map(|k| {
                    let cut = k.char_indices().last().map_or(0, |(i, _)| i);
                    [k.as_str(), &k[..cut]]
                });
                for probe in Self::FUZZ_PROBES.into_iter().chain(between) {
                    assert_eq!(routed.id(probe), idx.id(probe), "routed id({probe:?})");
                    assert_eq!(
                        routed.lower_bound(probe),
                        idx.lower_bound(probe),
                        "routed lower_bound({probe:?})"
                    );
                }
            }
        }
        if let Some(idx) = &checked {
            let all: Vec<(String, u64)> = idx.iter().take(257).collect();
            for (i, (_, id)) in all.iter().enumerate() {
                assert_eq!(*id, i as u64, "iter is not in rank order");
            }
            let whole = all.len() == idx.len() && all.len() <= 256;
            let utf8 = all
                .iter()
                .all(|(k, id)| idx.key(*id).as_deref() == Some(k.as_str()));
            if whole && utf8 {
                for lo in Self::FUZZ_PROBES {
                    let want: Vec<(String, u64)> = all
                        .iter()
                        .filter(|(k, _)| k.starts_with(lo))
                        .cloned()
                        .collect();
                    assert_eq!(idx.prefix(lo), want, "prefix({lo:?})");
                    assert_eq!(
                        idx.prefix_count(lo) as usize,
                        want.len(),
                        "prefix_count({lo:?})"
                    );
                    assert_eq!(
                        idx.successor(lo),
                        all.iter().find(|(k, _)| k.as_str() >= lo).cloned(),
                        "successor({lo:?})"
                    );
                    assert_eq!(
                        idx.predecessor(lo),
                        all.iter().rev().find(|(k, _)| k.as_str() <= lo).cloned(),
                        "predecessor({lo:?})"
                    );
                    assert_eq!(
                        idx.iter_after(lo).collect::<Vec<_>>(),
                        all.iter()
                            .filter(|(k, _)| k.as_str() > lo)
                            .cloned()
                            .collect::<Vec<_>>(),
                        "iter_after({lo:?})"
                    );
                    for hi in Self::FUZZ_PROBES {
                        let want: Vec<(String, u64)> = all
                            .iter()
                            .filter(|(k, _)| k.as_str() >= lo && k.as_str() < hi)
                            .cloned()
                            .collect();
                        assert_eq!(idx.range(lo, hi), want, "range({lo:?}, {hi:?})");
                        assert_eq!(
                            idx.range_count(lo, hi) as usize,
                            want.len(),
                            "range_count({lo:?}, {hi:?})"
                        );
                    }
                }
            }
        }
        checked.is_some()
    }

    /// Write the index to `path` — the same bytes as [`to_bytes`](Self::to_bytes), streamed
    /// section by section.
    pub fn save(&self, path: impl AsRef<std::path::Path>) -> Result<(), IndexError> {
        crate::blob::write_atomically_with(path.as_ref(), |w| self.write_to(w))
    }

    pub(crate) fn write_to(&self, w: &mut dyn std::io::Write) -> Result<(), IndexError> {
        w.write_all(&self.header())?;
        self.write_sections(|s| Ok(w.write_all(s)?))
    }

    /// Load an index previously written with [`DictIndex::save`]. Safe on any file — see
    /// [`from_bytes`](Self::from_bytes).
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        Self::from_shared(SharedBytes::from_owned(std::fs::read(path)?), true)
    }

    /// Memory-map the file and borrow it: the heads, the block data and the three offset arrays
    /// are read where they lie, and the load touches the header, the symbol tables and the heads
    /// once more, to build the per-block samples — eight bytes a block, held in memory because the
    /// search over them opens every lookup and the blob does not store them (see the field). Skips the
    /// payload checksum and the walk over the arrays [`load`](Self::load) makes — the mapped file
    /// is trusted intact, and every access bounds what the arrays say.
    ///
    /// # Safety
    /// One obligation, and it is not about the bytes: the file must not be modified or truncated
    /// by any process while the returned index is alive, because the index borrows the mapping.
    /// A crafted file is *not* undefined behaviour here — the framing is checked, and the rest
    /// is read with every access bounded — it is merely wrong. See
    /// [`StringIndex::load_mmap`](crate::StringIndex::load_mmap) for the full contract.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap(path: impl AsRef<std::path::Path>) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), false)
    }

    /// [`load_mmap`](Self::load_mmap) plus the checks [`load`](Self::load) makes — the payload
    /// checksum and the walk over the per-block arrays — one pass over the mapping at load,
    /// pages still shared and nothing copied. For a file you wrote but did not carry yourself.
    ///
    /// # Safety
    /// The same obligation as [`load_mmap`](Self::load_mmap): the file must not change while the
    /// index is alive. The checks run once, at load, and say nothing about later.
    #[cfg(feature = "mmap")]
    #[cfg_attr(docsrs, doc(cfg(feature = "mmap")))]
    pub unsafe fn load_mmap_verified(
        path: impl AsRef<std::path::Path>,
    ) -> Result<Self, IndexError> {
        let file = std::fs::File::open(path)?;
        // SAFETY: forwarded from this function's own contract.
        let mmap = unsafe { memmap2::Mmap::map(&file)? };
        Self::from_shared(SharedBytes::from_mmap(std::sync::Arc::new(mmap)), true)
    }
}
