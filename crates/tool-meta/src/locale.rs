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
/// Cheap to clone — wraps a single canonical [`String`]
/// (`es-AR`, never `ES_ar`). Construct via [`std::str::FromStr`]
/// or [`Locale::new`]; both routes guarantee the closed-enum set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Locale(String);

impl Locale {
    /// Build a locale from already-typed enum values. The caller
    /// has already proved the language + region pair is valid; the
    /// resulting [`Locale::as_bcp47`] is the canonical
    /// `"<lang>-<REGION>"` (or `"<lang>"` when region is None).
    pub fn new(language: LangCode, region: Option<RegionCode>) -> Self {
        let raw = match region {
            Some(r) => format!("{}-{}", language.as_str(), r.as_str()),
            None => language.as_str().to_string(),
        };
        Self(raw)
    }

    /// Recover the [`LangCode`]. Infallible: every constructed
    /// [`Locale`] has been parsed against the closed enum.
    pub fn language(&self) -> LangCode {
        // Safe-by-construction: `Self::new` and `FromStr` both
        // build from a known-good `LangCode`. We re-parse the
        // first segment instead of caching to keep the type
        // small + `Copy`-clone-friendly via the inner String.
        let head = self.0.split('-').next().unwrap_or("");
        parse_language(head).expect("Locale invariant: language always valid")
    }

    /// Recover the [`RegionCode`] when present, `None` for
    /// language-only locales (`"es"`, `"en"`, …).
    pub fn region(&self) -> Option<RegionCode> {
        let mut parts = self.0.split('-');
        let _lang = parts.next();
        let region = parts.next()?;
        // `Self::new` and `FromStr` only produce already-validated
        // region tokens, so this expect can never trip on a value
        // that came through the public surface.
        Some(parse_region(region).expect("Locale invariant: region always valid"))
    }

    /// The canonical BCP-47 string (`"es-AR"` / `"en-US"` /
    /// `"es"`). Identical to [`Locale::to_string`] but without the
    /// allocation when the caller can borrow.
    pub fn as_bcp47(&self) -> &str {
        &self.0
    }

    /// Drop the region subtag, returning the language-only locale.
    /// Useful for fallback logic in voice picker / addendum tables.
    pub fn language_only(&self) -> Locale {
        Self::new(self.language(), None)
    }

    /// `true` when the locale carries no region subtag.
    pub fn is_just_language(&self) -> bool {
        !self.0.contains('-')
    }

    /// Iterate every locale that [`FromStr`] would accept: each
    /// [`LangCode`] paired with `None`, then with each
    /// [`RegionCode`]. Output is the full cross-product
    /// (8 × (1 + 17) = 144 entries) regardless of voice picker /
    /// addendum coverage; the lint script intersects this against
    /// the curated frontend `SUPPORTED_LOCALES` list to catch drift.
    ///
    /// Order is deterministic (alphabetical by language code,
    /// then language-only first, then alphabetical by region
    /// code) so the dump is diff-stable.
    pub fn iter_supported() -> impl Iterator<Item = Locale> {
        const LANGS: &[LangCode] = &[
            LangCode::De,
            LangCode::En,
            LangCode::Es,
            LangCode::Fr,
            LangCode::It,
            LangCode::Ja,
            LangCode::Pt,
            LangCode::Zh,
        ];
        const REGIONS: &[RegionCode] = &[
            RegionCode::Ar,
            RegionCode::Au,
            RegionCode::Br,
            RegionCode::Ca,
            RegionCode::Cl,
            RegionCode::Cn,
            RegionCode::Co,
            RegionCode::De,
            RegionCode::Es,
            RegionCode::Fr,
            RegionCode::Gb,
            RegionCode::It,
            RegionCode::Jp,
            RegionCode::Mx,
            RegionCode::Pe,
            RegionCode::Pt,
            RegionCode::Us,
        ];
        LANGS.iter().flat_map(|lang| {
            std::iter::once(Locale::new(*lang, None)).chain(
                REGIONS
                    .iter()
                    .map(move |region| Locale::new(*lang, Some(*region))),
            )
        })
    }
}

