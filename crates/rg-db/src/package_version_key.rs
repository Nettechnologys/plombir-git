//! Protocol-owned identity keys for package versions.
//!
//! The raw spelling remains part of the published metadata, but protocols can
//! define several spellings as the same version. These keys are persisted next
//! to that spelling and protected by a database UNIQUE constraint.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::sync::OnceLock;

use regex::{Captures, Regex};
use sha2::{Digest, Sha256};

/// Composer's client-visible normalized version.
///
/// Repository entries without `version_normalized` are passed through
/// `Composer\\Semver\\VersionParser::normalize` by `ArrayLoader`. Reproduce that
/// normalizer here so publish-time uniqueness and the value advertised back to
/// Composer are one decision. This deliberately follows Composer's historical
/// compatibility grammar rather than strict SemVer: it accepts four numeric
/// components, named stability suffixes, date versions and numeric dev branches.
pub fn composer_version_normalized(value: &str) -> Option<String> {
    let mut version = value.trim().to_string();

    if let Some(captures) = composer_alias_regex().captures(&version) {
        version = captures.get(1)?.as_str().to_string();
    }
    if let Some(stability) = composer_stability_flag_regex().find(&version) {
        version.truncate(stability.start());
    }

    if matches!(version.as_str(), "master" | "trunk" | "default") {
        version.insert_str(0, "dev-");
    }
    if version
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("dev-"))
    {
        return Some(format!("dev-{}", &version[4..]));
    }

    if let Some(captures) = composer_build_metadata_regex().captures(&version) {
        version = captures.get(1)?.as_str().to_string();
    }

    if let Some(captures) = composer_classical_version_regex().captures(&version) {
        let mut normalized = captures.get(1)?.as_str().to_string();
        for component in 2..=4 {
            normalized.push_str(captures.get(component).map_or(".0", |value| value.as_str()));
        }
        return composer_normalized_modifier(normalized, &captures, 5, 6, 7);
    }

    if let Some(captures) = composer_date_version_regex().captures(&version) {
        let normalized = captures
            .get(1)?
            .as_str()
            .chars()
            .map(|character| {
                if character.is_ascii_digit() {
                    character
                } else {
                    '.'
                }
            })
            .collect();
        return composer_normalized_modifier(normalized, &captures, 2, 3, 4);
    }

    let captures = composer_dev_suffix_regex().captures(&version)?;
    let branch = captures.get(1)?.as_str();
    composer_normalized_branch(branch)
}

/// Composer's database identity key for one raw version spelling.
///
/// The normal form is also what `packages.json.version_normalized` publishes.
/// A valid but unusually long dev branch is compressed only for the bounded DB
/// key column; the full normalized value remains available through
/// [`composer_version_normalized`].
pub fn composer_version_key(value: &str) -> Option<String> {
    let normalized = composer_version_normalized(value)?;
    if normalized.len() <= 255 {
        return Some(normalized);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"composer\0");
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    let mut key = String::with_capacity(71);
    key.push_str("sha256:");
    for byte in digest {
        write!(&mut key, "{byte:02x}").expect("writing hex to a String cannot fail");
    }
    Some(key)
}

fn composer_normalized_modifier(
    mut normalized: String,
    captures: &Captures<'_>,
    stability_index: usize,
    number_index: usize,
    dev_index: usize,
) -> Option<String> {
    if let Some(stability) = captures.get(stability_index) {
        let stability = stability.as_str();
        if stability.eq_ignore_ascii_case("stable") {
            return Some(normalized);
        }
        normalized.push('-');
        normalized.push_str(match stability.to_ascii_lowercase().as_str() {
            "a" => "alpha",
            "b" => "beta",
            "p" | "pl" => "patch",
            "rc" => "RC",
            "alpha" => "alpha",
            "beta" => "beta",
            "patch" => "patch",
            _ => return None,
        });
        if let Some(number) = captures.get(number_index) {
            normalized.push_str(number.as_str().trim_start_matches(['.', '-']));
        }
    }
    if captures.get(dev_index).is_some() {
        normalized.push_str("-dev");
    }
    Some(normalized)
}

fn composer_normalized_branch(branch: &str) -> Option<String> {
    let captures = composer_numeric_branch_regex().captures(branch)?;
    let mut normalized = captures.get(1)?.as_str().to_string();
    for component in 2..=4 {
        normalized.push_str(captures.get(component).map_or(".x", |value| value.as_str()));
    }
    let mut expanded = String::with_capacity(normalized.len());
    for character in normalized.chars() {
        match character {
            'x' | 'X' | '*' => expanded.push_str("9999999"),
            _ => expanded.push(character),
        }
    }
    normalized = expanded;
    normalized.push_str("-dev");
    Some(normalized)
}

fn composer_alias_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^([^,\s]+) +as +([^,\s]+)$").expect("valid regex"))
}

fn composer_stability_flag_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)@(?:stable|RC|beta|alpha|dev)$").expect("valid regex"))
}

fn composer_build_metadata_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"^([^,\s+]+)\+[^\s]+$").expect("valid regex"))
}

fn composer_classical_version_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)^v?([0-9]{1,5})(\.[0-9]+)?(\.[0-9]+)?(\.[0-9]+)?[._-]?(?:(stable|beta|b|RC|alpha|a|patch|pl|p)((?:[.-]?[0-9]+)*)?)?([.-]?dev)?$",
        )
        .expect("valid regex")
    })
}

fn composer_date_version_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"(?i)^v?([0-9]{4}(?:[.:-]?[0-9]{2}){1,6}(?:[.:-]?[0-9]{1,3}){0,2})[._-]?(?:(stable|beta|b|RC|alpha|a|patch|pl|p)((?:[.-]?[0-9]+)*)?)?([.-]?dev)?$",
        )
        .expect("valid regex")
    })
}

fn composer_dev_suffix_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?i)^(.*?)[.-]?dev$").expect("valid regex"))
}

fn composer_numeric_branch_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)^v?([0-9]+)(\.(?:[0-9]+|[x*]))?(\.(?:[0-9]+|[x*]))?(\.(?:[0-9]+|[x*]))?$")
            .expect("valid regex")
    })
}

const NODE_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// A version parsed with `node-semver`'s loose npm compatibility grammar.
///
/// Keep this type shared between the persisted protocol identity and read-side
/// precedence selection. Feeding the cleaned spelling back through Rust's
/// strict `semver` parser is not equivalent: node-semver deliberately retains
/// numeric-looking prerelease identifiers at and above JavaScript's
/// `MAX_SAFE_INTEGER` as strings, including their leading zeroes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NpmVersion {
    numbers: [u64; 3],
    prerelease: Option<Vec<NpmPrereleaseIdentifier>>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum NpmPrereleaseIdentifier {
    Numeric(u64),
    Text(String),
}

