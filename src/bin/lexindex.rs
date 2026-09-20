//! `lexindex`, the command line: price the indexes on a corpus, build one, say what a blob is, and
//! read the keys back out of it.
//!
//! A shell over `lexindex::plan`, the builders, `lexindex::inspect_file` and the indexes' own key
//! iterators, which computes nothing of its own — the ladder `plan` prints is `Plan`'s own
//! `Display`, and `--index auto` builds the index that ladder puts first. `main` is three lines
//! over `run`, so the argument parsing, the four subcommands and every error path are reachable
//! from a unit test.

use lexindex::{
    BlobInfo, BlobKind, DictIndex, DictProfile, IndexError, Kind, Needs, Objective, Overlay,
    OverlayBase, Plan, StringIndex, Workload, plan_file, plan_for,
};
use std::io::{BufRead, Write};
use std::path::Path;

#[cfg(feature = "mph")]
use lexindex::{ClosedHashIndex, CompactHashIndex, PerfectHashIndex};

const USAGE: &str = concat!(
    "lexindex ",
    env!("CARGO_PKG_VERSION"),
    r#" — price, build and inspect string↔id indexes

usage:
  lexindex plan    <keys-file> [--objective NAME] [needs]
  lexindex build   <keys-file> <out-blob> [--index NAME] [--block SPEC] [--stream MODE]
                   [--objective NAME] [needs]
  lexindex inspect <blob> [--sections]
  lexindex dump    <blob>

  plan     what every index that answers the needs would weigh, cheapest first
  build    build one index and save it; `--index auto` asks plan and builds the winner
  inspect  what a blob already on disk is, from its header alone; `--sections` reads a
           DictIndex whole and prints where its bytes went
  dump     the keys a blob holds, one per line, in id order — `dump | build` is the
           migration path off a format a later version stops reading

needs — what the index must be able to do. They narrow what `plan` ranks and what
`--index auto` may pick; with none of them the only question asked is `id(key)`:
  --reverse    key(id) as well as id(key)
  --ordered    ids in lexicographic order, and in-order iteration
  --prefix     prefix and range queries (an ordered index)
  --fuzzy      Levenshtein and subsequence queries
  --exact      a non-member must be answered as one, barring the probabilistic indexes

`plan` and `--index auto` assume an **open** vocabulary and so imply `--exact`: left to itself
the ranking is won by the index that answers a stranger with some member's id, and nothing in
the blob would later say so. Pass `--closed-vocabulary` when every key you will ask about is in
the file, and the two probabilistic indexes are ranked with the rest — `ClosedHashIndex` at
0.24 bytes a key, `CompactHashIndex` at 1.24. Naming an index with `--index` builds it whatever
the needs say.

options (`--name value` or `--name=value`):
  --objective N  what the ladder ranks by: memory (default, the smallest blob), latency
                 (the fastest id(key), modelled), balanced (nearest to both), or a
                 workload of op=weight pairs between commas, as in hits=9,misses=1 —
                 ops hits, misses, reverse, prefix, common_prefix, longest_prefix, and
                 batch=N for the keys an ids_of call holds. An op adds the need it asks
                 for. The nanoseconds are a model of this crate's own machine, never a
                 measurement of yours
  --index NAME   auto (default), dict, string, compact, closed, perfect
  --block SPEC   keys per DictIndex block, with `--index dict`: 1..=1024, or one of
                 fast / balanced / compact (32 / 256 / 1024)
  --stream MODE  when to build straight to the blob instead of holding the corpus:
                 auto (default), always, never, or the keys-file size past which to do
                 it (1000, 512M, 4G). `auto` is a quarter of the memory the machine
                 says is available. Needs a named `--index`: `auto` prices the corpus,
                 which means reading it
  --sections     with `inspect`, on a DictIndex: the blob's byte split
  -h, --help     this text
  -V, --version  the version

A keys file is one key per line, UTF-8, in any order; `-` reads standard input. Duplicates
are removed by the builders. An empty line is skipped and the count is reported on stderr,
since a file that ends in a newline is the common case and an empty key is not. Invalid UTF-8
is an error naming the line rather than a replacement character, since an index built from
lossy bytes would hold keys the file does not.
"#
);

/// What went wrong, and whose fault it was.
#[derive(Debug)]
enum Fail {
    /// The command line did not parse. The usage text follows it and the exit code is 2.
    Usage(String),
    /// The work itself failed: a file that is not there, a blob that is not one, a build that
    /// could not finish. Exit code 1.
    Failed(String),
}

impl From<IndexError> for Fail {
    fn from(e: IndexError) -> Self {
        Fail::Failed(e.to_string())
    }
}

fn io_fail(e: std::io::Error) -> Fail {
    Fail::Failed(e.to_string())
}

/// Which optional arguments a subcommand takes. One outside its command's set is a usage error
/// rather than a silent no-op.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Accepts {
    /// `dump`: the blob, and nothing else.
    Nothing,
    /// `inspect`: the blob, and whether to read it whole.
    Sections,
    /// `plan`: the needs.
    Needs,
    /// `build`: the needs, plus what to build and how.
    NeedsAndIndex,
}

/// `--index`: one named index, or the planner's pick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Choice {
    Auto,
    One(Kind),
}

/// One subcommand's arguments, parsed.
#[derive(Debug)]
struct Cmd {
    positional: Vec<String>,
    needs: Needs,
    index: Choice,
    block: Option<usize>,
    sections: bool,
    stream: Stream,
    objective: Objective,
    /// Whether `--exact` was added because nothing said the vocabulary was closed. Only then is
    /// the ladder worth a line about what it left out.
    implied_exact: bool,
}

fn objective(name: &str) -> Result<Objective, Fail> {
    Ok(match name {
        "memory" => Objective::Memory,
        "latency" => Objective::Latency,
        "balanced" => Objective::Balanced,
        spec if spec.contains('=') => Objective::Workload(workload(spec)?),
        other => {
            return Err(Fail::Usage(format!(
                "--objective: `{other}` is not one of memory / latency / balanced, or a workload \
                 such as hits=9,misses=1"
            )));
        }
    })
}

/// `--objective hits=9,common_prefix=1,batch=64`: a workload, one `op=weight` between commas.
fn workload(spec: &str) -> Result<Workload, Fail> {
    let mut asked = Workload::default();
    for pair in spec.split(',') {
        let bad = || {
            Fail::Usage(format!(
                "--objective: `{pair}` is not op=weight, with op one of hits / misses / reverse / \
                 prefix / common_prefix / longest_prefix / batch"
            ))
        };
        let (op, weight) = pair.split_once('=').ok_or_else(bad)?;
        let weight: u64 = weight.parse().map_err(|_| bad())?;
        asked = match op {
            "hits" => asked.hits(weight),
            "misses" => asked.misses(weight),
            "reverse" => asked.reverse(weight),
            "prefix" => asked.prefix(weight),
            "common_prefix" => asked.common_prefix(weight),
            "longest_prefix" => asked.longest_prefix(weight),
            "batch" => asked.batch(u32::try_from(weight).map_err(|_| bad())?),
            _ => return Err(bad()),
        };
    }
    Ok(asked)
}

fn choice(name: &str) -> Result<Choice, Fail> {
    Ok(match name {
        "auto" => Choice::Auto,
        "dict" => Choice::One(Kind::Dict),
        "string" => Choice::One(Kind::String),
        "compact" => Choice::One(Kind::Compact),
        "closed" => Choice::One(Kind::Closed),
        "perfect" => Choice::One(Kind::Perfect),
        other => return Err(Fail::Usage(format!("--index: no such index `{other}`"))),
    })
}

/// `--stream`: when `build` writes the blob as it reads rather than holding the keys.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stream {
    /// Past a quarter of what the machine says is available.
    Auto,
    Always,
    Never,
    /// Past this many bytes of keys file.
    Above(u64),
}

/// Keys-file bytes past which `--stream auto` streams where the machine will not say what it has.
/// A build holds the corpus, the index it is making and the builder's working set at once, so a
/// file this size is already the larger part of a small machine.
const STREAM_FALLBACK: u64 = 1 << 30;

