//! BCP-47 locale subset for agent language configuration.
//!
//! The SDK ships a closed-enum locale model — every recognised
//! language and region is enumerated explicitly in [`LangCode`] and
//! [`RegionCode`]. Adding new locales requires a code change so
//! that the per-locale system addenda + voice picker tables stay
//! in lock-step (an exhaustive `match` over the enums prevents
//! "parse-but-no-addendum" gaps).
//!
//! ## Why a string-backed value type?
//!
//! [`Locale`] stores the canonical BCP-47 string (`es-AR`, not
//! `ES_ar`) so the wire shape stays transparent — call sites that
//! already serialise `Option<String>` (e.g.
//! `nexo_tool_meta::reply_kind::OutboundReplyContext::language`)
//! keep working unchanged. Consumers parse the string back into a
//! [`Locale`] on the receiving side.
//!
//! See `crates/microapp-sdk/src/voice/locale_addenda.rs` for the
//! addendum + voice picker tables that consume this type.

use thiserror::Error;

/// Closed set of language subtags. Adding one == code change.
///
/// Lowercase 2-letter ISO-639-1 codes when serialised. Variants
/// listed alphabetically by code for diff stability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LangCode {
    /// German.
    De,
    /// English.
    En,
    /// Spanish.
    Es,
    /// French.
    Fr,
    /// Italian.
    It,
    /// Japanese.
    Ja,
    /// Portuguese.
    Pt,
    /// Chinese (simplified script assumed; `zh-Hant` rejected v1).
    Zh,
}

impl LangCode {
    /// Lowercase 2-letter ISO-639-1 code (`es`, `en`, `pt`, …).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::De => "de",
            Self::En => "en",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::It => "it",
            Self::Ja => "ja",
            Self::Pt => "pt",
            Self::Zh => "zh",
        }
    }
}

/// Closed set of region subtags. Per-language coverage is what the
/// voice picker table guarantees a region-matched Edge voice for.
///
/// Uppercase 2-letter ISO-3166-1 alpha-2 codes when serialised.
/// Variants listed alphabetically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionCode {
    /// Argentina.
    Ar,
    /// Australia.
    Au,
    /// Brazil.
    Br,
    /// Canada.
    Ca,
    /// Chile.
    Cl,
    /// China.
    Cn,
    /// Colombia.
    Co,
    /// Germany.
    De,
    /// Spain.
    Es,
    /// France.
    Fr,
    /// United Kingdom.
    Gb,
    /// Italy.
    It,
    /// Japan.
    Jp,
    /// Mexico.
    Mx,
    /// Peru.
    Pe,
    /// Portugal.
    Pt,
    /// United States.
    Us,
}

impl RegionCode {
    /// Uppercase 2-letter ISO-3166-1 alpha-2 code (`AR`, `MX`, …).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ar => "AR",
            Self::Au => "AU",
            Self::Br => "BR",
            Self::Ca => "CA",
            Self::Cl => "CL",
            Self::Cn => "CN",
            Self::Co => "CO",
            Self::De => "DE",
            Self::Es => "ES",
            Self::Fr => "FR",
            Self::Gb => "GB",
            Self::It => "IT",
            Self::Jp => "JP",
            Self::Mx => "MX",
            Self::Pe => "PE",
            Self::Pt => "PT",
            Self::Us => "US",
        }
    }
}

/// Parsed BCP-47 locale (subset).
///
/// Cheap to clone — wraps a single canonical [`String`]. Step 2
/// adds [`std::str::FromStr`] / [`std::fmt::Display`] / serde
/// transparency.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Locale(String);

/// Parser-side errors. Wrapped in [`thiserror::Error`] so they
/// surface cleanly through the existing error envelopes
/// (`ToolError::InvalidArguments`, daemon boot logs, admin RPC).
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum LocaleParseError {
    /// Empty input string after trimming whitespace.
    #[error("empty locale string")]
    Empty,
    /// Language subtag not in [`LangCode`]'s closed set.
    #[error("unsupported language subtag `{0}`")]
    UnknownLanguage(String),
    /// Region subtag not in [`RegionCode`]'s closed set OR not
    /// covered by the voice picker for the supplied language.
    #[error("unsupported region subtag `{1}` for language `{0}`")]
    UnknownRegion(String, String),
    /// Locale string carries more than `language[-region]` —
    /// script subtags (`zh-Hant`), variants (`de-DE-1996`), and
    /// extension subtags are deferred to a follow-up.
    #[error("unsupported subtag count: locale `{0}` has more than one region/script subtag")]
    TooManySubtags(String),
}
