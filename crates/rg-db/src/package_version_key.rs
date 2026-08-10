//! Protocol-owned identity keys for package versions.
//!
//! The raw spelling remains part of the published metadata, but protocols can
//! define several spellings as the same version. These keys are persisted next
//! to that spelling and protected by a database UNIQUE constraint.

use std::cmp::Ordering;

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