impl std::str::FromStr for Locale {
    type Err = LocaleParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(LocaleParseError::Empty);
        }
        // `es_AR` (Java/Microsoft style) accepted; canonical
        // separator is `-`. Lowercase the language head, uppercase
        // the region tail, refuse anything past the first region.
        let normalised = trimmed.replace('_', "-");
        let mut parts = normalised.split('-');
        let lang_raw = parts.next().unwrap_or(""); // safe: non-empty trimmed
        let region_raw = parts.next();
        if parts.next().is_some() {
            return Err(LocaleParseError::TooManySubtags(trimmed.to_string()));
        }

        let lang_lower = lang_raw.to_ascii_lowercase();
        let language = parse_language(&lang_lower)
            .ok_or_else(|| LocaleParseError::UnknownLanguage(lang_raw.to_string()))?;

        let region = match region_raw {
            None => None,
            Some(r) => {
                let upper = r.to_ascii_uppercase();
                let region = parse_region(&upper).ok_or_else(|| {
                    LocaleParseError::UnknownRegion(language.as_str().to_string(), r.to_string())
                })?;
                Some(region)
            }
        };

        Ok(Self::new(language, region))
    }
}

impl std::fmt::Display for Locale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// `#[serde(transparent)]` makes the JSON / YAML wire shape a bare
// string (`"es-AR"`), preserving compatibility with the existing
// `OutboundReplyContext.language: Option<String>` field. The
// transparent representation also means `serde_json::to_string`
// of a `Locale` is `"\"es-AR\""` — same as the input.
impl serde::Serialize for Locale {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> serde::Deserialize<'de> for Locale {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let raw = String::deserialize(d)?;
        raw.parse().map_err(D::Error::custom)
    }
}

fn parse_language(s: &str) -> Option<LangCode> {
    Some(match s {
        "de" => LangCode::De,
        "en" => LangCode::En,
        "es" => LangCode::Es,
        "fr" => LangCode::Fr,
        "it" => LangCode::It,
        "ja" => LangCode::Ja,
        "pt" => LangCode::Pt,
        "zh" => LangCode::Zh,
        _ => return None,
    })
}