fn stream_mode(spec: &str) -> Result<Stream, Fail> {
    Ok(match spec {
        "auto" => Stream::Auto,
        "always" => Stream::Always,
        "never" => Stream::Never,
        other => Stream::Above(byte_size(other)?),
    })
}

/// A byte count, with an optional `K` / `M` / `G` for the binary multiples.
fn byte_size(spec: &str) -> Result<u64, Fail> {
    let (digits, scale) = match spec.chars().last() {
        Some('K' | 'k') => (&spec[..spec.len() - 1], 1u64 << 10),
        Some('M' | 'm') => (&spec[..spec.len() - 1], 1 << 20),
        Some('G' | 'g') => (&spec[..spec.len() - 1], 1 << 30),
        _ => (spec, 1),
    };
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .ok_or_else(|| {
            Fail::Usage(format!(
                "--stream: `{spec}` is neither a size (1000, 512M, 4G) nor one of auto / always / never"
            ))
        })
}

/// What the kernel says is available, in bytes. `MemAvailable` is the figure that accounts for
/// reclaimable cache, which is what a build actually gets to use; `None` wherever there is no
/// `/proc`, and `--stream auto` falls back to [`STREAM_FALLBACK`] there.
fn available_memory() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = meminfo.lines().find(|l| l.starts_with("MemAvailable:"))?;
    line.split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()?
        .checked_mul(1024)
}

/// The size of the keys file when `build` should stream it, `None` when it should not. Standard
/// input never streams: the dictionary builder reads its source three times and the perfect hash
/// twice, and a pipe cannot be rewound.
fn stream_above(mode: Stream, path: &str) -> Option<u64> {
    let floor = match mode {
        Stream::Never => return None,
        Stream::Always => 0,
        Stream::Above(n) => n,
        // A build holds the corpus, the index it is making and the builder's own working set at
        // once, so a quarter of what is available is the point past which reading the file first
        // stops being the cheap option.
        Stream::Auto => available_memory().map_or(STREAM_FALLBACK, |free| free / 4),
    };
    if path == "-" {
        return None;
    }
    let bytes = std::fs::metadata(path).ok().filter(|m| m.is_file())?.len();
    (bytes >= floor).then_some(bytes)
}

/// A `--block` spec: one of the three named points of the size/speed curve, or the number itself.
/// The bounds are `DictProfile`'s own, so the two cannot drift apart.
fn block_size(spec: &str) -> Result<usize, Fail> {
    let max = DictProfile::Compact.block();
    let n = match spec {
        "fast" => DictProfile::Fast.block(),
        "balanced" => DictProfile::Balanced.block(),
        "compact" => max,
        other => other.parse().map_err(|_| {
            Fail::Usage(format!(
                "--block: `{other}` is neither a number nor one of fast / balanced / compact"
            ))
        })?,
    };
    if (1..=max).contains(&n) {
        Ok(n)
    } else {
        Err(Fail::Usage(format!("--block: {n} is outside 1..={max}")))
    }
}

/// The value of `--name value` or `--name=value`, or a usage error naming the option.
fn value(
    args: &[String],
    at: &mut usize,
    name: &str,
    inline: Option<&str>,
) -> Result<String, Fail> {
    if let Some(v) = inline {
        return Ok(v.to_string());
    }
    let v = args
        .get(*at)
        .cloned()
        .ok_or_else(|| Fail::Usage(format!("{name} needs a value")))?;
    *at += 1;
    Ok(v)
}

/// Parse one subcommand's arguments: `want` positionals, and the options `accepts` allows.
/// `form` is how the command is spelled, for the message when the count is wrong.
fn parse(args: &[String], accepts: Accepts, want: usize, form: &str) -> Result<Cmd, Fail> {
    let mut cmd = Cmd {
        positional: Vec::new(),
        needs: Needs::default(),
        index: Choice::Auto,
        block: None,
        sections: false,
        stream: Stream::Auto,
        objective: Objective::Memory,
        implied_exact: false,
    };
    let mut closed_vocabulary = false;
    let mut at = 0;
    while at < args.len() {
        let arg = &args[at];
        at += 1;
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg.as_str(), None),
        };
        match name {
            "--sections" if accepts == Accepts::Sections => cmd.sections = true,
            "--closed-vocabulary" if !matches!(accepts, Accepts::Nothing | Accepts::Sections) => {
                closed_vocabulary = true;
            }
            "--reverse" | "--ordered" | "--prefix" | "--fuzzy" | "--exact"
                if !matches!(accepts, Accepts::Nothing | Accepts::Sections) =>
            {
                let flag = match name {
                    "--reverse" => &mut cmd.needs.reverse,
                    "--ordered" => &mut cmd.needs.ordered,
                    "--prefix" => &mut cmd.needs.prefix,
                    "--fuzzy" => &mut cmd.needs.fuzzy,
                    _ => &mut cmd.needs.exact,
                };
                *flag = true;
            }
            "--objective" if !matches!(accepts, Accepts::Nothing | Accepts::Sections) => {
                cmd.objective = objective(&value(args, &mut at, name, inline)?)?;
            }
            "--index" if accepts == Accepts::NeedsAndIndex => {
                cmd.index = choice(&value(args, &mut at, name, inline)?)?;
            }
            "--block" if accepts == Accepts::NeedsAndIndex => {
                cmd.block = Some(block_size(&value(args, &mut at, name, inline)?)?);
            }
            "--stream" if accepts == Accepts::NeedsAndIndex => {
                cmd.stream = stream_mode(&value(args, &mut at, name, inline)?)?;
            }
            "-" => cmd.positional.push(arg.clone()),
            other if other.starts_with('-') => {
                return Err(Fail::Usage(format!("no such option `{other}`")));
            }
            _ => cmd.positional.push(arg.clone()),
        }
    }
    if cmd.positional.len() != want {
        return Err(Fail::Usage(format!("expected `lexindex {form}`")));
    }
    // With no needs at all the cheapest index that answers `id(key)` is the one that answers a
    // stranger with some other key's id, and nothing in the output would say so. A tool asked
    // "which index" should not answer with the one whose failure is silent unless the caller has
    // said the vocabulary is closed. The library's `Needs::default()` is unchanged: this is the
    // command line choosing a default for a person, not the API choosing one for a program.
    if !matches!(accepts, Accepts::Nothing | Accepts::Sections)
        && !closed_vocabulary
        && !cmd.needs.exact
    {
        cmd.needs.exact = true;
        cmd.implied_exact = true;
    }
    Ok(cmd)
}

/// The keys of a file, one per line, and how many empty lines were dropped. `-` is standard input.
fn read_keys(path: &str, stdin: &mut dyn BufRead) -> Result<(Vec<String>, usize), Fail> {
    let mut opened;
    let src: &mut dyn BufRead = if path == "-" {
        stdin
    } else {
        opened = std::io::BufReader::new(
            std::fs::File::open(path).map_err(|e| Fail::Failed(format!("{path}: {e}")))?,
        );
        &mut opened
    };
    let (mut keys, mut blank) = (Vec::new(), 0);
    for (i, line) in src.lines().enumerate() {
        // `lines()` reports invalid UTF-8 as an `InvalidData` error, which is what this propagates:
        // the library takes `&str`, and a lossy conversion would index keys the file does not hold.
        let line = line.map_err(|e| Fail::Failed(format!("{path}:{}: {e}", i + 1)))?;
        if line.is_empty() {
            blank += 1;
        } else {
            keys.push(line);
        }
    }
    Ok((keys, blank))
}

fn note_blank(path: &str, blank: usize, err: &mut dyn Write) -> Result<(), Fail> {
    if blank > 0 {
        writeln!(err, "{path}: {blank} empty lines skipped").map_err(io_fail)?;
    }
    Ok(())
}

