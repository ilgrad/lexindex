//! `lexindex`, the command line: price the indexes on a corpus, build one, say what a blob is, and
//! read the keys back out of it.
//!
//! A shell over `lexindex::plan`, the builders, `lexindex::inspect_file` and the indexes' own key
//! iterators, which computes nothing of its own — the ladder `plan` prints is `Plan`'s own
//! `Display`, and `--index auto` builds the index that ladder puts first. `main` is three lines
//! over `run`, so the argument parsing, the four subcommands and every error path are reachable
//! from a unit test.

use lexindex::{
    BlobInfo, BlobKind, DictIndex, DictProfile, IndexError, Kind, Needs, Overlay, OverlayBase,
    StringIndex, plan,
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
  lexindex plan    <keys-file> [needs]
  lexindex build   <keys-file> <out-blob> [--index NAME] [--block SPEC] [needs]
  lexindex inspect <blob>
  lexindex dump    <blob>

  plan     what every index that answers the needs would weigh, cheapest first
  build    build one index and save it; `--index auto` asks plan and builds the winner
  inspect  what a blob already on disk is, from its header alone
  dump     the keys a blob holds, one per line, in id order — `dump | build` is the
           migration path off a format a later version stops reading

needs — what the index must be able to do. They narrow what `plan` ranks and what
`--index auto` may pick; with none of them the only question asked is `id(key)`:
  --reverse    key(id) as well as id(key)
  --ordered    ids in lexicographic order, and in-order iteration
  --prefix     prefix and range queries (an ordered index)
  --fuzzy      Levenshtein and subsequence queries
  --exact      a non-member must be answered as one, barring the probabilistic indexes

options (`--name value` or `--name=value`):
  --index NAME   auto (default), dict, string, compact, closed, perfect
  --block SPEC   keys per DictIndex block, with `--index dict`: 1..=1024, or one of
                 fast / balanced / compact (32 / 256 / 1024)
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
    /// `inspect` and `dump`: the blob, and nothing else.
    Nothing,
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
    };
    let mut at = 0;
    while at < args.len() {
        let arg = &args[at];
        at += 1;
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg.as_str(), None),
        };
        match name {
            "--reverse" | "--ordered" | "--prefix" | "--fuzzy" | "--exact"
                if accepts != Accepts::Nothing =>
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
            "--index" if accepts == Accepts::NeedsAndIndex => {
                cmd.index = choice(&value(args, &mut at, name, inline)?)?;
            }
            "--block" if accepts == Accepts::NeedsAndIndex => {
                cmd.block = Some(block_size(&value(args, &mut at, name, inline)?)?);
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
    let (keys, blank) = read_keys(path, stdin)?;
    note_blank(path, blank, err)?;
    write!(out, "{}", plan(&keys, cmd.needs)?).map_err(io_fail)
}

fn cmd_build(cmd: &Cmd, stdin: &mut dyn BufRead, err: &mut dyn Write) -> Result<(), Fail> {
    // `plan` prices `DictIndex` at the default block, so a block named alongside `auto` would build
    // something other than what was quoted. Naming the index is what naming its block goes with.
    if cmd.block.is_some() && cmd.index != Choice::One(Kind::Dict) {
        return Err(Fail::Usage(
            "--block is the DictIndex block size and needs `--index dict`".to_string(),
        ));
    }
    let (path, dest) = (&cmd.positional[0], Path::new(&cmd.positional[1]));
    let (keys, blank) = read_keys(path, stdin)?;
    note_blank(path, blank, err)?;
    let kind = match cmd.index {
        Choice::One(k) => k,
        Choice::Auto => {
            let ranked = plan(&keys, cmd.needs)?;
            write!(err, "{ranked}").map_err(io_fail)?;
            ranked.best().kind
        }
    };
    let n = build_and_save(kind, &keys, dest, cmd.block)?;
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
    print_info(&info, "", out)
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
        "inspect" => cmd_inspect(&parse(rest, Accepts::Nothing, 1, "inspect <blob>")?, out),
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
        // `--fuzzy` leaves exactly one candidate, so the ladder is one line under the shape.
        let (code, out, _) = go(&["plan", &keys, "--fuzzy"], "");
        assert_eq!(code, 0);
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(out.contains("StringIndex"), "{out}");
        // The remaining needs parse and narrow nothing that is not already covered.
        let (code, _, _) = go(&["plan", &keys, "--ordered", "--exact"], "");
        assert_eq!(code, 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dash_reads_standard_input_and_empty_lines_are_reported() {
        let (code, out, err) = go(&["plan", "-"], "b\n\na\n\n\nc\n");
        assert_eq!(code, 0);
        assert!(out.starts_with("3 keys,"), "{out}");
        assert_eq!(err, "-: 3 empty lines skipped\n");
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