impl NpmVersion {
    /// Parse and clean one npm version spelling using node-semver loose rules.
    pub fn parse(value: &str) -> Option<Self> {
        // node-semver rejects a raw input longer than 256 JavaScript UTF-16 code
        // units. Its `\s` prefix accepts Unicode whitespace, so byte length would
        // incorrectly reject some inputs which the client cleans successfully.
        if value.encode_utf16().count() > 256 {
            return None;
        }

        let captures = npm_loose_version_regex().captures(value.trim())?;
        let component = |index| {
            let number = captures.get(index)?.as_str().parse::<u64>().ok()?;
            (number <= NODE_MAX_SAFE_INTEGER).then_some(number)
        };
        let prerelease = captures.get(4).map(|prerelease| {
            prerelease
                .as_str()
                .split('.')
                .map(|identifier| {
                    if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                        // node-semver numberifies numeric prerelease identifiers
                        // only below MAX_SAFE_INTEGER. Larger identifiers remain
                        // strings and retain their exact spelling.
                        if let Ok(number) = identifier.parse::<u64>() {
                            if number < NODE_MAX_SAFE_INTEGER {
                                return NpmPrereleaseIdentifier::Numeric(number);
                            }
                        }
                    }
                    NpmPrereleaseIdentifier::Text(identifier.to_string())
                })
                .collect()
        });

        Some(Self {
            numbers: [component(1)?, component(2)?, component(3)?],
            prerelease,
        })
    }

    /// The cleaned `.version` npm's resolver consumes, without build metadata.
    pub fn normalized(&self) -> String {
        let mut normalized = format!(
            "{}.{}.{}",
            self.numbers[0], self.numbers[1], self.numbers[2]
        );
        if let Some(prerelease) = &self.prerelease {
            normalized.push('-');
            for (index, identifier) in prerelease.iter().enumerate() {
                if index != 0 {
                    normalized.push('.');
                }
                match identifier {
                    NpmPrereleaseIdentifier::Numeric(value) => {
                        write!(&mut normalized, "{value}")
                            .expect("writing a number to a String cannot fail");
                    }
                    NpmPrereleaseIdentifier::Text(value) => normalized.push_str(value),
                }
            }
        }
        normalized
    }
}

impl Ord for NpmVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers
            .cmp(&other.numbers)
            .then_with(|| match (&self.prerelease, &other.prerelease) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
    }
}

impl PartialOrd for NpmVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// npm's resolver identity for one version spelling.
///
/// `normalize-package-data` runs `node-semver`'s `valid` and `clean` functions
/// in loose mode before `npm publish` builds its packument. That accepts the
/// historical `v` / `=` prefixes, leading zeroes and a prerelease without the
/// separating dash, then publishes the cleaned `.version`. The latter excludes
/// build metadata entirely. Reproduce that identity here because the HTTP
/// endpoint also accepts hand-built packuments which never passed through the
/// client normalizer.
pub fn npm_version_key(value: &str) -> Option<String> {
    let key = NpmVersion::parse(value)?.normalized();

    if key.len() <= 255 {
        return Some(key);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"npm\0");
    hasher.update(key.as_bytes());
    let digest = hasher.finalize();
    let mut compressed = String::with_capacity(71);
    compressed.push_str("sha256:");
    for byte in digest {
        write!(&mut compressed, "{byte:02x}").expect("writing hex to a String cannot fail");
    }
    Some(compressed)
}

fn npm_loose_version_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r"^[v=\s]*(\d+)\.(\d+)\.(\d+)(?:-?((?:\d*[A-Za-z-][A-Za-z0-9-]*|\d+)(?:\.(?:\d*[A-Za-z-][A-Za-z0-9-]*|\d+))*))?(?:\+(?:[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*))?$",
        )
        .expect("valid regex")
    })
}

/// Cargo's SemVer identity for one crate version.
///
/// Cargo requires strict SemVer spellings, but build metadata is excluded from
/// precedence and from requirement matching. A sparse index containing both
/// `1.0.0+linux` and `1.0.0+macos` therefore advertises two rows for one
/// resolver version. Keep the publisher's full spelling in `version`; this key
/// is the same parsed version with only the non-identifying build metadata
/// removed.
pub fn cargo_version_key(value: &str) -> Option<String> {
    let parsed = semver::Version::parse(value).ok()?;
    let mut key = format!("{}.{}.{}", parsed.major, parsed.minor, parsed.patch);
    if !parsed.pre.is_empty() {
        key.push('-');
        key.push_str(parsed.pre.as_str());
    }
    Some(key)
}

/// Helm's resolver identity for one chart version.
///
/// Helm validates `Metadata.Version` with Masterminds/semver `NewVersion`,
/// whose default coercion accepts a lowercase `v`, one or two missing numeric
/// components and leading zeroes. Comparison then ignores build metadata. We
/// reproduce that boundary here before handing the normalized spelling to the
/// strict Rust SemVer parser, so validation, migration and publish all share
/// one client-compatible decision.
pub fn helm_version_key(value: &str) -> Option<String> {
    // Masterminds/semver caps NewVersion input at 256 bytes.
    if value.is_empty() || value.len() > 256 {
        return None;
    }

    let value = value.strip_prefix('v').unwrap_or(value);
    let suffix_start = value.find(['-', '+']).unwrap_or(value.len());
    let (numeric, suffix) = value.split_at(suffix_start);
    let mut components = numeric.split('.');
    let major = helm_numeric_component(components.next()?)?;
    let minor = match components.next() {
        Some(component) => Some(helm_numeric_component(component)?),
        None => None,
    };
    let patch = match components.next() {
        Some(component) => Some(helm_numeric_component(component)?),
        None => None,
    };
    if components.next().is_some() {
        return None;
    }

    let normalized = format!(
        "{major}.{}.{}{suffix}",
        minor.unwrap_or(0),
        patch.unwrap_or(0)
    );
    let parsed = semver::Version::parse(&normalized).ok()?;
    let mut identity = format!("{}.{}.{}", parsed.major, parsed.minor, parsed.patch);
    if !parsed.pre.is_empty() {
        identity.push('-');
        identity.push_str(parsed.pre.as_str());
    }
    if identity.len() <= 255 {
        return Some(identity);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"helm\0");
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut key = String::with_capacity(71);
    key.push_str("sha256:");
    for byte in digest {
        write!(&mut key, "{byte:02x}").expect("writing hex to a String cannot fail");
    }
    Some(key)
}

