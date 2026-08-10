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
}
