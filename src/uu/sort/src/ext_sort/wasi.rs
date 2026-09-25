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

/// Appends all of `reader` to `input`, failing rather than holding more than the limits allow.
fn read_bounded(mut reader: impl Read, input: &mut Vec<u8>, separator: u8) -> UResult<()> {
    let room = MAX_INPUT_BYTES.saturating_sub(input.len()) as u64;
    reader.by_ref().take(room + 1).read_to_end(input)?;
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
