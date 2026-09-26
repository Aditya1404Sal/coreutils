// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

use std::io;
use std::ops::Range;
use thiserror::Error;
use uucore::display::Quotable;
use uucore::error::{UError, strip_errno};
use uucore::translate;

/// Where inside a pattern operand the caret belongs, and what to write under
/// it.
///
/// `csplit: '/a(b/': invalid pattern` says nothing about which part of the
/// pattern the parser choked on. The parser works that out anyway, so it is
/// kept here for [`crate::diagnostics`] rather than thrown away; it never
/// changes the one-line message.
#[derive(Debug)]
pub struct PatternProblem {
    /// Byte range inside the operand to point at.
    pub span: Range<usize>,
    /// What the regex engine said, when the operand was a well-formed
    /// `/REGEXP/` or `%REGEXP%` whose regex did not compile. Not localized:
    /// it is quoted from the engine, which has no translations.
    pub label: Option<String>,
}

/// Errors thrown by the csplit command
#[derive(Debug, Error)]
pub enum CsplitError {
    #[error("{}", strip_errno(_0))]
    IoError(#[from] io::Error),
    #[error("{}", translate!("csplit-error-line-out-of-range", "pattern" => uucore::display::gnu_quote(_0)))]
    LineOutOfRange(String),
    #[error("{}", translate!("csplit-error-line-out-of-range-on-repetition", "pattern" => uucore::display::gnu_quote(_0), "repetition" => _1))]
    LineOutOfRangeOnRepetition(String, usize),
    #[error("{}", translate!("csplit-error-match-not-found", "pattern" => uucore::display::gnu_quote(_0)))]
    MatchNotFound(String),
    #[error("{}", translate!("csplit-error-match-not-found-on-repetition", "pattern" => uucore::display::gnu_quote(_0), "repetition" => _1))]
    MatchNotFoundOnRepetition(String, usize),
    #[error("{}", translate!("csplit-error-line-number-is-zero"))]
    LineNumberIsZero,
    #[error("{}", translate!("csplit-error-line-number-smaller-than-previous", "current" => _0, "previous" => _1))]
    LineNumberSmallerThanPrevious(usize, usize),
    #[error("{}", translate!("csplit-error-invalid-pattern", "pattern" => uucore::display::gnu_quote(_0)))]
    InvalidPattern(String, Option<PatternProblem>),
    #[error("{}", translate!("csplit-error-invalid-number", "number" => uucore::display::gnu_quote(_0)))]
    InvalidNumber(String),
    // `.quote()` (`os_display`) always wraps in straight quotes for shell-safety; GNU's own
    // curly ones (matching the rest of a diagnostic like `line number out of range`'s pattern
    // quoting, and `seq_usage_error` in the bash tool crate) are hardcoded directly here since
    // nothing in this dependency produces them.
    #[error("{}", translate!("csplit-error-integer-expected-after-delimiter", "pattern" => format!("\u{2018}{_0}\u{2019}")))]
    IntegerExpectedAfterDelimiter(String),
    #[error("{}", translate!("csplit-error-suffix-format-incorrect"))]
    SuffixFormatIncorrect,
    #[error("{}", translate!("csplit-error-suffix-format-too-many-percents"))]
    SuffixFormatTooManyPercents,
    #[error("{}", translate!("csplit-error-not-regular-file", "file" => _0.quote()))]
    NotRegularFile(String),
    #[error("{}", _0)]
    UError(Box<dyn UError>),
}

impl From<Box<dyn UError>> for CsplitError {
    fn from(error: Box<dyn UError>) -> Self {
        Self::UError(error)
    }
}

impl UError for CsplitError {
    fn code(&self) -> i32 {
        match self {
            Self::UError(e) => e.code(),
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::CsplitError;

    #[cfg(unix)]
    #[test]
    fn io_error_display_is_clean() {
        // GNU does not print "IO error:" nor the raw "(os error N)" suffix.
        let err = CsplitError::IoError(std::io::Error::from_raw_os_error(13));
        let msg = err.to_string();
        assert_eq!(msg, "Permission denied");
        assert!(!msg.contains("IO error:"));
        assert!(!msg.contains("os error"));
    }
}
