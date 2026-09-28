// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

//! WASI single-threaded sort: read all input into memory, sort, and output.
//! Threads are not available on WASI, so we bypass the chunked/threaded path.

use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::iter;

use itertools::Itertools;
use uucore::error::{UError, UResult, USimpleError};

use crate::chunks::{self, Chunk};
use crate::tmp_dir::TmpDirWrapper;
use crate::{GlobalSettings, SortError, compare_by, open, print_sorted, sort_by};
use crate::{Line, Output};

/// The most input sort holds in memory here, where it has no external merge: 64 MiB, and 2
/// million lines, whose bookkeeping costs more than short lines themselves.
const MAX_INPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_INPUT_LINES: usize = 2_000_000;

fn input_too_large() -> Box<dyn UError> {
    USimpleError::new(
        2,
        "input over 64 MiB or 2000000 lines is unsupported in bash-tool",
    )
}

/// A read error naming the input it came from.
#[derive(Debug)]
struct ReadFailed(std::path::PathBuf, std::io::Error);

impl std::fmt::Display for ReadFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use uucore::display::Quotable;
        write!(
            f,
            "read failed: {}: {}",
            self.0.maybe_quote(),
            uucore::error::strip_errno(&self.1)
        )
    }
}

impl std::error::Error for ReadFailed {}

/// `reader`, whose read errors name `path` as GNU sort reports them (`read failed: PATH: …`).
pub fn named_reader(path: &OsStr, reader: Box<dyn Read + Send>) -> Box<dyn Read + Send> {
    struct Named(std::path::PathBuf, Box<dyn Read + Send>);
    impl Read for Named {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1
                .read(buf)
                .map_err(|error| std::io::Error::other(ReadFailed(self.0.clone(), error)))
        }
    }
    Box::new(Named(std::path::PathBuf::from(path), reader))
}

/// Appends all of `reader` to `input`, failing rather than holding more than the limits allow.
fn read_bounded(mut reader: impl Read, input: &mut Vec<u8>, separator: u8) -> UResult<()> {
    let room = MAX_INPUT_BYTES.saturating_sub(input.len()) as u64;
    if let Err(error) = reader.by_ref().take(room + 1).read_to_end(input) {
        // A read error that names its input is reported as GNU sort words it.
        if error
            .get_ref()
            .is_some_and(<dyn std::error::Error + Send + Sync>::is::<ReadFailed>)
        {
            return Err(USimpleError::new(2, error.to_string()));
        }
        return Err(error.into());
    }
    if input.len() > MAX_INPUT_BYTES
        || memchr::memchr_iter(separator, input).count() > MAX_INPUT_LINES
    {
        return Err(input_too_large());
    }
    Ok(())
}

/// Read one input whole and split it into lines, or `None` for an empty input.
fn read_whole(path: &OsStr, settings: &GlobalSettings) -> UResult<Option<Chunk>> {
    let mut input = Vec::new();
    read_bounded(open(path)?, &mut input, settings.line_ending.into())?;
    if input.is_empty() {
        return Ok(None);
    }
    let separator = settings.line_ending.into();
    Chunk::try_new(input, |buffer| {
        Ok::<_, Box<dyn UError>>(chunks::parse_into_chunk(buffer, separator, settings))
    })
    .map(Some)
}

/// `sort -c`/`-C` without the reader thread the threaded check uses: read the input whole and
/// compare each line with the one before it, reporting the first out of order.
pub fn check_input(path: &OsStr, settings: &GlobalSettings) -> UResult<()> {
    // With `-u` a line must sort strictly after the one before it.
    let max_allowed_cmp = if settings.unique {
        Ordering::Less
    } else {
        Ordering::Equal
    };
    let Some(chunk) = read_whole(path, settings)? else {
        return Ok(());
    };
    for (index, (a, b)) in chunk.lines().iter().tuple_windows().enumerate() {
        if compare_by(a, b, settings, chunk.line_data(), chunk.line_data()) > max_allowed_cmp {
            return Err(SortError::Disorder {
                file: path.to_owned(),
                line_number: index + 2,
                line: String::from_utf8_lossy(b.line).into_owned(),
                silent: settings.check_silent,
            }
            .into());
        }
    }
    Ok(())
}

