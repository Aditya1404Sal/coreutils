// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore reflink

use std::fs::File;
use std::io;
use std::path::Path;
use uucore::display::Quotable;
use uucore::translate;

use crate::{
    CopyDebug, CopyResult, CpError, OffloadReflinkDebug, ReflinkMode, SparseDebug, SparseMode,
};

/// Copies `source` to `dest` for systems without copy-on-write
pub(crate) fn copy_on_write(
    source: &Path,
    dest: &Path,
    reflink_mode: ReflinkMode,
    sparse_mode: SparseMode,
    context: &str,
) -> CopyResult<CopyDebug> {
    if reflink_mode == ReflinkMode::Always {
        return Err(translate!("cp-error-reflink-not-supported")
            .to_string()
            .into());
    }
    if sparse_mode != SparseMode::Auto {
        return Err(translate!("cp-error-sparse-not-supported")
            .to_string()
            .into());
    }
    let copy_debug = CopyDebug {
        offload: OffloadReflinkDebug::Unsupported,
        reflink: OffloadReflinkDebug::Unsupported,
        sparse_detection: SparseDebug::Unsupported,
    };

    // Not `fs::copy`: its generic fallback (the one every non-unix, non-windows target gets,
    // WASI included) refuses any source that doesn't satisfy `Metadata::is_file()`, with
    // `std`'s own internal wording ("the source path is neither a regular file nor a symlink
    // to a regular file") -- accurate to what it checked, but misleading here and wrong for
    // what it's guarding: a stream-backed source like `/dev/fd/N` on a pipe is exactly the
    // kind of readable, non-regular file GNU cp already copies fine (its read/write loop has
    // no such requirement). Open and stream the bytes directly instead, which works
    // uniformly for regular files and stream-backed descriptors alike; permissions are
    // preserved separately by `copy_attributes` when `-p`/`-a` asks for that, same as the
    // stream branch of the `unix`-only platform backend this mirrors.
    // GNU names the file whose open failed.
    let mut src_file = File::open(source).map_err(|e| {
        CpError::IoErrContext(
            e,
            translate!("cp-error-cannot-open-for-reading", "source" => source.quote()),
        )
    })?;
    let mut dst_file = File::create(dest).map_err(|e| {
        CpError::IoErrContext(
            e,
            translate!("cp-error-cannot-create-regular-file", "path" => dest.quote()),
        )
    })?;
    io::copy(&mut src_file, &mut dst_file)
        .map_err(|e| CpError::IoErrContext(e, context.to_owned()))?;

    Ok(copy_debug)
}