fn parse_region(s: &str) -> Option<RegionCode> {
    Some(match s {
        "AR" => RegionCode::Ar,
        "AU" => RegionCode::Au,
        "BR" => RegionCode::Br,
        "CA" => RegionCode::Ca,
        "CL" => RegionCode::Cl,
        "CN" => RegionCode::Cn,
        "CO" => RegionCode::Co,
        "DE" => RegionCode::De,
        "ES" => RegionCode::Es,
        "FR" => RegionCode::Fr,
        "GB" => RegionCode::Gb,
        "IT" => RegionCode::It,
        "JP" => RegionCode::Jp,
        "MX" => RegionCode::Mx,
        "PE" => RegionCode::Pe,
        "PT" => RegionCode::Pt,
        "US" => RegionCode::Us,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    // ── Parser: valid inputs ────────────────────────────────────

    #[test]
    fn parses_language_only() {
        let l = Locale::from_str("es").unwrap();
        assert_eq!(l.language(), LangCode::Es);
        assert_eq!(l.region(), None);
        assert_eq!(l.as_bcp47(), "es");
    }

    #[test]
    fn parses_full_locale() {
        let l = Locale::from_str("es-AR").unwrap();
        assert_eq!(l.language(), LangCode::Es);
        assert_eq!(l.region(), Some(RegionCode::Ar));
        assert_eq!(l.as_bcp47(), "es-AR");
    }

    #[test]
    fn parses_underscore_separator_canonicalises_to_hyphen() {
        let l = Locale::from_str("es_AR").unwrap();
        assert_eq!(l.as_bcp47(), "es-AR");
    }

    #[test]
    fn parses_mixed_case_canonicalises() {
        let l = Locale::from_str("ES-ar").unwrap();
        assert_eq!(l.as_bcp47(), "es-AR");
    }

    #[test]
    fn parses_with_surrounding_whitespace() {
        let l = Locale::from_str("  es-AR  ").unwrap();
        assert_eq!(l.as_bcp47(), "es-AR");
    }

    #[test]
    fn parses_pt_br() {
        let l = Locale::from_str("pt-BR").unwrap();
        assert_eq!(l.language(), LangCode::Pt);
        assert_eq!(l.region(), Some(RegionCode::Br));
    }

    // ── Parser: invalid inputs ─────────────────────────────────

    #[test]
    fn empty_string_errors_with_empty_variant() {
        assert_eq!(Locale::from_str("").unwrap_err(), LocaleParseError::Empty);
    }

    #[test]
    fn whitespace_only_errors_with_empty_variant() {
        assert_eq!(
            Locale::from_str("   ").unwrap_err(),
            LocaleParseError::Empty
        );
    }

    #[test]
    fn unknown_language_errors() {
        match Locale::from_str("xx").unwrap_err() {
            LocaleParseError::UnknownLanguage(s) => assert_eq!(s, "xx"),
            other => panic!("expected UnknownLanguage, got {other:?}"),
        }
    }

    #[test]
    fn unknown_region_for_known_language_errors() {
        match Locale::from_str("es-XX").unwrap_err() {
            LocaleParseError::UnknownRegion(lang, region) => {
                assert_eq!(lang, "es");
                assert_eq!(region, "XX");
            }
            other => panic!("expected UnknownRegion, got {other:?}"),
        }
    }

    #[test]
    fn extra_subtags_errors_too_many() {
        match Locale::from_str("es-AR-x").unwrap_err() {
            LocaleParseError::TooManySubtags(s) => assert_eq!(s, "es-AR-x"),
            other => panic!("expected TooManySubtags, got {other:?}"),
        }
    }

    #[test]
    fn script_subtag_errors_too_many() {
        // `zh-Hant` carries a script subtag; v1 parser rejects.
        match Locale::from_str("zh-Hant-CN").unwrap_err() {
            LocaleParseError::TooManySubtags(_) => {}
            other => panic!("expected TooManySubtags, got {other:?}"),
        }
    }

    #[test]
    fn variant_subtag_errors_too_many() {
        match Locale::from_str("de-DE-1996").unwrap_err() {
            LocaleParseError::TooManySubtags(_) => {}
            other => panic!("expected TooManySubtags, got {other:?}"),
        }
    }

    #[test]
    fn m49_un_region_code_errors_unknown_region() {
        // `es-419` (UN M.49 for Latin America) — not in v1 enum.
        match Locale::from_str("es-419").unwrap_err() {
            LocaleParseError::UnknownRegion(lang, region) => {
                assert_eq!(lang, "es");
                assert_eq!(region, "419");
            }
            other => panic!("expected UnknownRegion, got {other:?}"),
        }
    }

    /// STT lang_hint trim.
    /// `Locale::language().as_str()` is the documented path the SDK's
    /// `InboundTransformHandler` uses to convert a binding's BCP-47
    /// (`es-AR`) into the ISO-639-1 prefix (`es`) that whisper's
    /// `set_language` accepts. The trim must be lossless on every
    /// supported lang+region pair.
    #[test]
    fn lang_only_trim_drops_region_for_whisper_hint() {
        for (input, expected_iso639_1) in [
            ("es-AR", "es"),
            ("es-MX", "es"),
            ("en-GB", "en"),
            ("en-US", "en"),
            ("pt-BR", "pt"),
            ("pt-PT", "pt"),
            ("zh-CN", "zh"),
            ("ja-JP", "ja"),
            // Lang-only inputs already at the trim target.
            ("es", "es"),
            ("en", "en"),
        ] {
            let l = Locale::from_str(input).unwrap();
            assert_eq!(
                l.language().as_str(),
                expected_iso639_1,
                "BCP-47 {input} must trim to ISO-639-1 {expected_iso639_1}"
            );
        }
    }

    /// `iter_supported()` is
    /// the source of truth for `cargo run -p nexo-microapp-sdk
    /// --bin locale_dump`. Verify it yields the documented count
    /// (8 lang × (1 + 17 region) = 144) and no duplicates.
    #[test]
    fn iter_supported_yields_full_cross_product() {
        let all: Vec<String> = Locale::iter_supported()
            .map(|l| l.as_bcp47().to_string())
            .collect();
        assert_eq!(
            all.len(),
            8 * (1 + 17),
            "expected 8 langs × (lang-only + 17 regions) = 144"
        );
        let unique: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "no duplicates in iter_supported");
        // Spot-check first/last by sorted order.
        let mut sorted = all.clone();
        sorted.sort();
        assert_eq!(sorted.first().map(String::as_str), Some("de"));
        assert_eq!(sorted.last().map(String::as_str), Some("zh-US"));
    }
}

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

/// Default voice id used when no locale is supplied. Matches the
/// SDK's pre-Phase-89 default ("English neutral, female").
pub const DEFAULT_VOICE_ID: &str = "en-US-AriaNeural";

/// Phase 89 (relocated from `nexo-microapp-sdk::voice` in Phase
/// 81.31 follow-up #2) — recommended Microsoft Edge neural voice
/// id for a given locale. Lookup is a deterministic match on
/// `(LangCode, Option<RegionCode>)`; returns
/// [`DEFAULT_VOICE_ID`] when `locale` is `None`. Same data the
/// microapp SDK's voice-mode runtime consumes; surfaced here so
/// admin surfaces (wizard, PersonaEditor) can preview which voice
/// a locale would use without pulling in the full SDK.
pub fn default_voice_for_locale(locale: Option<&Locale>) -> &'static str {
    let Some(loc) = locale else {
        return DEFAULT_VOICE_ID;
    };
    match (loc.language(), loc.region()) {
        // Spanish family.
        (LangCode::Es, Some(RegionCode::Ar)) => "es-AR-ElenaNeural",
        (LangCode::Es, Some(RegionCode::Mx)) => "es-MX-DaliaNeural",
        (LangCode::Es, Some(RegionCode::Es)) => "es-ES-ElviraNeural",
        (LangCode::Es, Some(RegionCode::Co)) => "es-CO-SalomeNeural",
        (LangCode::Es, Some(RegionCode::Pe)) => "es-PE-CamilaNeural",
        (LangCode::Es, Some(RegionCode::Cl)) => "es-CL-CatalinaNeural",
        (LangCode::Es, Some(RegionCode::Us)) => "es-US-PalomaNeural",
        (LangCode::Es, _) => "es-MX-DaliaNeural",
        // English family.
        (LangCode::En, Some(RegionCode::Us)) => "en-US-AriaNeural",
        (LangCode::En, Some(RegionCode::Gb)) => "en-GB-SoniaNeural",
        (LangCode::En, Some(RegionCode::Au)) => "en-AU-NatashaNeural",
        (LangCode::En, Some(RegionCode::Ca)) => "en-CA-ClaraNeural",
        (LangCode::En, _) => "en-US-AriaNeural",
        // Portuguese.
        (LangCode::Pt, Some(RegionCode::Br)) => "pt-BR-FranciscaNeural",
        (LangCode::Pt, Some(RegionCode::Pt)) => "pt-PT-RaquelNeural",
        (LangCode::Pt, _) => "pt-BR-FranciscaNeural",
        // French.
        (LangCode::Fr, Some(RegionCode::Fr)) => "fr-FR-DeniseNeural",
        (LangCode::Fr, Some(RegionCode::Ca)) => "fr-CA-SylvieNeural",
        (LangCode::Fr, _) => "fr-FR-DeniseNeural",
        // Italian.
        (LangCode::It, Some(RegionCode::It)) => "it-IT-ElsaNeural",
        (LangCode::It, _) => "it-IT-ElsaNeural",
        // German.
        (LangCode::De, Some(RegionCode::De)) => "de-DE-KatjaNeural",
        (LangCode::De, _) => "de-DE-KatjaNeural",
        // Japanese.
        (LangCode::Ja, Some(RegionCode::Jp)) => "ja-JP-NanamiNeural",
        (LangCode::Ja, _) => "ja-JP-NanamiNeural",
        // Chinese.
        (LangCode::Zh, Some(RegionCode::Cn)) => "zh-CN-XiaoxiaoNeural",
        (LangCode::Zh, _) => "zh-CN-XiaoxiaoNeural",
    }
}