fn helm_numeric_component(component: &str) -> Option<u64> {
    if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    component.parse().ok()
}

/// RubyGems' identity for one version number.
///
/// `Gem::Version` compares typed numeric/text segments, not the publisher's raw
/// spelling. It also removes trailing numeric zeroes and, for prereleases, the
/// first dot-anchored `[0.]+` run immediately before text. Consequently `1.0`,
/// `1.0.0` and `1.0.0.0` are one version, as are `1.0.0.pre1` and
/// `1.0.0.pre.1`.
///
/// A dash is not a separator with SemVer meaning: RubyGems rewrites every `-`
/// to `.pre.` before partitioning. Text segments retain their case because
/// `Gem::Version` compares Ruby strings as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RubyGemsVersion {
    canonical_segments: Vec<RubyGemsVersionSegment>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RubyGemsVersionSegment {
    Number(String),
    Text(String),
}

impl RubyGemsVersion {
    /// Parse one spelling accepted by `Gem::Version`.
    ///
    /// Numeric segments stay as normalized decimal strings instead of machine
    /// integers: Ruby integers are arbitrary precision, so a very long but
    /// otherwise valid component must not acquire a different identity here.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let value = if value.is_empty() { "0" } else { value };
        if !valid_rubygems_version(value) {
            return None;
        }