/// `sort -m` without reader threads: read each input whole, then repeatedly take the least head
/// line, the earliest input's on a tie, as the threaded merger does. Every input is read before
/// any output is written, so an output file that is also an input needs no copy.
pub fn merge_inputs(files: &[OsString], settings: &GlobalSettings, output: Output) -> UResult<()> {
    let mut inputs = Vec::with_capacity(files.len());
    for file in files {
        inputs.extend(read_whole(file, settings)?);
    }
    let mut next = vec![0; inputs.len()];
    let merged = iter::from_fn(|| {
        let mut least: Option<(usize, &Line<'_>)> = None;
        for (i, input) in inputs.iter().enumerate() {
            let Some(line) = input.lines().get(next[i]) else {
                continue;
            };
            let take = least.is_none_or(|(j, current)| {
                let order = compare_by(
                    line,
                    current,
                    settings,
                    input.line_data(),
                    inputs[j].line_data(),
                );
                order == Ordering::Less
            });
            if take {
                least = Some((i, line));
            }
        }
        let (i, line) = least?;
        next[i] += 1;
        Some((i, line))
    });
    if settings.unique {
        let unique = merged.dedup_by(|(i, a), (j, b)| {
            let order = compare_by(
                a,
                b,
                settings,
                inputs[*i].line_data(),
                inputs[*j].line_data(),
            );
            order == Ordering::Equal
        });
        print_sorted(unique.map(|(_, line)| line), settings, output)
    } else {
        print_sorted(merged.map(|(_, line)| line), settings, output)
    }
}

/// Sort files by reading all input into memory, sorting in a single thread, and outputting directly.
pub fn ext_sort(
    files: &mut impl Iterator<Item = UResult<Box<dyn Read + Send>>>,
    settings: &GlobalSettings,
    output: Output,
    _tmp_dir: &mut TmpDirWrapper,
) -> UResult<()> {
    let separator = settings.line_ending.into();
    // Read all input into memory at once, within the limits: WASI has no threads for the
    // chunked, merging path.
    let mut input = Vec::new();
    for file in files {
        // A file's own last line still ends at that file's EOF, even without a trailing
        // separator: GNU sort never splices one file's unterminated tail onto the next file's
        // head. Since every file here gets flattened into one buffer before it is split into
        // lines, force that boundary in the byte stream itself, before this file's own bytes
        // (not after -- read_bounded appends straight into the shared, bounds-checked `input`,
        // so there's no separate per-file buffer left to inspect afterward). Inserting it
        // unconditionally before every file but the first is equivalent to only inserting it
        // before the next *non-empty* file: an empty file leaves `input` already ending with
        // the separator this just added, so the check before the following file is a no-op.
        if !input.is_empty() && input.last() != Some(&separator) {
            input.push(separator);
        }
        read_bounded(file?, &mut input, separator)?;
    }

    if input.is_empty() {
        // empty files are sorted to empty like in coreutils
        print_sorted(iter::empty::<&Line<'_>>(), settings, output)?;
        return Ok(());
    }

    let mut chunk = Chunk::try_new(input, |buffer| {
        Ok::<_, Box<dyn uucore::error::UError>>(chunks::parse_into_chunk(
            buffer, separator, settings,
        ))
    })?;
    chunk.with_dependent_mut(|_, contents| {
        sort_by(&mut contents.lines, settings, &contents.line_data);
    });
    if settings.unique {
        print_sorted(
            chunk.lines().iter().dedup_by(|a, b| {
                compare_by(a, b, settings, chunk.line_data(), chunk.line_data()) == Ordering::Equal
            }),
            settings,
            output,
        )?;
    } else {
        print_sorted(chunk.lines().iter(), settings, output)?;
    }
    Ok(())
}