fn cmd_plan(
    cmd: &Cmd,
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Fail> {
    let path = &cmd.positional[0];
    // A file is priced without being held: `plan_file` sorts it externally and counts the shape
    // during the merge, and the answer is the one `plan` gives for the same keys, to the byte.
    // Standard input cannot be sorted that way -- it cannot be read twice -- so it is read in.
    let ranked = if path == "-" {
        let (keys, blank) = read_keys(path, stdin)?;
        note_blank(path, blank, err)?;
        plan_for(&keys, cmd.needs, cmd.objective)?
    } else {
        plan_file(path, cmd.needs, cmd.objective)?
    };
    write!(out, "{ranked}").map_err(io_fail)?;
    note_implied_exact(cmd, &ranked, err)
}

/// What the ladder left out and how to ask for it back. Printed only when `--exact` was the
/// command line's idea rather than the caller's, since otherwise it says nothing new.
fn note_implied_exact(cmd: &Cmd, ranked: &Plan, err: &mut dyn Write) -> Result<(), Fail> {
    // The plan's needs rather than the command line's: a workload's operations add to them.
    let mut without = ranked.needs();
    without.exact = false;
    // And only when the default is what excluded them: an index that answers nothing but `id(key)`
    // is already ruled out by any other need, and a build without `mph` does not have one at all —
    // in either case the line would name something that was never a candidate.
    if !cmd.implied_exact || !cfg!(feature = "mph") || !Kind::Closed.answers(without) {
        return Ok(());
    }
    writeln!(
        err,
        "excluded: needs exact — CompactHashIndex (a bounded false-positive rate) and \
         ClosedHashIndex (a stranger gets some member's id). Pass --closed-vocabulary if every \
         key you will ask about is in this file, and they are ranked with the rest."
    )
    .map_err(io_fail)
}

fn cmd_build(cmd: &Cmd, stdin: &mut dyn BufRead, err: &mut dyn Write) -> Result<(), Fail> {
    // The plan prices every named `DictIndex` block and ranks them with the rest, so a block
    // alongside `auto` would ask the planner to choose one and then overrule its answer. Naming the
    // index is what naming its block goes with.
    if cmd.block.is_some() && cmd.index != Choice::One(Kind::Dict) {
        return Err(Fail::Usage(
            "--block is the DictIndex block size and needs `--index dict`".to_string(),
        ));
    }
    let (path, dest) = (&cmd.positional[0], Path::new(&cmd.positional[1]));
    let big = stream_above(cmd.stream, path);
    // What to build, decided before the keys are read wherever it can be. `--index auto` over a
    // file prices it with `plan_file`, which does not hold it either, so a ranked build streams as
    // readily as a named one; standard input is the exception, since it cannot be priced and then
    // read again. The block comes back with the kind -- the ladder names one on the winning line,
    // and building the default block instead would write a blob the quote does not describe.
    let decided = match cmd.index {
        Choice::One(kind) => Some((kind, cmd.block)),
        Choice::Auto if path != "-" => {
            let ranked = plan_file(path, cmd.needs, cmd.objective)?;
            write!(err, "{ranked}").map_err(io_fail)?;
            note_implied_exact(cmd, &ranked, err)?;
            let best = ranked.best();
            Some((best.kind, best.block))
        }
        Choice::Auto => None,
    };
    if let Some((kind, block)) = decided {
        if big.is_some() && can_stream(kind) {
            let src = Source::new(path);
            let n = stream_to_file(kind, &src, dest, block)?;
            note_blank(path, src.blank.get(), err)?;
            return note_written(kind, n, dest, err);
        }
        if big.is_some() {
            writeln!(
                err,
                "reading {path}: this file is large enough to build without holding it, but \
                 {} has no streaming build in this configuration.",
                kind.name()
            )
            .map_err(io_fail)?;
        }
    }
    let (keys, blank) = read_keys(path, stdin)?;
    note_blank(path, blank, err)?;
    let (kind, block) = match decided {
        Some(pair) => pair,
        None => {
            let ranked = plan_for(&keys, cmd.needs, cmd.objective)?;
            write!(err, "{ranked}").map_err(io_fail)?;
            note_implied_exact(cmd, &ranked, err)?;
            let best = ranked.best();
            (best.kind, best.block)
        }
    };
    let n = build_and_save(kind, &keys, dest, block)?;
    note_written(kind, n, dest, err)
}

/// What was written, and what it cost a key.
fn note_written(kind: Kind, n: usize, dest: &Path, err: &mut dyn Write) -> Result<(), Fail> {
    let bytes = std::fs::metadata(dest)
        .map_err(|e| Fail::Failed(format!("{}: {e}", dest.display())))?
        .len();
    let per_key = bytes as f64 / n.max(1) as f64;
    writeln!(
        err,
        "wrote {}: {} over {n} keys, {bytes} bytes ({per_key:.2} B/key)",
        dest.display(),
        kind.name()
    )
    .map_err(io_fail)
}

/// Whether `kind` has a streaming builder in this build. Where it does not, `build` falls back to
/// reading the corpus, which either works or fails with its own message about the missing feature.
fn can_stream(kind: Kind) -> bool {
    match kind {
        Kind::Dict | Kind::String => true,
        Kind::Compact | Kind::Closed => cfg!(feature = "mph"),
        // Its file build fills the arena through a mapping.
        Kind::Perfect => cfg!(all(feature = "mph", feature = "mmap")),
    }
}

/// A keys file the streaming builders can read again — the dictionary reads its source three
/// times, the perfect hash twice — skipping empty lines. They take a plain `Iterator`, which
/// cannot carry a failure, so a read error ends the stream and is parked here for the caller to
/// find once the build has returned.
struct Source {
    path: std::path::PathBuf,
    failed: std::rc::Rc<std::cell::RefCell<Option<Fail>>>,
    blank: std::rc::Rc<std::cell::Cell<usize>>,
}

impl Source {
    fn new(path: &str) -> Self {
        Self {
            path: std::path::PathBuf::from(path),
            failed: std::rc::Rc::default(),
            blank: std::rc::Rc::default(),
        }
    }

    /// The keys, one per line. Each call reopens the file, and counts the empty lines of that
    /// pass alone.
    fn keys(&self) -> Box<dyn Iterator<Item = String>> {
        let (failed, blank) = (self.failed.clone(), self.blank.clone());
        blank.set(0);
        let name = self.path.display().to_string();
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(e) => {
                *failed.borrow_mut() = Some(Fail::Failed(format!("{name}: {e}")));
                return Box::new(std::iter::empty());
            }
        };
        let lines = std::io::BufReader::new(file)
            .lines()
            .enumerate()
            // `lines()` reports invalid UTF-8 as an `InvalidData` error, which is what this parks:
            // the library takes `&str`, and a lossy conversion would index keys the file does not
            // hold.
            .map_while(move |(i, line)| match line {
                Ok(line) => Some(line),
                Err(e) => {
                    *failed.borrow_mut() = Some(Fail::Failed(format!("{name}:{}: {e}", i + 1)));
                    None
                }
            })
            .filter(move |line| {
                if line.is_empty() {
                    blank.set(blank.get() + 1);
                }
                !line.is_empty()
            });
        Box::new(lines)
    }
}