        let canonical_input = rubygems_canonical_input(value);
        let bytes = canonical_input.as_bytes();
        let mut cursor = 0;
        let mut segments = Vec::new();
        while cursor < bytes.len() {
            let byte = bytes[cursor];
            if byte.is_ascii_digit() {
                let start = cursor;
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
                    cursor += 1;
                }
                let digits = &canonical_input[start..cursor];
                let normalized = digits.trim_start_matches('0');
                segments.push(RubyGemsVersionSegment::Number(
                    if normalized.is_empty() {
                        "0"
                    } else {
                        normalized
                    }
                    .to_string(),
                ));
            } else if byte.is_ascii_alphabetic() {
                let start = cursor;
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
                    cursor += 1;
                }
                segments.push(RubyGemsVersionSegment::Text(
                    canonical_input[start..cursor].to_string(),
                ));
            } else {
                // Canonicalization leaves only dots here, and dots do not
                // themselves become comparison segments.
                cursor += 1;
            }
        }

        while segments.len() > 1
            && matches!(
                segments.last(),
                Some(RubyGemsVersionSegment::Number(number)) if number == "0"
            )
        {
            segments.pop();
        }

        Some(Self {
            canonical_segments: segments,
        })
    }

    /// Stable, unambiguous encoding of `Gem::Version#canonical_segments`.
    pub fn canonical(&self) -> String {
        self.canonical_segments
            .iter()
            .map(|segment| match segment {
                RubyGemsVersionSegment::Number(number) => format!("n{number}"),
                RubyGemsVersionSegment::Text(text) => format!("s{text}"),
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

/// Apply the spelling-level rewrite from current `Gem::Version` before its
/// `partition_segments` scan.
///
/// The zero removal is intentionally not expressed only in terms of parsed
/// segments: `1.a.0b` loses that dot-delimited zero, while `1.a0b` keeps it.
/// Repeated dashes can also introduce adjacent dots, and Ruby's one-shot
/// substitution must stop at the same first match.
fn rubygems_canonical_input(value: &str) -> String {
    let mut canonical = value.replace('-', ".pre.");
    if !canonical.bytes().any(|byte| byte.is_ascii_alphabetic()) {
        return canonical;
    }

    let bytes = canonical.as_bytes();
    let mut start = 0;
    let mut removal = None;
    while start < bytes.len() {
        let starts_zero_run = matches!(bytes[start], b'0' | b'.');
        let has_anchor = start == 0 || bytes[start - 1] == b'.';
        if starts_zero_run && has_anchor {
            let mut end = start;
            while end < bytes.len() && matches!(bytes[end], b'0' | b'.') {
                end += 1;
            }
            if end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                removal = Some(start..end);
                break;
            }
        }
        start += 1;
    }
    if let Some(range) = removal {
        canonical.replace_range(range, "");
    }
    canonical
}

fn take_rubygems_run(bytes: &[u8], cursor: &mut usize, allowed: fn(u8) -> bool) -> bool {
    let start = *cursor;
    while *cursor < bytes.len() && allowed(bytes[*cursor]) {
        *cursor += 1;
    }
    *cursor > start
}

fn ascii_digit(byte: u8) -> bool {
    byte.is_ascii_digit()
}

fn ascii_alphanumeric(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

fn rubygems_suffix_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-'
}

/// `Gem::Version::VERSION_PATTERN`, kept as a scanner so this database-boundary
/// parser does not gain a regex dependency or accept Unicode lookalikes.
fn valid_rubygems_version(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    if !take_rubygems_run(bytes, &mut cursor, ascii_digit) {
        return false;
    }

    while cursor < bytes.len() && bytes[cursor] == b'.' {
        cursor += 1;
        if !take_rubygems_run(bytes, &mut cursor, ascii_alphanumeric) {
            return false;
        }
    }
    if cursor == bytes.len() {
        return true;
    }
    if bytes[cursor] != b'-' {
        return false;
    }

    cursor += 1;
    if !take_rubygems_run(bytes, &mut cursor, rubygems_suffix_character) {
        return false;
    }
    while cursor < bytes.len() {
        if bytes[cursor] != b'.' {
            return false;
        }
        cursor += 1;
        if !take_rubygems_run(bytes, &mut cursor, rubygems_suffix_character) {
            return false;
        }
    }
    true
}

/// Recover the platform identity recorded by the RubyGems adapter.
///
/// A valid metadata object with no `platform` field is a known pure-Ruby gem;
/// absent or unreadable metadata is unknown and must not be guessed as `ruby`.
pub fn rubygems_platform_from_metadata(metadata: &str) -> Option<String> {
    let document = serde_json::from_str::<serde_json::Value>(metadata).ok()?;
    let object = document.as_object()?;
    match object.get("platform") {
        None => Some("ruby".to_string()),
        Some(serde_json::Value::String(platform)) => {
            let platform = platform.trim();
            (!platform.is_empty()).then(|| platform.to_string())
        }
        Some(_) => None,
    }
}

/// Composite RubyGems release identity: canonical version plus platform.
///
/// RubyGems permits the same number for `ruby`, `java`, and native platforms;
/// only aliases on the *same* platform conflict. Normal identities stay human
/// readable. The rare value that would exceed the database's 255-character
/// key column is SHA-256 compressed instead of silently losing uniqueness.
pub fn rubygems_version_key(value: &str, platform: &str) -> Option<String> {
    let platform = platform.trim();
    if platform.is_empty() {
        return None;
    }
    let canonical = RubyGemsVersion::parse(value)?.canonical();
    let identity = format!("{canonical}|p:{platform}");
    if identity.len() <= 255 {
        return Some(identity);
    }

    let mut hasher = Sha256::new();
    hasher.update(b"rubygems\0");
    hasher.update(identity.as_bytes());
    let digest = hasher.finalize();
    let mut key = String::with_capacity(71);
    key.push_str("sha256:");
    for byte in digest {
        write!(&mut key, "{byte:02x}").expect("writing hex to a String cannot fail");
    }
    Some(key)
}

/// The NuGet version identity used by storage paths, protocol responses and
/// the package-version uniqueness boundary.
///
/// NuGet accepts one through four numeric components (missing components are
/// zero), compares the fourth `Revision`, treats prerelease text
/// case-insensitively and excludes build metadata from version identity.
///
/// Build metadata is kept on the parsed value even though it is *not* part of
/// that identity: NuGet drops it from every address it derives from a version
/// (flat-container token, registration leaf URL) but keeps it in the version's
/// full spelling, which is what `catalogEntry.version` publishes. Equality and
/// ordering therefore ignore the field — see [`NuGetVersion::full`] for the one
/// renderer that reads it.
#[derive(Clone, Debug)]
pub struct NuGetVersion {
    numbers: [u32; 4],
    release: Option<Vec<NuGetReleaseLabel>>,
    metadata: Option<String>,
}

/// Two spellings are the same version when their numbers and prerelease labels
/// agree; build metadata is excluded from NuGet version identity, so it is
/// excluded here rather than left to a derive that would silently disagree with
/// [`Ord`].
impl PartialEq for NuGetVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for NuGetVersion {}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NuGetReleaseLabel {
    Numeric(u32),
    Text(String),
}

impl NuGetVersion {
    /// Parse one valid NuGet version spelling.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() {
            return None;
        }

        let (without_metadata, metadata) = value
            .split_once('+')
            .map_or((value, None), |(version, metadata)| {
                (version, Some(metadata))
            });
        if metadata.is_some_and(|metadata| !valid_nuget_labels(metadata, true)) {
            return None;
        }

        let (numeric, release) = without_metadata
            .split_once('-')
            .map_or((without_metadata, None), |(numeric, release)| {
                (numeric, Some(release))
            });
        let components: Vec<&str> = numeric.split('.').collect();
        if components.is_empty() || components.len() > 4 {
            return None;
        }

        let mut numbers = [0; 4];
        for (slot, component) in numbers.iter_mut().zip(components) {
            let component = component.trim();
            if component.is_empty() || !component.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let parsed = component.parse::<u32>().ok()?;
            if parsed > i32::MAX as u32 {
                return None;
            }
            *slot = parsed;
        }

        let release = match release {
            Some(release) => {
                if !valid_nuget_labels(release, false) {
                    return None;
                }
                Some(
                    release
                        .split('.')
                        .map(|label| {
                            label
                                .parse::<u32>()
                                .ok()
                                .filter(|value| *value <= i32::MAX as u32)
                                .map_or_else(
                                    || NuGetReleaseLabel::Text(label.to_ascii_lowercase()),
                                    NuGetReleaseLabel::Numeric,
                                )
                        })
                        .collect(),
                )
            }
            None => None,
        };

        Some(Self {
            numbers,
            release,
            metadata: metadata.map(str::to_string),
        })
    }

    pub fn is_prerelease(&self) -> bool {
        self.release.is_some()
    }

    /// Whether this spelling needs a SemVer 2-aware client.
    ///
    /// NuGet keeps its four-component version extension in the SemVer
    /// 1-compatible set. Only dotted prerelease labels and build metadata opt a
    /// package version into SemVer 2.
    pub fn is_semver2_specific(value: &str) -> bool {
        let value = value.trim();
        let (without_metadata, metadata) = value
            .split_once('+')
            .map_or((value, None), |(version, metadata)| {
                (version, Some(metadata))
            });

        metadata.is_some()
            || without_metadata
                .split_once('-')
                .is_some_and(|(_, release)| release.contains('.'))
    }

    /// Canonical NuGet identity: three numeric components, a non-zero fourth
    /// component when present, lowercase prerelease labels, and no metadata.
    pub fn normalized(&self) -> String {
        let mut normalized = format!(
            "{}.{}.{}",
            self.numbers[0], self.numbers[1], self.numbers[2]
        );
        if self.numbers[3] != 0 {
            normalized.push('.');
            normalized.push_str(&self.numbers[3].to_string());
        }
        if let Some(release) = &self.release {
            normalized.push('-');
            normalized.push_str(
                &release
                    .iter()
                    .map(|label| match label {
                        NuGetReleaseLabel::Numeric(value) => value.to_string(),
                        NuGetReleaseLabel::Text(value) => value.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("."),
            );
        }
        normalized
    }

    /// The full normalized spelling: [`NuGetVersion::normalized`] plus the build
    /// metadata the version was published with.
    ///
    /// This is what a version's *metadata* says it is, as opposed to what it is
    /// addressed by. NuGet excludes build metadata from version identity, so a
    /// URL derived from a version must not carry it — two spellings that differ
    /// only in metadata are one package version and must resolve to one
    /// address. `catalogEntry.version` is the opposite case: it is the document
    /// stating the version's full SemVer 2 spelling, and dropping the metadata
    /// there tells a client the package was published as something it was not.
    pub fn full(&self) -> String {
        match &self.metadata {
            Some(metadata) => format!("{}+{}", self.normalized(), metadata),
            None => self.normalized(),
        }
    }
}

fn valid_nuget_labels(labels: &str, allow_numeric_leading_zeroes: bool) -> bool {
    labels.split('.').all(|label| {
        !label.is_empty()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && (allow_numeric_leading_zeroes
                || label.len() == 1
                || !label.starts_with('0')
                || !label.bytes().all(|byte| byte.is_ascii_digit()))
    })
}

impl Ord for NuGetVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers
            .cmp(&other.numbers)
            .then_with(|| match (&self.release, &other.release) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.cmp(right),
            })
    }
}

impl PartialOrd for NuGetVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NuGetReleaseLabel {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::Text(_)) => Ordering::Less,
            (Self::Text(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for NuGetReleaseLabel {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The PEP 440 identity of a Python distribution version.
///
/// Python packaging requires a public version to be unique inside a
/// distribution, and it declares whole families of spellings to be the same
/// public version: a leading `v`, letter case, leading zeroes in any numeric
/// component, the `-`/`_`/`.` separators around the pre/post/dev segments, the
/// alternate spellings of those segments (`alpha`/`beta`/`c`/`pre`/`preview`,
/// `rev`/`r`, the bare `-N` post form), an omitted segment number, an explicit
/// zero epoch, and trailing `.0` components of the release segment.
///
/// [`Pep440Version::canonical`] renders the same string
/// `packaging.utils.canonicalize_version` does — the function the reference
/// index uses for exactly this uniqueness question — so two spellings share a
/// key here whenever an installer would treat them as one release.
///
/// Unlike NuGet, the local version segment (`+ubuntu.1`) *is* part of the
/// identity: PEP 440 orders and compares it, so two local versions are two
/// releases rather than aliases of one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pep440Version {
    epoch: u64,
    release: Vec<u64>,
    pre: Option<(&'static str, u64)>,
    post: Option<u64>,
    dev: Option<u64>,
    local: Option<Vec<Pep440LocalSegment>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Pep440LocalSegment {
    Number(u64),
    Text(String),
}

/// The pre-release spellings PEP 440 accepts, each with the spelling it
/// normalizes to. Longer spellings come first so `preview` is not read as the
/// `pre` it starts with.
const PEP440_PRE_LABELS: [(&str, &str); 8] = [
    ("alpha", "a"),
    ("beta", "b"),
    ("preview", "rc"),
    ("pre", "rc"),
    ("rc", "rc"),
    ("a", "a"),
    ("b", "b"),
    ("c", "rc"),
];

/// The post-release spellings, longest first for the same reason.
const PEP440_POST_LABELS: [(&str, &str); 3] = [("post", "post"), ("rev", "post"), ("r", "post")];

impl Pep440Version {
    /// Parse one valid PEP 440 public version spelling.
    ///
    /// Returns `None` for anything the specification does not accept, and for
    /// the numerically absurd (a component that does not fit a `u64`). Both
    /// leave the caller with no protocol identity for the value, which is what
    /// keeps an unparseable historical spelling addressable by its raw text
    /// instead of being merged into something it is not.
    pub fn parse(value: &str) -> Option<Self> {
        // Every character the grammar accepts is ASCII, so lower-casing the
        // whole value up front applies the specification's case rules to the
        // segment labels and the local segment in one step; anything else is
        // rejected by the component checks below.
        let value = value.trim().to_ascii_lowercase();

        let (public, local) = match value.split_once('+') {
            Some((public, local)) => (public, Some(parse_pep440_local(local)?)),
            None => (value.as_str(), None),
        };

        let public = public.strip_prefix('v').unwrap_or(public);
        let (epoch, mut cursor) = match public.split_once('!') {
            Some((epoch, rest)) => (parse_pep440_number(epoch)?, rest),
            None => (0, public),
        };

        let mut release = vec![parse_pep440_number(take_ascii_digits(&mut cursor))?];
        while let Some(tail) = cursor.strip_prefix('.') {
            if !tail.starts_with(|character: char| character.is_ascii_digit()) {
                break;
            }
            cursor = tail;
            release.push(parse_pep440_number(take_ascii_digits(&mut cursor))?);
        }

        let pre = take_pep440_pre(&mut cursor);
        let post = take_pep440_post(&mut cursor);
        let dev = take_pep440_dev(&mut cursor);
        if !cursor.is_empty() {
            return None;
        }

        Some(Self {
            epoch,
            release,
            pre,
            post,
            dev,
            local,
        })
    }

    /// The canonical public version: the identity two spellings must share to
    /// be the same release.
    pub fn canonical(&self) -> String {
        let mut canonical = String::new();
        if self.epoch != 0 {
            canonical.push_str(&self.epoch.to_string());
            canonical.push('!');
        }

        // `1`, `1.0` and `1.0.0` are one release: PEP 440 pads the shorter
        // release segment with zeroes before comparing, so the trailing zeroes
        // carry no identity of their own.
        let mut release = self.release.as_slice();
        while release.len() > 1 && release[release.len() - 1] == 0 {
            release = &release[..release.len() - 1];
        }
        canonical.push_str(
            &release
                .iter()
                .map(u64::to_string)
                .collect::<Vec<_>>()
                .join("."),
        );

        if let Some((label, number)) = self.pre {
            canonical.push_str(label);
            canonical.push_str(&number.to_string());
        }
        if let Some(post) = self.post {
            canonical.push_str(".post");
            canonical.push_str(&post.to_string());
        }
        if let Some(dev) = self.dev {
            canonical.push_str(".dev");
            canonical.push_str(&dev.to_string());
        }
        if let Some(local) = &self.local {
            canonical.push('+');
            canonical.push_str(
                &local
                    .iter()
                    .map(|segment| match segment {
                        Pep440LocalSegment::Number(number) => number.to_string(),
                        Pep440LocalSegment::Text(text) => text.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join("."),
            );
        }
        canonical
    }
}

/// Consume the leading run of ASCII digits, leaving the cursor after it.
/// An empty result means there were none — never that the cursor moved.
fn take_ascii_digits<'a>(cursor: &mut &'a str) -> &'a str {
    let current: &'a str = cursor;
    let end = current
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(current.len());
    let (digits, rest) = current.split_at(end);
    *cursor = rest;
    digits
}

/// A PEP 440 numeric component: digits only, leading zeroes normalized away by
/// being read as a number. Values too large to hold have no identity here.
fn parse_pep440_number(digits: &str) -> Option<u64> {
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Match one label from `labels` and return the spelling it normalizes to.
fn take_pep440_label<'a>(
    cursor: &mut &'a str,
    labels: &[(&str, &'static str)],
) -> Option<&'static str> {
    let current: &'a str = cursor;
    for (spelling, normalized) in labels {
        if let Some(rest) = current.strip_prefix(*spelling) {
            *cursor = rest;
            return Some(normalized);
        }
    }
    None
}

/// Consume the optional `-`, `_` or `.` that may sit around a segment label.
fn take_pep440_separator<'a>(cursor: &mut &'a str) {
    let current: &'a str = cursor;
    if let Some(rest) = current.strip_prefix(['-', '_', '.']) {
        *cursor = rest;
    }
}

/// The number trailing a segment label, defaulting to the implicit `0`.
///
/// A digit run too large to hold yields `None`, which abandons the whole
/// segment; the digits are then left unconsumed and [`Pep440Version::parse`]
/// rejects the spelling rather than silently reading it as something smaller.
fn take_pep440_segment_number(cursor: &mut &str) -> Option<u64> {
    let digits = take_ascii_digits(cursor);
    if digits.is_empty() {
        Some(0)
    } else {
        digits.parse().ok()
    }
}

fn take_pep440_pre(cursor: &mut &str) -> Option<(&'static str, u64)> {
    let mut probe = *cursor;
    take_pep440_separator(&mut probe);
    let label = take_pep440_label(&mut probe, &PEP440_PRE_LABELS)?;
    take_pep440_separator(&mut probe);
    let number = take_pep440_segment_number(&mut probe)?;
    *cursor = probe;
    Some((label, number))
}

fn take_pep440_post(cursor: &mut &str) -> Option<u64> {
    // The implicit post form: `1.0-1` is `1.0.post1`. It is the first
    // alternative in the specification's grammar, so a `-` followed by digits
    // is a post release even though the labelled form could also start with a
    // `-` separator.
    if let Some(tail) = (*cursor).strip_prefix('-') {
        let mut probe = tail;
        let digits = take_ascii_digits(&mut probe);
        if !digits.is_empty() {
            let number = digits.parse().ok()?;
            *cursor = probe;
            return Some(number);
        }
    }

    let mut probe = *cursor;
    take_pep440_separator(&mut probe);
    take_pep440_label(&mut probe, &PEP440_POST_LABELS)?;
    take_pep440_separator(&mut probe);
    let number = take_pep440_segment_number(&mut probe)?;
    *cursor = probe;
    Some(number)
}

fn take_pep440_dev(cursor: &mut &str) -> Option<u64> {
    let mut probe = *cursor;
    take_pep440_separator(&mut probe);
    take_pep440_label(&mut probe, &[("dev", "dev")])?;
    take_pep440_separator(&mut probe);
    let number = take_pep440_segment_number(&mut probe)?;
    *cursor = probe;
    Some(number)
}

/// Parse the local version segment: alphanumeric parts separated by `-`, `_`
/// or `.`, all of which normalize to `.`, with numeric parts read as numbers so
/// their leading zeroes go the same way as everywhere else.
fn parse_pep440_local(local: &str) -> Option<Vec<Pep440LocalSegment>> {
    if local.is_empty() {
        return None;
    }
    local
        .split(['-', '_', '.'])
        .map(|part| {
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
                return None;
            }
            if part.bytes().all(|byte| byte.is_ascii_digit()) {
                part.parse().ok().map(Pep440LocalSegment::Number)
            } else {
                Some(Pep440LocalSegment::Text(part.to_string()))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_identity_matches_node_semver_loose_clean() {
        for (raw, normalized) in [
            ("1.0.0", "1.0.0"),
            ("1.0.0+Build.7", "1.0.0"),
            ("v1.0.0", "1.0.0"),
            ("=01.002.0003", "1.2.3"),
            (" 1.0.0 ", "1.0.0"),
            ("\u{a0}1.0.0\u{a0}", "1.0.0"),
            ("1.0.0-rc.01", "1.0.0-rc.1"),
            ("1.0.0alpha1+Build.7", "1.0.0-alpha1"),
            ("1.0.0-", "1.0.0--"),
            ("1.0.0-09007199254740991", "1.0.0-09007199254740991"),
        ] {
            assert_eq!(npm_version_key(raw).as_deref(), Some(normalized), "{raw}");
        }
    }

    #[test]
    fn npm_precedence_uses_the_same_loose_node_semver_parse() {
        let parse = |value| NpmVersion::parse(value).unwrap();

        assert!(parse("v02.0.0") > parse("v01.0.0"));
        assert!(parse("1.0.0") > parse("1.0.0alpha.2"));
        assert!(parse("1.0.0-alpha.10") > parse("1.0.0-alpha.2"));
        assert!(parse("1.0.0-alpha") > parse("1.0.0-2"));
        assert_eq!(parse("1.0.0+Build.7"), parse("=01.0.0"));
        assert_eq!(
            parse("1.0.0-09007199254740991").normalized(),
            "1.0.0-09007199254740991"
        );
    }

    #[test]
    fn invalid_node_semver_spelling_has_no_protocol_identity_key() {
        for invalid in [
            "",
            "1",
            "1.2",
            "V1.0.0",
            "1.0.0+",
            "1.0.0+build!",
            "9007199254740992.0.0",
            "١.٠.٠",
        ] {
            assert!(
                npm_version_key(invalid).is_none(),
                "{invalid:?} parsed as an npm version"
            );
        }
    }

    #[test]
    fn composer_identity_matches_version_parser_normalize() {
        for (raw, normalized) in [
            ("1.0.0", "1.0.0.0"),
            ("v1.0.0", "1.0.0.0"),
            ("1.0", "1.0.0.0"),
            ("1.0.0+Build.7", "1.0.0.0"),
            (" 1.0.0 ", "1.0.0.0"),
            ("10.4.13beta.2", "10.4.13.0-beta2"),
            ("1.0.0-rC15-dev", "1.0.0.0-RC15-dev"),
            ("1.0.0.pl3-dev", "1.0.0.0-patch3-dev"),
            ("2010-01-02-10-20-30.5", "2010.01.02.10.20.30.5"),
            ("20100102.x-dev", "20100102.9999999.9999999.9999999-dev"),
            ("master", "dev-master"),
            ("DEV-FOOBAR", "dev-FOOBAR"),
            ("dev-feature+issue-1", "dev-feature+issue-1"),
            ("dev-master as 1.0.0", "dev-master"),
            ("1.0.0+foo@dev", "1.0.0.0"),
            ("00.01.03.04", "00.01.03.04"),
        ] {
            assert_eq!(
                composer_version_normalized(raw).as_deref(),
                Some(normalized),
                "{raw}"
            );
            assert_eq!(
                composer_version_key(raw).as_deref(),
                Some(normalized),
                "{raw}"
            );
        }
    }

    #[test]
    fn invalid_composer_spelling_has_no_protocol_identity_key() {
        for invalid in [
            "",
            "legacy row",
            "1.0.0-meh",
            "1.0.0.0.0",
            "feature-foo",
            "1.0.0+foo bar",
            "1.0 .2",
            "~1",
            "^1",
            "1.*",
            "١.٠.٠",
        ] {
            assert!(
                composer_version_key(invalid).is_none(),
                "{invalid:?} parsed as a Composer version"
            );
        }
    }

    #[test]
    fn oversized_composer_identity_is_stably_compressed() {
        let first_spelling = format!("dev-{}", "a".repeat(300));
        let second_spelling = format!("dev-{}b", "a".repeat(299));
        let first = composer_version_key(&first_spelling).unwrap();
        assert_eq!(first, composer_version_key(&first_spelling).unwrap());
        assert!(first.starts_with("sha256:"), "{first}");
        assert_eq!(first.len(), 71);
        assert_ne!(first, composer_version_key(&second_spelling).unwrap());
        assert_eq!(
            composer_version_normalized(&first_spelling).unwrap(),
            first_spelling
        );
    }

    fn rubygems(value: &str) -> String {
        RubyGemsVersion::parse(value)
            .unwrap_or_else(|| panic!("{value:?} is a valid Gem::Version"))
            .canonical()
    }

    #[test]
    fn rubygems_identity_matches_canonical_segments() {
        for spelling in ["1", "1.0", "1.0.0", "01.000.0"] {
            assert_eq!(rubygems(spelling), "n1", "{spelling}");
        }
        for spelling in ["1.0.0.pre1", "1.0.0.pre.1"] {
            assert_eq!(rubygems(spelling), "n1.spre.n1", "{spelling}");
        }
        for spelling in ["1.0.0-rc1", "1.0.0.pre.rc1"] {
            assert_eq!(rubygems(spelling), "n1.spre.src.n1", "{spelling}");
        }
        for spelling in ["1.0.a10", "1.0.a.10"] {
            assert_eq!(rubygems(spelling), "n1.sa.n10", "{spelling}");
        }

        // Current Gem::Version canonicalizes one spelling-level `[0.]+`
        // run before text. The dot boundary matters, and the substitution is
        // deliberately one-shot when repeated dashes introduce adjacent dots.
        assert_eq!(rubygems("1-0a"), "n1.spre.sa");
        assert_eq!(rubygems("1.a.0b"), "n1.sa.sb");
        assert_eq!(rubygems("1.a0b"), "n1.sa.n0.sb");
        assert_eq!(rubygems("1--0a"), "n1.spre.spre.n0.sa");
        assert_eq!(rubygems("0.0.a.0"), "sa");
        assert_eq!(rubygems("000000000000000000000000001.000"), "n1");

        // Ruby string comparison is case-sensitive even though the segmenting
        // regex uses `/i` to recognize both cases.
        assert_ne!(rubygems("1.0.A1"), rubygems("1.0.a1"));
        assert_ne!(rubygems("1.0.a1"), rubygems("1.0.b1"));
        assert_ne!(rubygems("1.0"), rubygems("1.0.1"));
    }

    #[test]
    fn invalid_rubygems_spelling_has_no_protocol_identity_key() {
        for invalid in [
            "v1.0",
            "1+build",
            "1..0",
            "1.0_1",
            "1.0-",
            "1.-rc1",
            "legacy row",
            "١.0",
        ] {
            assert!(
                RubyGemsVersion::parse(invalid).is_none(),
                "{invalid:?} parsed as a Gem::Version"
            );
        }

        // Gem::Version treats an empty spelling as zero.
        assert_eq!(rubygems(""), rubygems("0"));
    }

    #[test]
    fn rubygems_release_identity_includes_platform() {
        assert_eq!(
            rubygems_platform_from_metadata(r#"{"dependencies":[]}"#).as_deref(),
            Some("ruby")
        );
        assert_eq!(
            rubygems_platform_from_metadata(r#"{"platform":" x86_64-linux ","dependencies":[]}"#)
                .as_deref(),
            Some("x86_64-linux")
        );
        assert!(rubygems_platform_from_metadata("not json").is_none());
        assert!(rubygems_platform_from_metadata(r#"{"platform":7}"#).is_none());

        let pure_short = rubygems_version_key("1.0", "ruby").unwrap();
        let pure_expanded = rubygems_version_key("1.0.0", "ruby").unwrap();
        let java = rubygems_version_key("1.0.0", "java").unwrap();
        assert_eq!(pure_short, pure_expanded);
        assert_ne!(pure_expanded, java);
    }

    #[test]
    fn oversized_rubygems_identity_is_stably_compressed() {
        let long_version = format!("1.{}", "a".repeat(248));
        let first = rubygems_version_key(&long_version, "x86_64-linux").unwrap();
        let second = rubygems_version_key(&long_version, "x86_64-linux").unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"), "{first}");
        assert_eq!(first.len(), 71);
        assert_ne!(first, rubygems_version_key(&long_version, "java").unwrap());
    }

    #[test]
    fn cargo_identity_excludes_build_metadata_only() {
        for spelling in ["1.2.3", "1.2.3+linux.7", "1.2.3+macos.9"] {
            assert_eq!(cargo_version_key(spelling).as_deref(), Some("1.2.3"));
        }
        assert_eq!(
            cargo_version_key("1.2.3-rc.1+linux.7").as_deref(),
            Some("1.2.3-rc.1")
        );
        assert_ne!(
            cargo_version_key("1.2.3-rc.1"),
            cargo_version_key("1.2.3-rc.2")
        );
    }

    #[test]
    fn invalid_cargo_legacy_spelling_has_no_protocol_identity_key() {
        for invalid in ["", "1", "1.2", "v1.2.3", "01.2.3", "legacy-row"] {
            assert!(
                cargo_version_key(invalid).is_none(),
                "{invalid:?} parsed as Cargo SemVer"
            );
        }
    }

    #[test]
    fn helm_identity_matches_masterminds_new_version() {
        for spelling in [
            "1",
            "1.0",
            "1.0.0",
            "v1.0.0",
            "01.000.000+build.one",
            "1.0.0+build.two",
        ] {
            assert_eq!(helm_version_key(spelling).as_deref(), Some("1.0.0"));
        }
        assert_eq!(
            helm_version_key("v2.3-rc.1+linux").as_deref(),
            Some("2.3.0-rc.1")
        );
        assert_ne!(
            helm_version_key("2.3.0-rc.1"),
            helm_version_key("2.3.0-rc.2")
        );
    }

    #[test]
    fn invalid_helm_spelling_has_no_protocol_identity_key() {
        for invalid in [
            "",
            "V1.2.3",
            "1.2.3.4",
            "1.0.0-01",
            "1.0.0+",
            " 1.0.0",
            "legacy row",
            "١.٠.٠",
        ] {
            assert!(
                helm_version_key(invalid).is_none(),
                "{invalid:?} parsed as Helm SemVer"
            );
        }
    }

    #[test]
    fn oversized_helm_identity_is_stably_compressed() {
        let first_spelling = format!("1.0.0-{}", "a".repeat(250));
        let second_spelling = format!("1.0.0-{}b", "a".repeat(249));
        let first = helm_version_key(&first_spelling).unwrap();
        assert_eq!(first, helm_version_key(&first_spelling).unwrap());
        assert!(first.starts_with("sha256:"), "{first}");
        assert_eq!(first.len(), 71);
        assert_ne!(first, helm_version_key(&second_spelling).unwrap());
        assert!(helm_version_key(&format!("1.0.0-{}", "a".repeat(251))).is_none());
    }

    #[test]
    fn nuget_identity_normalizes_every_equivalent_component() {
        let spellings = ["01.00.000.0-RC+Build.7", "1.0.0-rc+other", "1.0.0.0-Rc"];

        let parsed = spellings.map(|value| NuGetVersion::parse(value).unwrap());
        assert!(parsed.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(parsed[0].normalized(), "1.0.0-rc");
    }

    /// The address form drops build metadata because identity does; the full
    /// form keeps it, because that is what the package was published as.
    #[test]
    fn full_spelling_keeps_the_build_metadata_identity_excludes() {
        let with_metadata = NuGetVersion::parse("01.2-RC.2+Build.7").unwrap();
        assert_eq!(with_metadata.normalized(), "1.2.0-rc.2");
        assert_eq!(with_metadata.full(), "1.2.0-rc.2+Build.7");

        // Metadata is not a version component: it neither creates a second
        // version nor orders one spelling above the other.
        let without_metadata = NuGetVersion::parse("1.2.0-rc.2").unwrap();
        assert_eq!(with_metadata, without_metadata);
        assert_eq!(with_metadata.cmp(&without_metadata), Ordering::Equal);
        assert_eq!(without_metadata.full(), without_metadata.normalized());
    }

    #[test]
    fn invalid_legacy_spelling_has_no_protocol_identity_key() {
        assert!(NuGetVersion::parse("legacy-row").is_none());
        assert!(NuGetVersion::parse("1.0.0-alpha.01").is_none());
    }

    fn pep440(value: &str) -> String {
        Pep440Version::parse(value)
            .unwrap_or_else(|| panic!("{value:?} is a valid PEP 440 version"))
            .canonical()
    }

    /// Every normalization rule the specification lists, one case each, plus
    /// the spellings that must stay apart. Expectations are what
    /// `packaging.utils.canonicalize_version` renders.
    #[test]
    fn pep440_identity_normalizes_every_equivalent_spelling() {
        for (raw, canonical) in [
            // Case, the `v` prefix, surrounding whitespace and integer
            // normalization of every numeric component.
            ("  V01.0.0  ", "1"),
            ("1.0", "1"),
            ("1", "1"),
            ("0!1.0", "1"),
            ("1!01.02", "1!1.2"),
            // Pre-release: separators, alternate spellings, implicit number.
            ("1.1.a1", "1.1a1"),
            ("1.1-a1", "1.1a1"),
            ("1.1_a1", "1.1a1"),
            ("1.1a-1", "1.1a1"),
            ("1.1ALPHA1", "1.1a1"),
            ("1.1.Beta.01", "1.1b1"),
            ("1.1c1", "1.1rc1"),
            ("1.1pre1", "1.1rc1"),
            ("1.1preview1", "1.1rc1"),
            ("1.2a", "1.2a0"),
            // Post-release: separators, alternate spellings, implicit number,
            // and the bare `-N` form.
            ("1.2-post2", "1.2.post2"),
            ("1.2_post2", "1.2.post2"),
            ("1.2.post-2", "1.2.post2"),
            ("1.2.rev2", "1.2.post2"),
            ("1.2-r2", "1.2.post2"),
            ("1.2.post", "1.2.post0"),
            ("1.0-1", "1.post1"),
            // Development release.
            ("1.2-dev2", "1.2.dev2"),
            ("1.2_dev2", "1.2.dev2"),
            ("1.2.dev", "1.2.dev0"),
            // Local version separators and their numeric segments.
            ("1.0+ubuntu-1", "1+ubuntu.1"),
            ("1.0+Ubuntu_01", "1+ubuntu.1"),
            // Every segment at once, in the order the grammar allows them.
            (
                "V1!1.0.0.0-Beta-3-Post_4.DEV05+Local-Build.007",
                "1!1b3.post4.dev5+local.build.7",
            ),
        ] {
            assert_eq!(pep440(raw), canonical, "{raw}");
        }
    }

    /// A local version is a version of its own, and the segments that only
    /// differ in kind are not aliases either.
    #[test]
    fn pep440_keeps_apart_what_the_specification_keeps_apart() {
        assert_ne!(pep440("1.0"), pep440("1.0+local"));
        assert_ne!(pep440("1.0+one"), pep440("1.0+two"));
        assert_ne!(pep440("1.0"), pep440("1!1.0"));
        assert_ne!(pep440("1.0a1"), pep440("1.0b1"));
        assert_ne!(pep440("1.0.post1"), pep440("1.0.dev1"));
        assert_ne!(pep440("1.0"), pep440("1.0.1"));
        assert_eq!(pep440("0.0"), "0");
    }

    #[test]
    fn invalid_pep440_spelling_has_no_protocol_identity_key() {
        for invalid in [
            "",
            "legacy-row",
            "1.0.0.dev1.post2", // the grammar fixes the segment order
            "1.0+",             // an empty local version
            "1.0+_local",       // a local segment must start alphanumeric
            "1.0+lo..cal",      // and must not be empty
            "1.0-",             // a dangling separator
            "1.0.0a1b2",        // one pre-release segment, not two
            "1.-0",
            "v",
            "1!2!3",
            "1.0.0-99999999999999999999999", // beyond any numeric component
        ] {
            assert!(
                Pep440Version::parse(invalid).is_none(),
                "{invalid:?} parsed as a PEP 440 version"
            );
        }
    }
}