/// Build one index straight to `dest`, reading the file rather than holding it.
///
/// The builders publish atomically, but that is a promise against a crash: a source that fails
/// halfway ends their iterator and they finish a whole, short index over what they did read. So
/// the blob lands on a sibling name and is renamed into place only once the source is known to
/// have run to the end — whatever was at `dest` survives a failure.
fn stream_to_file(
    kind: Kind,
    src: &Source,
    dest: &Path,
    block: Option<usize>,
) -> Result<usize, Fail> {
    let part = dest.with_file_name(format!(
        "{}.part.{}",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let built = build_streamed(kind, src, &part, block);
    let failed = src.failed.borrow_mut().take();
    let n = match (built, failed) {
        (Ok(n), None) => n,
        (built, failed) => {
            std::fs::remove_file(&part).ok();
            return Err(failed.unwrap_or_else(|| built.expect_err("a failure or an error")));
        }
    };
    std::fs::rename(&part, dest).map_err(|e| {
        std::fs::remove_file(&part).ok();
        Fail::Failed(format!("{}: {e}", dest.display()))
    })?;
    Ok(n)
}

fn build_streamed(
    kind: Kind,
    src: &Source,
    dest: &Path,
    block: Option<usize>,
) -> Result<usize, Fail> {
    let n = match kind {
        Kind::String => StringIndex::build_to_file(src.keys(), dest)?,
        Kind::Dict => match block {
            Some(b) => DictIndex::build_to_file_with_block(src.keys(), dest, b)?,
            None => DictIndex::build_to_file(src.keys(), dest)?,
        },
        #[cfg(feature = "mph")]
        Kind::Compact => CompactHashIndex::build_to_file(src.keys(), dest, 1)?,
        #[cfg(feature = "mph")]
        Kind::Closed => ClosedHashIndex::build_to_file(src.keys(), dest)?,
        #[cfg(all(feature = "mph", feature = "mmap"))]
        Kind::Perfect => PerfectHashIndex::build_to_file(dest, || src.keys())?,
        #[cfg(not(all(feature = "mph", feature = "mmap")))]
        other => {
            return Err(Fail::Failed(format!(
                "{} has no streaming build without the `mph` and `mmap` features",
                other.name()
            )));
        }
    };
    Ok(n)
}

/// Build one index and save it, returning the distinct keys it holds.
fn build_and_save(
    kind: Kind,
    keys: &[String],
    dest: &Path,
    block: Option<usize>,
) -> Result<usize, Fail> {
    let n = match kind {
        Kind::String => {
            let index = StringIndex::build(keys)?;
            index.save(dest)?;
            index.len()
        }
        Kind::Dict => {
            let index = match block {
                Some(b) => DictIndex::build_with_block(keys, b)?,
                None => DictIndex::build(keys)?,
            };
            index.save(dest)?;
            index.len()
        }
        #[cfg(feature = "mph")]
        Kind::Compact => {
            // One fingerprint byte, which is what `plan` prices `CompactHashIndex` at.
            let index = CompactHashIndex::build(keys, 1)?;
            index.save(dest)?;
            index.len()
        }
        #[cfg(feature = "mph")]
        Kind::Closed => {
            let index = ClosedHashIndex::build(keys)?;
            index.save(dest)?;
            index.len()
        }
        #[cfg(feature = "mph")]
        Kind::Perfect => {
            let index = PerfectHashIndex::build(keys)?;
            index.save(dest)?;
            index.len()
        }
        #[cfg(not(feature = "mph"))]
        other => {
            return Err(Fail::Failed(format!(
                "{} needs the `mph` feature, which this build does not have",
                other.name()
            )));
        }
    };
    Ok(n)
}

/// One key and the newline that ends it. A key that contains a line break cannot be written as a
/// line — a builder would read it back as two keys — so it is an error naming the id rather than a
/// dump that does not round-trip.
fn write_key(out: &mut dyn Write, key: &str, id: u64) -> Result<(), Fail> {
    if key.contains(['\n', '\r']) {
        return Err(Fail::Failed(format!(
            "id {id} holds a line break, which one key per line cannot carry"
        )));
    }
    writeln!(out, "{key}").map_err(io_fail)
}

/// The keys a blob holds, in id order, for the kinds that store them. The point is the round trip:
/// `lexindex dump old.blob | lexindex build - new.blob` rebuilds an index whose format this version
/// writes, which is the migration a refused format would otherwise need a special tool for.
fn cmd_dump(cmd: &Cmd, out: &mut dyn Write) -> Result<(), Fail> {
    let path = &cmd.positional[0];
    let info = lexindex::inspect_file(path).map_err(|e| Fail::Failed(format!("{path}: {e}")))?;
    let mut sink = std::io::BufWriter::new(out);
    let keyless = |what: &str| {
        Fail::Failed(format!(
            "{path}: a {what} stores no keys, so there is nothing to dump — the key list it was \
             built from is the only way back"
        ))
    };
    match info.kind {
        BlobKind::StringIndex => {
            for (key, id) in StringIndex::load(path)?.iter() {
                write_key(&mut sink, &key, id)?;
            }
        }
        BlobKind::DictIndex => {
            for (key, id) in DictIndex::load(path)?.iter() {
                write_key(&mut sink, &key, id)?;
            }
        }
        #[cfg(feature = "mph")]
        BlobKind::PerfectHashIndex => {
            let index = PerfectHashIndex::load(path)?;
            for id in 0..index.len() as u32 {
                let key = index
                    .key(id)
                    .ok_or_else(|| Fail::Failed(format!("{path}: id {id} has no key")))?;
                write_key(&mut sink, key, u64::from(id))?;
            }
        }
        BlobKind::Overlay => dump_overlay(path, &info, &mut sink)?,
        BlobKind::CompactHashIndex => return Err(keyless("CompactHashIndex")),
        BlobKind::ClosedHashIndex => return Err(keyless("ClosedHashIndex")),
        BlobKind::Mphf => return Err(keyless("minimal perfect hash")),
        #[cfg(not(feature = "mph"))]
        other => {
            return Err(Fail::Failed(format!(
                "{path}: {other:?} needs the `mph` feature, which this build does not have"
            )));
        }
        #[cfg(feature = "mph")]
        other => return Err(Fail::Failed(format!("{path}: cannot dump a {other:?}"))),
    }
    sink.flush().map_err(io_fail)
}

/// An overlay's live keys, through the loader its base tag names. The base type is a compile-time
/// parameter, so the tag in the header is what chooses the branch.
fn dump_overlay(path: &str, info: &BlobInfo, out: &mut dyn Write) -> Result<(), Fail> {
    let tag = info
        .overlay
        .as_ref()
        .map(|o| o.base_tag)
        .ok_or_else(|| Fail::Failed(format!("{path}: an overlay without a base tag")))?;
    let keys = if tag == <StringIndex as OverlayBase>::BASE_TAG {
        Overlay::<StringIndex>::load_with(path, StringIndex::from_bytes)?.keys()
    } else {
        #[cfg(feature = "mph")]
        if tag == <PerfectHashIndex as OverlayBase>::BASE_TAG {
            Overlay::<PerfectHashIndex>::load_with(path, PerfectHashIndex::from_bytes)?.keys()
        } else {
            return Err(Fail::Failed(format!(
                "{path}: an overlay over a base tagged {tag} stores no keys of its own to dump"
            )));
        }
        #[cfg(not(feature = "mph"))]
        return Err(Fail::Failed(format!(
            "{path}: an overlay over a base tagged {tag} needs the `mph` feature, which this \
             build does not have"
        )));
    };
    for (id, key) in keys.iter().enumerate() {
        write_key(out, key, id as u64)?;
    }
    Ok(())
}

fn cmd_inspect(cmd: &Cmd, out: &mut dyn Write) -> Result<(), Fail> {
    let path = &cmd.positional[0];
    let info = lexindex::inspect_file(path).map_err(|e| Fail::Failed(format!("{path}: {e}")))?;
    print_info(&info, "", out)?;
    if cmd.sections {
        if info.kind != BlobKind::DictIndex {
            return Err(Fail::Failed(format!(
                "{path}: --sections is a DictIndex's byte split, and this is a {:?}",
                info.kind
            )));
        }
        print_sections(&DictIndex::load(path)?, out)?;
    }
    Ok(())
}

/// Where a `DictIndex`'s bytes went: one line a section, with what it costs a key and what share
/// of the blob it is, since a section is only ever read against those two.
fn print_sections(index: &DictIndex, out: &mut dyn Write) -> Result<(), Fail> {
    let s = index.sections();
    let total = s.total();
    let keys = index.len().max(1) as f64;
    for (name, bytes) in [
        ("header", s.header),
        ("tables", s.tables),
        ("header_codes", s.header_codes),
        ("phrases", s.phrases),
        ("heads", s.heads),
        ("head_ends", s.head_ends),
        ("block_offsets", s.block_offsets),
        ("micro_offsets", s.micro_offsets),
        ("restart_headers", s.restart_headers),
        ("restart_wide", s.restart_wide),
        ("restart_codes", s.restart_codes),
        ("entry_headers", s.entry_headers),
        ("entry_wide", s.entry_wide),
        ("entry_codes", s.entry_codes),
        ("total", total),
    ] {
        let share = if total == 0 {
            0.0
        } else {
            100.0 * bytes as f64 / total as f64
        };
        writeln!(
            out,
            "sections.{name}: {bytes} ({:.4} B/key, {share:.2} %)",
            bytes as f64 / keys
        )
        .map_err(io_fail)?;
    }
    for (name, count) in [
        ("restarts", s.restarts),
        ("entries", s.entries),
        ("wide", s.wide),
    ] {
        writeln!(out, "sections.{name}: {count}").map_err(io_fail)?;
    }
    Ok(())
}

/// Every field the header gave, one per line. The optional ones are index-specific, so a field
/// absent for this kind is left out rather than printed empty.
fn print_info(info: &BlobInfo, prefix: &str, out: &mut dyn Write) -> Result<(), Fail> {
    writeln!(out, "{prefix}kind: {:?}", info.kind).map_err(io_fail)?;
    writeln!(out, "{prefix}format: {}", info.format).map_err(io_fail)?;
    writeln!(out, "{prefix}bytes: {}", info.bytes).map_err(io_fail)?;
    for (name, field) in [
        ("keys", info.keys),
        ("fingerprint_bits", info.fingerprint_bits.map(u64::from)),
        ("mph_bytes", info.mph_bytes),
        ("arena_bytes", info.arena_bytes),
        ("side_entries", info.side_entries),
    ] {
        if let Some(v) = field {
            writeln!(out, "{prefix}{name}: {v}").map_err(io_fail)?;
        }
    }
    if let Some(o) = &info.overlay {
        writeln!(out, "{prefix}overlay.base_tag: {}", o.base_tag).map_err(io_fail)?;
        writeln!(out, "{prefix}overlay.additions: {}", o.additions).map_err(io_fail)?;
        writeln!(out, "{prefix}overlay.retired: {}", o.retired).map_err(io_fail)?;
        if let Some(base) = &o.base {
            print_info(base, &format!("{prefix}base."), out)?;
        }
    }
    Ok(())
}

fn dispatch(
    args: &[String],
    stdin: &mut dyn BufRead,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<(), Fail> {
    let (command, rest) = args
        .split_first()
        .ok_or_else(|| Fail::Usage("no subcommand".to_string()))?;
    // Asking for help wins wherever it is asked, so `lexindex plan --help` is the usage text on
    // stdout rather than a parse error on stderr.
    if command.as_str() == "help" || args.iter().any(|a| a == "--help" || a == "-h") {
        return write!(out, "{USAGE}").map_err(io_fail);
    }
    match command.as_str() {
        "--version" | "-V" => {
            writeln!(out, "lexindex {}", env!("CARGO_PKG_VERSION")).map_err(io_fail)
        }
        "plan" => cmd_plan(
            &parse(rest, Accepts::Needs, 1, "plan <keys-file> [needs]")?,
            stdin,
            out,
            err,
        ),
        "build" => cmd_build(
            &parse(
                rest,
                Accepts::NeedsAndIndex,
                2,
                "build <keys-file> <out-blob> [options]",
            )?,
            stdin,
            err,
        ),
        "inspect" => cmd_inspect(
            &parse(rest, Accepts::Sections, 1, "inspect <blob> [--sections]")?,
            out,
        ),
        "dump" => cmd_dump(&parse(rest, Accepts::Nothing, 1, "dump <blob>")?, out),
        other => Err(Fail::Usage(format!("no such subcommand `{other}`"))),
    }
}

/// The whole program bar the process: 0 on success, 2 when the command line is wrong, 1 when the
/// work is.
fn run(args: &[String], stdin: &mut dyn BufRead, out: &mut dyn Write, err: &mut dyn Write) -> u8 {
    match dispatch(args, stdin, out, err) {
        Ok(()) => 0,
        Err(Fail::Usage(m)) => {
            let _ = writeln!(err, "lexindex: {m}\n\n{USAGE}");
            2
        }
        Err(Fail::Failed(m)) => {
            let _ = writeln!(err, "lexindex: {m}");
            1
        }
    }
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = run(
        &args,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    std::process::ExitCode::from(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A directory of this process's own, one per test, removed by the test that made it.
    fn tmpdir() -> std::path::PathBuf {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lexindex-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Keys with a handful of shared prefixes and tails that share almost nothing.
    fn corpus(n: usize) -> String {
        let mut text = String::new();
        for i in 0..n {
            let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            h ^= h >> 29;
            text.push_str(&format!(
                "{}-{:012x}\n",
                ["ab", "abc", "b", "cde", "d", "ef", "g"][i % 7],
                h >> 16
            ));
        }
        text
    }

    /// `run` on borrowed strings: the exit code, stdout and stderr.
    fn go(args: &[&str], stdin: &str) -> (u8, String, String) {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut input = stdin.as_bytes();
        let code = run(&args, &mut input, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    /// The names left in a directory, for asserting that a build cleaned up after itself.
    fn entries(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn keys_file(dir: &Path, text: &str) -> String {
        let path = dir.join("keys.txt");
        std::fs::write(&path, text).unwrap();
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn no_arguments_is_a_usage_error_that_prints_the_usage() {
        let (code, out, err) = go(&[], "");
        assert_eq!((code, out.as_str()), (2, ""));
        assert!(err.contains("no subcommand"), "{err}");
        assert!(err.contains("lexindex plan"), "{err}");
    }

    #[test]
    fn help_and_version_go_to_stdout() {
        for args in [
            vec!["help"],
            vec!["--help"],
            vec!["-h"],
            vec!["plan", "--help"],
            vec!["build", "keys", "out", "-h"],
        ] {
            let (code, out, err) = go(&args, "");
            assert_eq!((code, err.as_str()), (0, ""), "{args:?}");
            assert!(out.contains("lexindex inspect <blob>"), "{out}");
        }
        for flag in ["--version", "-V"] {
            let (code, out, _) = go(&[flag], "");
            assert_eq!(code, 0);
            assert_eq!(out, format!("lexindex {}\n", env!("CARGO_PKG_VERSION")));
        }
    }

    #[test]
    fn what_is_not_a_subcommand_an_option_or_a_value_is_refused() {
        for (args, message) in [
            (vec!["explain", "k"], "no such subcommand `explain`"),
            (vec!["plan"], "expected `lexindex plan"),
            (vec!["plan", "a", "b"], "expected `lexindex plan"),
            (vec!["plan", "k", "--wat"], "no such option `--wat`"),
            (vec!["plan", "k", "--index", "dict"], "no such option"),
            (vec!["inspect", "k", "--exact"], "no such option `--exact`"),
            (vec!["build", "k", "o", "--index"], "--index needs a value"),
            (vec!["build", "k", "o", "--block"], "--block needs a value"),
            (
                vec!["build", "k", "o", "--index", "trie"],
                "no such index `trie`",
            ),
            (vec!["build", "k", "o", "--block=huge"], "neither a number"),
            (
                vec!["build", "k", "o", "--index=dict", "--block=0"],
                "outside 1..=1024",
            ),
            (
                vec!["build", "k", "o", "--index=dict", "--block=1025"],
                "outside 1..=1024",
            ),
            (
                vec!["build", "k", "o", "--block=64"],
                "--block is the DictIndex block size",
            ),
            (
                vec!["build", "k", "o", "--index=string", "--block=64"],
                "--block is the DictIndex block size",
            ),
        ] {
            let (code, out, err) = go(&args, "");
            assert_eq!((code, out.as_str()), (2, ""), "{args:?}");
            assert!(err.contains(message), "{args:?}: {err}");
        }
    }

    #[test]
    fn plan_prints_the_ladder_and_marks_the_winner() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(400));
        let (code, out, err) = go(&["plan", &keys, "--prefix", "--reverse"], "");
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.starts_with("400 keys, mean length"), "{out}");
        assert!(out.contains("DictIndex"), "{out}");
        assert!(out.contains('*'), "the chosen line is marked: {out}");
        // `--fuzzy` leaves exactly one candidate, so the ladder is one line under the shape —
        // and one under that, the line saying the nanoseconds are modelled.
        let (code, out, _) = go(&["plan", &keys, "--fuzzy"], "");
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 3, "{out}");
        assert!(out.contains("StringIndex"), "{out}");
        // The remaining needs parse and narrow nothing that is not already covered.
        let (code, _, _) = go(&["plan", &keys, "--ordered", "--exact"], "");
        assert_eq!(code, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The ladder ranks for what it was asked to rank for, and says which that was.
    #[test]
    fn objective_reorders_the_ladder_and_is_named_on_it() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(400));
        let mut first = Vec::new();
        for (name, word) in [
            ("memory", "ranked by size"),
            ("latency", "ranked by latency"),
            ("balanced", "ranked by balance"),
        ] {
            let (code, out, err) = go(&["plan", &keys, "--reverse", "--objective", name], "");
            assert_eq!((code, err.as_str()), (0, ""), "{name}");
            assert!(out.lines().next().unwrap().ends_with(word), "{out}");
            assert!(out.contains(" ns  "), "{out}");
            first.push(
                out.lines()
                    .nth(1)
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_string(),
            );
        }
        if cfg!(feature = "mph") {
            assert_ne!(first[0], first[1], "size and latency picked the same index");
        }
        // And it reaches the build, which asks the same ladder.
        let blob = dir.join("out.bin");
        let (code, _, err) = go(
            &[
                "build",
                &keys,
                blob.to_str().unwrap(),
                "--objective",
                "latency",
            ],
            "",
        );
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("ranked by latency"), "{err}");
        for bad in ["", "fast", "size"] {
            let (code, _, err) = go(&["plan", &keys, "--objective", bad], "");
            assert_eq!(
                (code, err.contains("--objective")),
                (2, true),
                "{bad}: {err}"
            );
        }
        // `inspect` and `dump` rank nothing.
        let (code, _, err) = go(
            &["dump", blob.to_str().unwrap(), "--objective", "memory"],
            "",
        );
        assert_eq!((code, err.contains("--objective")), (2, true), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A workload on the command line is the library's, spelled `op=weight`.
    #[test]
    fn a_workload_objective_parses_and_ranks() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(400));
        let ladder = |spec: &str| {
            let (code, out, err) = go(&["plan", &keys, "--objective", spec], "");
            assert_eq!((code, err.as_str()), (0, ""), "{spec}");
            out
        };
        let first = |out: &str| {
            out.lines()
                .nth(1)
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap()
                .to_string()
        };
        let out = ladder("common_prefix=9,hits=1");
        assert!(
            out.lines()
                .next()
                .unwrap()
                .ends_with("ranked by workload (hits 1, common_prefix 9)"),
            "{out}"
        );
        assert_eq!(first(&out), "StringIndex", "{out}");
        assert_eq!(first(&ladder("prefix=9,hits=1")), "DictIndex");
        let every =
            ladder("hits=1,misses=2,reverse=3,prefix=4,common_prefix=5,longest_prefix=6,batch=64");
        assert!(
            every.lines().next().unwrap().ends_with(
                "ranked by workload (hits 1, misses 2, reverse 3, prefix 4, common_prefix 5, \
                 longest_prefix 6, batch 64)"
            ),
            "{every}"
        );
        // Only a question the two hashes cannot answer rules them out; hits alone leave it to the
        // implied --exact, and the note says so — in a build that has the two at all.
        let (code, _, err) = go(&["plan", &keys, "--objective", "hits=9,misses=1"], "");
        assert_eq!(
            (code, err.contains("excluded: needs exact")),
            (0, cfg!(feature = "mph")),
            "{err}"
        );
        for bad in [
            "hits",
            "hits=",
            "hits=-1",
            "bogus=1",
            "batch=99999999999",
            "hits=1,",
            "=1",
        ] {
            let (code, _, err) = go(&["plan", &keys, "--objective", bad], "");
            assert_eq!(
                (code, err.contains("--objective")),
                (2, true),
                "{bad}: {err}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dash_reads_standard_input_and_empty_lines_are_reported() {
        let (code, out, err) = go(&["plan", "-"], "b\n\na\n\n\nc\n");
        assert_eq!(code, 0);
        assert!(out.starts_with("3 keys,"), "{out}");
        // The count is the first thing on stderr; with no needs given, the open-vocabulary
        // default explains itself underneath — where there is a probabilistic index to leave out.
        assert!(err.starts_with("-: 3 empty lines skipped\n"), "{err}");
        assert_eq!(
            err.contains("excluded: needs exact"),
            cfg!(feature = "mph"),
            "{err}"
        );
    }

    #[test]
    fn a_keys_file_that_is_missing_or_not_utf8_fails_and_says_which_line() {
        let dir = tmpdir();
        let missing = dir.join("nope.txt");
        let (code, _, err) = go(&["plan", missing.to_str().unwrap()], "");
        assert_eq!(code, 1);
        assert!(err.contains("nope.txt:"), "{err}");
        let path = dir.join("bad.txt");
        std::fs::write(&path, b"ok\nfine\n\xff\xfe\n").unwrap();
        let (code, _, err) = go(&["plan", path.to_str().unwrap()], "");
        assert_eq!(code, 1);
        assert!(
            err.contains("bad.txt:3:"),
            "the failing line is named: {err}"
        );
        assert!(err.contains("UTF-8"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The ladder prices every `DictIndex` block, marks one, and `--index auto` builds *that*
    /// block. Below the sample every estimate is a real build, so the blob is the quoted size to
    /// the byte — which is the whole point of quoting it.
    #[test]
    fn an_auto_build_writes_the_block_the_ladder_named() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(3_000));
        let blob = dir.join("auto.bin");
        let dest = blob.to_str().unwrap();
        let (code, out, err) = go(
            &["build", keys.as_str(), dest, "--ordered", "--reverse"],
            "",
        );
        assert_eq!((code, out.as_str()), (0, ""), "{err}");
        let marked: Vec<&str> = err.lines().filter(|l| l.starts_with('*')).collect();
        assert_eq!(marked.len(), 1, "one winner: {err}");
        assert_eq!(
            err.lines().filter(|l| l.contains("at block ")).count(),
            3,
            "one row a priced block: {err}"
        );
        let won = marked[0];
        assert!(
            won.contains("DictIndex") && won.contains("at block "),
            "{err}"
        );
        let quoted: u64 = won.split_whitespace().nth(2).unwrap().parse().unwrap();
        assert_eq!(
            std::fs::metadata(&blob).unwrap().len(),
            quoted,
            "the blob is the line that was marked: {err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The streaming route writes the blob the in-memory one writes, byte for byte, for every
    /// index that has one — which is what makes `--stream` a memory setting and not a format.
    #[test]
    fn a_streamed_build_writes_the_blob_the_in_memory_one_does() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(2_000));
        for (index, kind, block) in named_indexes() {
            let mut written = Vec::new();
            for mode in ["never", "always"] {
                let blob = dir.join(format!("{index}-{mode}.bin"));
                let dest = blob.to_str().unwrap();
                let mut args = vec!["build", keys.as_str(), dest, "--index", index];
                args.extend(block.iter().copied());
                args.extend(["--stream", mode]);
                let (code, out, err) = go(&args, "");
                assert_eq!((code, out.as_str()), (0, ""), "{index} {mode}: {err}");
                assert!(
                    err.contains(&format!("wrote {dest}: {kind} over 2000 keys")),
                    "{index} {mode}: {err}"
                );
                written.push(std::fs::read(&blob).unwrap());
            }
            assert!(written[0] == written[1], "{index}: the blobs differ");
            assert_eq!(
                entries(&dir)
                    .iter()
                    .filter(|e| e.contains(".part."))
                    .count(),
                0
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A source that goes bad halfway is an error naming the line, and what was already at the
    /// destination is still there: the streaming builders would otherwise publish a whole index
    /// over the part of the file they managed to read.
    #[test]
    fn a_streamed_build_over_a_broken_file_publishes_nothing() {
        let dir = tmpdir();
        let path = dir.join("broken.txt");
        let mut bytes = corpus(500).into_bytes();
        bytes.extend_from_slice(b"good\n\xff\xfe not utf-8\nmore\n");
        std::fs::write(&path, &bytes).unwrap();
        let blob = dir.join("out.bin");
        std::fs::write(&blob, b"the previous index").unwrap();
        let (code, out, err) = go(
            &[
                "build",
                path.to_str().unwrap(),
                blob.to_str().unwrap(),
                "--index",
                "string",
                "--stream",
                "always",
            ],
            "",
        );
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("broken.txt:502"), "{err}");
        assert_eq!(std::fs::read(&blob).unwrap(), b"the previous index");
        assert_eq!(
            entries(&dir)
                .iter()
                .filter(|e| e.contains(".part."))
                .count(),
            0
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Standard input cannot be rewound, so it is built in memory whatever `--stream` says, and
    /// the empty lines are still counted on the streaming path.
    #[test]
    fn stream_skips_standard_input_and_still_counts_blank_lines() {
        let dir = tmpdir();
        let keys = keys_file(&dir, "alpha\n\nbeta\n\ngamma\n");
        let blob = dir.join("out.bin");
        let dest = blob.to_str().unwrap();
        let (code, _, err) = go(
            &[
                "build",
                keys.as_str(),
                dest,
                "--index",
                "string",
                "--stream",
                "always",
            ],
            "",
        );
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("2 empty lines skipped"), "{err}");
        assert!(err.contains("over 3 keys"), "{err}");
        let piped = dir.join("piped.bin");
        let (code, _, err) = go(
            &[
                "build",
                "-",
                piped.to_str().unwrap(),
                "--index",
                "string",
                "--stream",
                "always",
            ],
            "alpha\n\nbeta\n\ngamma\n",
        );
        assert_eq!(code, 0, "{err}");
        assert_eq!(
            std::fs::read(&blob).unwrap(),
            std::fs::read(&piped).unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `--index auto` has to price the corpus, so it says why it is reading a file it could
    /// otherwise have streamed.
    #[test]
    fn auto_over_a_large_file_streams_what_it_ranked() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(300));
        let blob = dir.join("out.bin");
        let (code, _, err) = go(
            &[
                "build",
                keys.as_str(),
                blob.to_str().unwrap(),
                "--stream",
                "always",
            ],
            "",
        );
        assert_eq!(code, 0, "{err}");
        // It used to read the file in and say why: pricing a corpus meant holding it. `plan_file`
        // prices it without, so a ranked build streams like a named one.
        assert!(!err.contains("prices the corpus"), "{err}");
        let marked: Vec<&str> = err.lines().filter(|l| l.starts_with('*')).collect();
        assert_eq!(marked.len(), 1, "the ladder is still printed: {err}");
        let quoted: u64 = marked[0]
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            std::fs::metadata(&blob).unwrap().len(),
            quoted,
            "the streamed blob is the line that was marked: {err}"
        );
        assert_eq!(
            entries(&dir)
                .iter()
                .filter(|e| e.contains(".part."))
                .count(),
            0
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn stream_takes_a_mode_or_a_size_and_nothing_else() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(50));
        let blob = dir.join("out.bin");
        let dest = blob.to_str().unwrap();
        for size in ["0", "1000", "512M", "4G", "8k"] {
            let (code, _, err) = go(
                &[
                    "build",
                    keys.as_str(),
                    dest,
                    "--index",
                    "string",
                    "--stream",
                    size,
                ],
                "",
            );
            assert_eq!(code, 0, "{size}: {err}");
        }
        for bad in ["", "512MB", "-1", "lots", "4E"] {
            let (code, _, err) = go(
                &[
                    "build",
                    keys.as_str(),
                    dest,
                    "--index",
                    "string",
                    "--stream",
                    bad,
                ],
                "",
            );
            assert_eq!(code, 2, "{bad}: {err}");
            assert!(err.contains("--stream"), "{bad}: {err}");
        }
        for cmd in [
            vec!["plan", keys.as_str(), "--stream", "always"],
            vec!["inspect", dest, "--stream", "always"],
            vec!["dump", dest, "--stream", "always"],
        ] {
            let (code, _, err) = go(&cmd, "");
            assert_eq!((code, err.contains("--stream")), (2, true), "{err}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn build_writes_a_blob_that_inspect_reads_back() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(300));
        for (index, kind, block) in named_indexes() {
            let blob = dir.join(format!("{index}.bin"));
            let dest = blob.to_str().unwrap();
            let mut args = vec!["build", keys.as_str(), dest, "--index", index];
            args.extend(block);
            let (code, out, err) = go(&args, "");
            assert_eq!((code, out.as_str()), (0, ""), "{index}: {err}");
            assert!(
                err.contains(&format!("wrote {dest}: {kind} over 300 keys")),
                "{err}"
            );
            assert!(err.contains("B/key)"), "{err}");
            let (code, out, err) = go(&["inspect", dest], "");
            assert_eq!((code, err.as_str()), (0, ""), "{index}");
            assert!(out.contains(&format!("kind: {kind}\n")), "{index}: {out}");
            assert!(out.contains("keys: 300\n"), "{index}: {out}");
            assert!(out.contains("bytes: "), "{index}: {out}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Every `--index` name this build can serve, with the `BlobKind` it writes.
    fn named_indexes() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
        let mut all = vec![
            ("dict", "DictIndex", vec![]),
            ("dict", "DictIndex", vec!["--block", "fast"]),
            ("dict", "DictIndex", vec!["--block=balanced"]),
            ("dict", "DictIndex", vec!["--block", "compact"]),
            ("dict", "DictIndex", vec!["--block", "64"]),
            ("string", "StringIndex", vec![]),
        ];
        if cfg!(feature = "mph") {
            all.extend([
                ("compact", "CompactHashIndex", vec![]),
                ("closed", "ClosedHashIndex", vec![]),
                ("perfect", "PerfectHashIndex", vec![]),
            ]);
        }
        all
    }

    #[cfg(not(feature = "mph"))]
    #[test]
    fn an_index_this_build_does_not_have_says_so() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(50));
        let dest = dir.join("out.bin");
        let (code, _, err) = go(
            &["build", &keys, dest.to_str().unwrap(), "--index", "compact"],
            "",
        );
        assert_eq!(code, 1);
        assert!(err.contains("needs the `mph` feature"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auto_prints_the_ladder_it_chose_from_and_builds_the_winner() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(300));
        let dest = dir.join("auto.bin");
        let out_path = dest.to_str().unwrap();
        // `--reverse --prefix --exact` leaves the two ordered indexes, and the ladder names both.
        let (code, out, err) = go(
            &["build", &keys, out_path, "--reverse", "--prefix", "--exact"],
            "",
        );
        assert_eq!((code, out.as_str()), (0, ""), "{err}");
        assert!(err.contains("300 keys, mean length"), "{err}");
        assert!(err.contains("StringIndex"), "{err}");
        let chosen = lexindex::inspect_file(&dest).unwrap();
        assert!(
            err.contains(&format!(
                "wrote {out_path}: {:?} over 300 keys",
                chosen.kind
            )),
            "the summary names what was written: {err}"
        );
        // The default `--index auto` with no needs is the same path over every candidate.
        let (code, _, err) = go(&["build", &keys, out_path], "");
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("wrote "), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inspect_reports_an_overlays_own_fields_and_its_base() {
        let dir = tmpdir();
        let dest = dir.join("overlay.bin");
        let mut overlay = lexindex::Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        overlay.add("c");
        assert!(overlay.remove("a"));
        overlay.save(&dest).unwrap();
        let (code, out, err) = go(&["inspect", dest.to_str().unwrap()], "");
        assert_eq!((code, err.as_str()), (0, ""));
        assert!(out.contains("kind: Overlay\n"), "{out}");
        assert!(out.contains("overlay.additions: 1\n"), "{out}");
        assert!(out.contains("overlay.retired: 1\n"), "{out}");
        assert!(out.contains("base.kind: StringIndex\n"), "{out}");
        assert!(out.contains("base.keys: 2\n"), "{out}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dump_reads_back_the_keys_every_kind_that_stores_them_holds() {
        let dir = tmpdir();
        let text = corpus(300);
        let keys = keys_file(&dir, &text);
        let mut sorted: Vec<&str> = text.lines().collect();
        sorted.sort_unstable();
        for (index, _kind, block) in named_indexes() {
            let blob = dir.join(format!("dump-{index}-{}.bin", block.len()));
            let dest = blob.to_str().unwrap();
            let mut args = vec!["build", keys.as_str(), dest, "--index", index];
            args.extend(block.iter().copied());
            assert_eq!(go(&args, "").0, 0, "{index}");
            let (code, out, err) = go(&["dump", dest], "");
            if matches!(index, "compact" | "closed") {
                assert_eq!((code, out.as_str()), (1, ""), "{index}");
                assert!(err.contains("stores no keys"), "{index}: {err}");
                continue;
            }
            assert_eq!((code, err.as_str()), (0, ""), "{index}");
            let dumped: Vec<&str> = out.lines().collect();
            assert_eq!(dumped.len(), 300, "{index}");
            // An ordered index dumps in sorted order; the perfect hash dumps in its own id order,
            // and only the set is promised there.
            if matches!(index, "dict" | "string") {
                assert_eq!(dumped, sorted, "{index}");
            } else {
                let mut seen = dumped.clone();
                seen.sort_unstable();
                assert_eq!(seen, sorted, "{index}");
            }
            // The round trip is the point: rebuilding from the dump gives the same blob back.
            let again = dir.join(format!("again-{index}-{}.bin", block.len()));
            let back = again.to_str().unwrap();
            let mut args = vec!["build", "-", back, "--index", index];
            args.extend(block.iter().copied());
            assert_eq!(go(&args, &out).0, 0, "{index}");
            assert_eq!(
                std::fs::read(&blob).unwrap(),
                std::fs::read(&again).unwrap(),
                "{index}: dump | build did not reproduce the blob"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dump_walks_an_overlay_in_id_order_and_skips_what_was_retired() {
        let dir = tmpdir();
        let dest = dir.join("overlay.bin");
        let mut overlay = lexindex::Overlay::new(StringIndex::build(["a", "b"]).unwrap());
        overlay.add("c");
        assert!(overlay.remove("a"));
        overlay.save(&dest).unwrap();
        let (code, out, err) = go(&["dump", dest.to_str().unwrap()], "");
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(out, "b\nc\n");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dump_refuses_what_is_not_a_blob_and_takes_no_options() {
        let dir = tmpdir();
        let path = dir.join("junk.bin");
        std::fs::write(&path, b"not a blob at all").unwrap();
        let (code, out, err) = go(&["dump", path.to_str().unwrap()], "");
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("junk.bin: format error"), "{err}");
        let (code, _, err) = go(&["dump", path.to_str().unwrap(), "--exact"], "");
        assert_eq!(code, 2);
        assert!(err.contains("no such option `--exact`"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn auto_assumes_an_open_vocabulary_until_told_otherwise() {
        let dir = tmpdir();
        let keys = keys_file(&dir, &corpus(500));
        // The ladder leaves the two probabilistic indexes out, and says so on stderr.
        let (code, out, err) = go(&["plan", keys.as_str()], "");
        assert_eq!(code, 0);
        assert!(!out.contains("ClosedHashIndex"), "{out}");
        assert!(!out.contains("CompactHashIndex"), "{out}");
        // So `auto` cannot build one by default, whatever the corpus makes cheapest.
        let blob = dir.join("auto.bin");
        let dest = blob.to_str().unwrap();
        assert_eq!(go(&["build", keys.as_str(), dest], "").0, 0);
        assert_ne!(&std::fs::read(&blob).unwrap()[..4], b"BCL2");
        let (_, out, _) = go(&["inspect", dest], "");
        assert!(out.contains("kind: DictIndex\n"), "{out}");
        if !cfg!(feature = "mph") {
            // Without `mph` there is no probabilistic index to leave out, so nothing is explained.
            assert!(!err.contains("excluded"), "{err}");
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        }
        assert!(err.contains("excluded: needs exact"), "{err}");
        assert!(err.contains("--closed-vocabulary"), "{err}");

        // With the flag they are ranked with the rest, and `auto` picks the smallest again.
        let (code, out, err) = go(&["plan", keys.as_str(), "--closed-vocabulary"], "");
        assert_eq!(code, 0);
        assert!(out.contains("* ClosedHashIndex"), "{out}");
        assert!(!err.contains("excluded"), "{err}");
        let open = dir.join("closed.bin");
        let dest = open.to_str().unwrap();
        assert_eq!(
            go(&["build", keys.as_str(), dest, "--closed-vocabulary"], "").0,
            0
        );
        assert_eq!(&std::fs::read(&open).unwrap()[..4], b"BCL2");

        // Asking for one by name builds it whatever the needs would have said.
        let named = dir.join("named.bin");
        let dest = named.to_str().unwrap();
        let (code, _, err) = go(&["build", keys.as_str(), dest, "--index", "closed"], "");
        assert_eq!(code, 0, "{err}");
        assert_eq!(&std::fs::read(&named).unwrap()[..4], b"BCL2");
        // No ladder was printed, so there is nothing to explain.
        assert!(!err.contains("excluded"), "{err}");

        // An explicit `--exact` is the caller's own, so the line stays out of the way.
        let (_, _, err) = go(&["plan", keys.as_str(), "--exact"], "");
        assert!(!err.contains("excluded"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inspect_sections_accounts_for_every_byte_of_a_dict_blob() {
        let dir = tmpdir();
        let text = corpus(2_000);
        let keys = keys_file(&dir, &text);
        let blob = dir.join("sections.bin");
        let dest = blob.to_str().unwrap();
        assert_eq!(
            go(&["build", keys.as_str(), dest, "--index", "dict"], "").0,
            0
        );
        let (code, out, err) = go(&["inspect", dest, "--sections"], "");
        assert_eq!((code, err.as_str()), (0, ""));
        // The header block is still printed, and the split comes after it.
        assert!(out.contains("kind: DictIndex\n"), "{out}");
        let of = |name: &str| -> u64 {
            let at = out
                .find(&format!("sections.{name}: "))
                .unwrap_or_else(|| panic!("no {name} in {out}"));
            let rest = &out[at + name.len() + 11..];
            rest[..rest.find([' ', '\n']).unwrap()].parse().unwrap()
        };
        let parts = [
            "header",
            "tables",
            "header_codes",
            "phrases",
            "heads",
            "head_ends",
            "block_offsets",
            "micro_offsets",
            "restart_headers",
            "restart_wide",
            "restart_codes",
            "entry_headers",
            "entry_wide",
            "entry_codes",
        ];
        let sum: u64 = parts.iter().map(|p| of(p)).sum();
        assert_eq!(sum, of("total"), "{out}");
        assert_eq!(sum, std::fs::metadata(&blob).unwrap().len(), "{out}");
        // Every key is a head, a restart or an entry, and the blob says how many keys it holds.
        let keys_held: u64 = {
            let at = out.find("keys: ").unwrap();
            let rest = &out[at + 6..];
            rest[..rest.find('\n').unwrap()].parse().unwrap()
        };
        let blocks = keys_held.div_ceil(256);
        assert_eq!(blocks + of("restarts") + of("entries"), keys_held, "{out}");
        assert!(out.contains(" B/key, "), "the split is read per key: {out}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inspect_sections_refuses_a_blob_that_is_not_a_dict_and_dump_refuses_the_flag() {
        let dir = tmpdir();
        let blob = dir.join("string.bin");
        StringIndex::build(["a", "b"]).unwrap().save(&blob).unwrap();
        let dest = blob.to_str().unwrap();
        let (code, out, err) = go(&["inspect", dest, "--sections"], "");
        assert_eq!(code, 1);
        // What the header said is still printed before the refusal names the kind.
        assert!(out.contains("kind: StringIndex\n"), "{out}");
        assert!(
            err.contains("--sections is a DictIndex's byte split"),
            "{err}"
        );
        let (code, _, err) = go(&["dump", dest, "--sections"], "");
        assert_eq!(code, 2);
        assert!(err.contains("no such option `--sections`"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inspect_on_what_is_not_a_blob_fails_and_names_the_file() {
        let dir = tmpdir();
        let path = dir.join("junk.bin");
        std::fs::write(&path, b"not a blob at all").unwrap();
        let (code, out, err) = go(&["inspect", path.to_str().unwrap()], "");
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("junk.bin: format error"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_build_the_library_refuses_is_reported_rather_than_panicking() {
        let dir = tmpdir();
        let keys = keys_file(&dir, "a\nb\n");
        let dest = dir.join("nosuchdir").join("out.bin");
        let (code, out, err) = go(
            &["build", &keys, dest.to_str().unwrap(), "--index=string"],
            "",
        );
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("io error"), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A writer whose every write fails, for the paths that only a broken pipe reaches.
    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "gone"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_write_that_fails_is_an_error_and_not_a_panic() {
        let mut err = Vec::new();
        let code = run(
            &["--version".to_string()],
            &mut &b""[..],
            &mut Broken,
            &mut err,
        );
        assert_eq!(code, 1);
        assert!(String::from_utf8(err).unwrap().contains("gone"));
    }
}
