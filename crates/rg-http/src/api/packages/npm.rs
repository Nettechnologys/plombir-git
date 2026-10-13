//! npm registry protocol: packument scan and publish, dist-tags, attestations.

use super::*;

/// Heap budget for the part of an npm publish document that is not attachment data.
///
/// The tarball may be hundreds of megabytes, but package coordinates, dist-tags and
/// the one version manifest are ordinary metadata. Attachment strings are removed
/// while the staged request is scanned, so this is a real heap ceiling rather than
/// another spelling of the request-body limit.
pub(crate) const MAX_NPM_PACKUMENT_METADATA_BYTES: usize = 4 * 1024 * 1024;

/// A provenance bundle is metadata, not a second package artifact.
///
/// It has to be parsed as JSON to validate its in-toto subject, so unlike the
/// tarball it cannot remain entirely opaque. Keeping a separate small ceiling
/// prevents a malicious `.sigstore` attachment from reopening the same heap bug.
pub(crate) const MAX_NPM_PROVENANCE_BYTES: usize = 4 * 1024 * 1024;

/// Match serde_json's default recursion boundary and fail before recursive descent
/// can turn a hostile packument into an unbounded call stack.
pub(crate) const MAX_NPM_PACKUMENT_DEPTH: usize = 128;

/// Object keys are metadata too. This is generous for scoped npm names while
/// keeping the small temporary allocation used to unescape a key honest.
pub(crate) const MAX_NPM_PACKUMENT_KEY_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct NpmAttachmentDataRange {
    pub(crate) start: u64,
    pub(crate) end: u64,
}

pub(crate) struct StagedNpmPackument {
    pub(crate) body: PackageArtifact,
    pub(crate) packument: NpmPublishPackument,
    pub(crate) data_ranges: BTreeMap<String, NpmAttachmentDataRange>,
}

#[derive(Debug)]
pub(crate) enum NpmPackumentError {
    Invalid(String),
    TooLarge(String),
    Internal(String),
}

impl std::fmt::Display for NpmPackumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) | Self::TooLarge(message) | Self::Internal(message) => {
                formatter.write_str(message)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum NpmJsonObject {
    Root,
    Attachments,
    Attachment(String),
    Other,
}

/// Copy an npm publish document while replacing attachment strings with `""`.
///
/// The request is already on disk. This scanner visits it once, remembers the
/// byte ranges occupied by `_attachments.<filename>.data`, and builds only the
/// small JSON document serde needs for coordinates and declared lengths. The
/// large strings are decoded from their original ranges in a second pass.
pub(crate) struct NpmPackumentScanner<R> {
    pub(crate) reader: io::BufReader<R>,
    pub(crate) position: u64,
    pub(crate) metadata: Vec<u8>,
    pub(crate) data_ranges: BTreeMap<String, NpmAttachmentDataRange>,
    pub(crate) attachment_names: BTreeSet<String>,
    pub(crate) saw_attachments: bool,
}

impl<R: io::Read> NpmPackumentScanner<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader: io::BufReader::with_capacity(64 * 1024, reader),
            position: 0,
            metadata: Vec::new(),
            data_ranges: BTreeMap::new(),
            attachment_names: BTreeSet::new(),
            saw_attachments: false,
        }
    }

    pub(crate) fn scan(
        mut self,
    ) -> Result<(Vec<u8>, BTreeMap<String, NpmAttachmentDataRange>), NpmPackumentError> {
        self.copy_whitespace()?;
        self.scan_value(0, NpmJsonObject::Root)?;
        self.copy_whitespace()?;
        if self.peek_byte()?.is_some() {
            return Err(NpmPackumentError::Invalid(
                "npm publish packument contains trailing JSON data".into(),
            ));
        }
        Ok((self.metadata, self.data_ranges))
    }

    pub(crate) fn read_byte(&mut self) -> Result<Option<u8>, NpmPackumentError> {
        let byte = match self.reader.fill_buf() {
            Ok([]) => return Ok(None),
            Ok(buffer) => buffer[0],
            Err(error) => Err(NpmPackumentError::Internal(format!(
                "cannot read the staged npm packument: {error}"
            )))?,
        };
        self.reader.consume(1);
        self.position += 1;
        Ok(Some(byte))
    }

    pub(crate) fn peek_byte(&mut self) -> Result<Option<u8>, NpmPackumentError> {
        match self.reader.fill_buf() {
            Ok([]) => Ok(None),
            Ok(buffer) => Ok(Some(buffer[0])),
            Err(error) => Err(NpmPackumentError::Internal(format!(
                "cannot read the staged npm packument: {error}"
            ))),
        }
    }

    pub(crate) fn required_byte(&mut self, what: &str) -> Result<u8, NpmPackumentError> {
        self.read_byte()?.ok_or_else(|| {
            NpmPackumentError::Invalid(format!(
                "invalid npm publish packument: unexpected end while reading {what}"
            ))
        })
    }

    pub(crate) fn emit(&mut self, byte: u8) -> Result<(), NpmPackumentError> {
        if self.metadata.len() == MAX_NPM_PACKUMENT_METADATA_BYTES {
            return Err(NpmPackumentError::TooLarge(format!(
                "npm packument metadata exceeds the configured {MAX_NPM_PACKUMENT_METADATA_BYTES}-byte limit"
            )));
        }
        self.metadata.push(byte);
        Ok(())
    }

    pub(crate) fn copy_whitespace(&mut self) -> Result<(), NpmPackumentError> {
        while self
            .peek_byte()?
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            let byte = self.required_byte("whitespace")?;
            self.emit(byte)?;
        }
        Ok(())
    }

    pub(crate) fn scan_value(
        &mut self,
        depth: usize,
        object: NpmJsonObject,
    ) -> Result<(), NpmPackumentError> {
        self.copy_whitespace()?;
        match self.peek_byte()? {
            Some(b'{') => self.scan_object(depth, object),
            Some(b'[') => self.scan_array(depth),
            Some(b'"') => {
                self.scan_copied_string(false)?;
                Ok(())
            }
            Some(_) => self.scan_primitive(),
            None => Err(NpmPackumentError::Invalid(
                "invalid npm publish packument: missing JSON value".into(),
            )),
        }
    }

    pub(crate) fn scan_object(
        &mut self,
        depth: usize,
        object: NpmJsonObject,
    ) -> Result<(), NpmPackumentError> {
        if depth >= MAX_NPM_PACKUMENT_DEPTH {
            return Err(NpmPackumentError::Invalid(format!(
                "npm publish packument exceeds the {MAX_NPM_PACKUMENT_DEPTH}-level JSON nesting limit"
            )));
        }
        let opening = self.required_byte("object opening")?;
        self.emit(opening)?;
        self.copy_whitespace()?;
        if self.peek_byte()? == Some(b'}') {
            let closing = self.required_byte("object closing")?;
            self.emit(closing)?;
            return Ok(());
        }

        loop {
            let capture_key = !matches!(object, NpmJsonObject::Other);
            let key = self.scan_copied_string(capture_key)?;
            self.copy_whitespace()?;
            if self.required_byte("object colon")? != b':' {
                return Err(NpmPackumentError::Invalid(
                    "invalid npm publish packument: expected ':' after object key".into(),
                ));
            }
            self.emit(b':')?;
            self.copy_whitespace()?;

            match (&object, key.as_deref()) {
                (NpmJsonObject::Root, Some("_attachments")) => {
                    if self.saw_attachments {
                        return Err(NpmPackumentError::Invalid(
                            "npm publish packument contains duplicate `_attachments` keys".into(),
                        ));
                    }
                    self.saw_attachments = true;
                    self.scan_value(depth + 1, NpmJsonObject::Attachments)?;
                }
                (NpmJsonObject::Attachments, Some(filename)) => {
                    if !self.attachment_names.insert(filename.to_string()) {
                        return Err(NpmPackumentError::Invalid(format!(
                            "npm publish packument contains duplicate attachment '{filename}'"
                        )));
                    }
                    self.scan_value(depth + 1, NpmJsonObject::Attachment(filename.to_string()))?;
                }
                (NpmJsonObject::Attachment(filename), Some("data")) => {
                    self.scan_attachment_data(filename.clone())?;
                }
                _ => self.scan_value(depth + 1, NpmJsonObject::Other)?,
            }

            self.copy_whitespace()?;
            match self.required_byte("object delimiter")? {
                b',' => {
                    self.emit(b',')?;
                    self.copy_whitespace()?;
                }
                b'}' => {
                    self.emit(b'}')?;
                    return Ok(());
                }
                _ => {
                    return Err(NpmPackumentError::Invalid(
                        "invalid npm publish packument: expected ',' or '}'".into(),
                    ))
                }
            }
        }
    }

    pub(crate) fn scan_array(&mut self, depth: usize) -> Result<(), NpmPackumentError> {
        if depth >= MAX_NPM_PACKUMENT_DEPTH {
            return Err(NpmPackumentError::Invalid(format!(
                "npm publish packument exceeds the {MAX_NPM_PACKUMENT_DEPTH}-level JSON nesting limit"
            )));
        }
        let opening = self.required_byte("array opening")?;
        self.emit(opening)?;
        self.copy_whitespace()?;
        if self.peek_byte()? == Some(b']') {
            let closing = self.required_byte("array closing")?;
            self.emit(closing)?;
            return Ok(());
        }
        loop {
            self.scan_value(depth + 1, NpmJsonObject::Other)?;
            self.copy_whitespace()?;
            match self.required_byte("array delimiter")? {
                b',' => {
                    self.emit(b',')?;
                    self.copy_whitespace()?;
                }
                b']' => {
                    self.emit(b']')?;
                    return Ok(());
                }
                _ => {
                    return Err(NpmPackumentError::Invalid(
                        "invalid npm publish packument: expected ',' or ']'".into(),
                    ))
                }
            }
        }
    }

    pub(crate) fn scan_copied_string(
        &mut self,
        capture: bool,
    ) -> Result<Option<String>, NpmPackumentError> {
        if self.required_byte("JSON string")? != b'"' {
            return Err(NpmPackumentError::Invalid(
                "invalid npm publish packument: object key is not a string".into(),
            ));
        }
        self.emit(b'"')?;
        let mut raw = capture.then(|| vec![b'"']);
        loop {
            let byte = self.required_byte("JSON string")?;
            self.emit(byte)?;
            if let Some(raw) = raw.as_mut() {
                if raw.len() == MAX_NPM_PACKUMENT_KEY_BYTES {
                    return Err(NpmPackumentError::TooLarge(format!(
                        "npm packument object key exceeds the configured {MAX_NPM_PACKUMENT_KEY_BYTES}-byte limit"
                    )));
                }
                raw.push(byte);
            }
            match byte {
                b'"' => break,
                b'\\' => {
                    let escaped = self.required_byte("JSON string escape")?;
                    self.emit(escaped)?;
                    if let Some(raw) = raw.as_mut() {
                        raw.push(escaped);
                    }
                    if escaped == b'u' {
                        for _ in 0..4 {
                            let digit = self.required_byte("JSON unicode escape")?;
                            self.emit(digit)?;
                            if let Some(raw) = raw.as_mut() {
                                raw.push(digit);
                            }
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(NpmPackumentError::Invalid(
                        "invalid npm publish packument: control byte in JSON string".into(),
                    ))
                }
                _ => {}
            }
        }
        raw.map(|raw| {
            serde_json::from_slice::<String>(&raw).map_err(|error| {
                NpmPackumentError::Invalid(format!(
                    "invalid npm publish packument object key: {error}"
                ))
            })
        })
        .transpose()
    }

    pub(crate) fn scan_attachment_data(
        &mut self,
        filename: String,
    ) -> Result<(), NpmPackumentError> {
        if self.required_byte("attachment data")? != b'"' {
            return Err(NpmPackumentError::Invalid(format!(
                "npm attachment '{filename}' data is not a JSON string"
            )));
        }
        self.emit(b'"')?;
        let start = self.position;
        loop {
            let plain = {
                let buffer = self.reader.fill_buf().map_err(|error| {
                    NpmPackumentError::Internal(format!(
                        "cannot read the staged npm packument: {error}"
                    ))
                })?;
                buffer
                    .iter()
                    .position(|byte| matches!(*byte, b'"' | b'\\' | 0x00..=0x1f))
                    .unwrap_or(buffer.len())
            };
            if plain > 0 {
                self.reader.consume(plain);
                self.position += plain as u64;
                continue;
            }
            let byte = self.required_byte("attachment data")?;
            match byte {
                b'"' => {
                    let end = self.position - 1;
                    self.emit(b'"')?;
                    if self
                        .data_ranges
                        .insert(filename.clone(), NpmAttachmentDataRange { start, end })
                        .is_some()
                    {
                        return Err(NpmPackumentError::Invalid(format!(
                            "npm attachment '{filename}' contains duplicate `data` keys"
                        )));
                    }
                    return Ok(());
                }
                b'\\' => {
                    let escaped = self.required_byte("attachment data escape")?;
                    if escaped == b'u' {
                        for _ in 0..4 {
                            self.required_byte("attachment data unicode escape")?;
                        }
                    }
                }
                0x00..=0x1f => {
                    return Err(NpmPackumentError::Invalid(format!(
                        "npm attachment '{filename}' contains a control byte"
                    )))
                }
                _ => {}
            }
        }
    }

    pub(crate) fn scan_primitive(&mut self) -> Result<(), NpmPackumentError> {
        let mut read = 0_usize;
        while let Some(byte) = self.peek_byte()? {
            if byte.is_ascii_whitespace() || matches!(byte, b',' | b']' | b'}') {
                break;
            }
            let byte = self.required_byte("JSON value")?;
            self.emit(byte)?;
            read += 1;
        }
        if read == 0 {
            return Err(NpmPackumentError::Invalid(
                "invalid npm publish packument: missing JSON value".into(),
            ));
        }
        Ok(())
    }
}

/// A bounded view of a JSON string's raw contents that yields its unescaped
/// bytes. It lets `base64::read::DecoderReader` consume a tarball incrementally
/// without ever constructing the encoded string.
pub(crate) struct JsonStringRangeReader<R> {
    pub(crate) inner: io::BufReader<R>,
    pub(crate) remaining: u64,
    pub(crate) pending: [u8; 4],
    pub(crate) pending_start: usize,
    pub(crate) pending_end: usize,
}

impl<R: io::Read> JsonStringRangeReader<R> {
    pub(crate) fn new(inner: R, remaining: u64) -> Self {
        Self {
            inner: io::BufReader::with_capacity(64 * 1024, inner),
            remaining,
            pending: [0; 4],
            pending_start: 0,
            pending_end: 0,
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message.into())
    }

    pub(crate) fn raw_byte(&mut self) -> io::Result<u8> {
        if self.remaining == 0 {
            return Err(Self::invalid("truncated JSON string escape"));
        }
        let byte = match self.inner.fill_buf()? {
            [] => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated JSON string",
                ))
            }
            buffer => buffer[0],
        };
        self.inner.consume(1);
        self.remaining -= 1;
        Ok(byte)
    }

    pub(crate) fn unicode_unit(&mut self) -> io::Result<u16> {
        let mut value = 0_u16;
        for _ in 0..4 {
            let digit = self.raw_byte()?;
            let digit = (digit as char)
                .to_digit(16)
                .ok_or_else(|| Self::invalid("invalid JSON unicode escape"))?;
            value = (value << 4) | digit as u16;
        }
        Ok(value)
    }

    pub(crate) fn fill_pending(&mut self) -> io::Result<bool> {
        if self.remaining == 0 {
            return Ok(false);
        }
        let byte = self.raw_byte()?;
        let character = if byte != b'\\' {
            if byte < 0x20 {
                return Err(Self::invalid("control byte in JSON string"));
            }
            self.pending[0] = byte;
            self.pending_start = 0;
            self.pending_end = 1;
            return Ok(true);
        } else {
            match self.raw_byte()? {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{0008}',
                b'f' => '\u{000c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let high = self.unicode_unit()?;
                    let codepoint = if (0xd800..=0xdbff).contains(&high) {
                        if self.raw_byte()? != b'\\' || self.raw_byte()? != b'u' {
                            return Err(Self::invalid("unpaired high surrogate in JSON string"));
                        }
                        let low = self.unicode_unit()?;
                        if !(0xdc00..=0xdfff).contains(&low) {
                            return Err(Self::invalid("unpaired high surrogate in JSON string"));
                        }
                        0x1_0000 + (((high as u32 - 0xd800) << 10) | (low as u32 - 0xdc00))
                    } else {
                        if (0xdc00..=0xdfff).contains(&high) {
                            return Err(Self::invalid("unpaired low surrogate in JSON string"));
                        }
                        high as u32
                    };
                    char::from_u32(codepoint)
                        .ok_or_else(|| Self::invalid("invalid unicode scalar in JSON string"))?
                }
                _ => return Err(Self::invalid("invalid JSON string escape")),
            }
        };

        let encoded = character.encode_utf8(&mut self.pending);
        self.pending_start = 0;
        self.pending_end = encoded.len();
        Ok(true)
    }
}

impl<R: io::Read> io::Read for JsonStringRangeReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut written = 0;
        while written < output.len() {
            if self.pending_start != self.pending_end {
                let available = self.pending_end - self.pending_start;
                let copy = available.min(output.len() - written);
                output[written..written + copy]
                    .copy_from_slice(&self.pending[self.pending_start..self.pending_start + copy]);
                self.pending_start += copy;
                written += copy;
                continue;
            }
            if self.remaining == 0 {
                break;
            }

            let plain = {
                let buffer = self.inner.fill_buf()?;
                if buffer.is_empty() {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated JSON string",
                    ));
                }
                let available = buffer.len().min(self.remaining as usize);
                buffer[..available]
                    .iter()
                    .position(|byte| *byte == b'\\' || *byte < 0x20)
                    .unwrap_or(available)
                    .min(output.len() - written)
            };
            if plain > 0 {
                let buffer = self.inner.fill_buf()?;
                output[written..written + plain].copy_from_slice(&buffer[..plain]);
                self.inner.consume(plain);
                self.remaining -= plain as u64;
                written += plain;
                continue;
            }
            if !self.fill_pending()? {
                break;
            }
        }
        Ok(written)
    }
}

pub(crate) fn parse_staged_npm_packument(
    body: PackageArtifact,
) -> Result<StagedNpmPackument, NpmPackumentError> {
    let reader = body.reader().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot read the staged npm packument: {error}"))
    })?;
    let (metadata, data_ranges) = NpmPackumentScanner::new(reader).scan()?;
    let packument = serde_json::from_slice(&metadata).map_err(|error| {
        NpmPackumentError::Invalid(format!("invalid npm publish packument: {error}"))
    })?;
    Ok(StagedNpmPackument {
        body,
        packument,
        data_ranges,
    })
}

pub(crate) struct DecodedNpmTarball {
    pub(crate) artifact: PackageArtifact,
    pub(crate) sha512: String,
}

pub(crate) fn decode_npm_tarball_range(
    body: &PackageArtifact,
    range: NpmAttachmentDataRange,
    filename: &str,
    artifact_limit: usize,
    repo_root: &FsPath,
) -> Result<DecodedNpmTarball, NpmPackumentError> {
    let reader = attachment_range_reader(body, range)?;
    let mut decoder =
        base64::read::DecoderReader::new(reader, &base64::engine::general_purpose::STANDARD);
    let staging_dir = rg_core::staging::StagingArea::PackageUploads.path_in(repo_root);
    let mut staged = tempfile::Builder::new()
        .prefix("npm-attachment-")
        .suffix(".upload")
        .tempfile_in(&staging_dir)
        .map_err(|error| {
            NpmPackumentError::Internal(format!(
                "cannot create npm attachment staging file: {error}"
            ))
        })?;
    let mut hasher = Sha512::new();
    let mut buffer = vec![0_u8; 128 * 1024];
    let mut len = 0_u64;
    loop {
        let read = decoder.read(&mut buffer).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                NpmPackumentError::Invalid(format!(
                    "npm tarball attachment '{filename}' is not valid base64: {error}"
                ))
            } else {
                NpmPackumentError::Internal(format!("cannot read npm tarball attachment: {error}"))
            }
        })?;
        if read == 0 {
            break;
        }
        len = len.checked_add(read as u64).ok_or_else(|| {
            NpmPackumentError::TooLarge(format!(
                "npm tarball attachment exceeds the configured {artifact_limit}-byte artifact limit"
            ))
        })?;
        if len > artifact_limit as u64 {
            return Err(NpmPackumentError::TooLarge(format!(
                "npm tarball attachment exceeds the configured {artifact_limit}-byte artifact limit"
            )));
        }
        staged.write_all(&buffer[..read]).map_err(|error| {
            NpmPackumentError::Internal(format!(
                "cannot write npm attachment staging file: {error}"
            ))
        })?;
        hasher.update(&buffer[..read]);
    }
    staged.flush().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot flush npm attachment staging file: {error}"))
    })?;
    let written = staged.as_file().metadata().map_err(|error| {
        NpmPackumentError::Internal(format!("cannot stat npm attachment staging file: {error}"))
    })?;
    if written.len() != len {
        return Err(NpmPackumentError::Internal(format!(
            "npm attachment staging file changed size before validation: expected {len}, got {}",
            written.len()
        )));
    }
    Ok(DecodedNpmTarball {
        artifact: PackageArtifact::spooled(staged.into_temp_path(), len),
        sha512: hex::encode(hasher.finalize()),
    })
}

/// The CouchDB-shaped document `npm publish` PUTs to the package URL.
///
/// npm sends one new version and its tarball per request. Unknown top-level and
/// version fields are intentionally ignored: the tarball's `package.json` is
/// the artifact authority, while this envelope only identifies and carries it.
///
/// `access` is the exception, and card_b5db648768e4 is why. It is not a
/// description of the artifact but an instruction about *who may read it*, and
/// serde's silent handling of unknown keys turned `npm publish --access
/// restricted` into a `201` for a package left as readable as its repository.
/// See [`npm_access_matches_repository`].
#[derive(Deserialize, ToSchema)]
pub struct NpmPublishPackument {
    #[serde(rename = "_id")]
    pub id: String,
    pub name: String,
    #[serde(rename = "dist-tags")]
    pub dist_tags: BTreeMap<String, String>,
    pub versions: BTreeMap<String, NpmPublishVersion>,
    #[serde(rename = "_attachments")]
    pub attachments: BTreeMap<String, NpmPublishAttachment>,
    /// `null` when the publisher expressed no preference, which npm's own CLI
    /// sends whenever `--access` is absent.
    #[serde(default)]
    pub access: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct NpmPublishVersion {
    pub name: String,
    pub version: String,
}

#[derive(Deserialize, ToSchema)]
pub struct NpmPublishAttachment {
    pub data: String,
    pub length: usize,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DecodedNpmProvenance {
    pub(crate) filename: String,
    pub(crate) bundle: Vec<u8>,
    pub(crate) predicate_type: String,
}

#[derive(Debug)]
pub(crate) struct DecodedNpmPublish {
    pub(crate) filename: String,
    pub(crate) version: String,
    pub(crate) dist_tag: String,
    pub(crate) tarball: PackageArtifact,
    pub(crate) provenance: Option<DecodedNpmProvenance>,
}

pub(crate) struct PlannedNpmPublish {
    pub(crate) filename: String,
    pub(crate) version: String,
    pub(crate) dist_tag: String,
    pub(crate) tarball: NpmPublishAttachment,
    pub(crate) provenance: Option<(String, NpmPublishAttachment)>,
}

#[derive(Debug)]
pub(crate) struct InspectedNpmProvenance {
    pub(crate) bundle: serde_json::Value,
    pub(crate) bytes: Vec<u8>,
    pub(crate) predicate_type: String,
    pub(crate) subject_name: String,
    pub(crate) subject_sha512: String,
}

/// Whether the access the packument asks for is the access the registry gives.
///
/// Plombir Git has no per-package visibility: a package is exactly as readable as
/// the repository holding it. So `access` cannot be *applied*, only agreed with
/// or refused — and refusing is the only honest answer, in both directions. A
/// `restricted` package in a public repository is world-readable while its
/// publisher believes it is not; a `public` one in a private repository is
/// invisible to the audience it was published for. Silence used to answer both
/// with `201`.
///
/// `None` is the absence of a claim (npm sends it whenever `--access` is not
/// given) and passes; an unrecognised value is refused, because a registry that
/// waves through an instruction it cannot read is back where it started.
pub(crate) fn npm_access_matches_repository(
    access: Option<&str>,
    is_private: bool,
) -> Result<(), String> {
    let Some(access) = access else {
        return Ok(());
    };
    let wanted_private = match access {
        "restricted" => true,
        "public" => false,
        other => {
            return Err(format!(
                "unsupported npm access '{other}': this registry understands only \
                 'public' and 'restricted'"
            ))
        }
    };
    if wanted_private == is_private {
        return Ok(());
    }
    let repository = if is_private { "private" } else { "public" };
    Err(format!(
        "npm access '{access}' does not match the {repository} repository this package is \
         published to. Plombir Git has no per-package visibility — a package is readable by \
         exactly whoever can read its repository — so change the repository's visibility \
         instead of the publish's --access"
    ))
}

pub(crate) fn npm_package_purl(name: &str, version: &str) -> String {
    match name.strip_prefix('@').and_then(|name| name.split_once('/')) {
        Some((scope, package)) => format!("pkg:npm/%40{scope}/{package}@{version}"),
        None => format!("pkg:npm/{name}@{version}"),
    }
}

/// Parse the Sigstore bundle shape emitted by npm and the in-toto statement it
/// signs. Trust-chain verification remains npm/Sigstore's job; Plombir Git's
/// publish boundary enforces the registry-specific invariant: the one subject
/// in that signed payload must name and hash the tarball in the same request.
pub(crate) fn inspect_npm_provenance_bytes(
    bytes: Vec<u8>,
    attachment: &NpmPublishAttachment,
) -> Result<InspectedNpmProvenance, String> {
    let data = std::str::from_utf8(&bytes)
        .map_err(|error| format!("npm provenance attachment is not valid UTF-8: {error}"))?;
    // npm writes JavaScript's `serializedBundle.length`, which counts UTF-16
    // code units rather than UTF-8 bytes. Real certificate material can contain
    // non-ASCII identity text, so comparing this field with `str::len()` rejects
    // a bundle npm itself just verified and sent.
    let npm_length = data.encode_utf16().count();
    if npm_length != attachment.length {
        return Err(format!(
            "npm provenance attachment length mismatch: declared {}, received {} UTF-16 code units",
            attachment.length, npm_length
        ));
    }

    let bundle: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("npm provenance attachment is not valid JSON: {error}"))?;
    let media_type = bundle
        .get("mediaType")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.starts_with("application/vnd.dev.sigstore.bundle."))
        .ok_or_else(|| {
            "npm provenance attachment has no supported Sigstore mediaType".to_string()
        })?;
    if attachment.content_type.as_deref() != Some(media_type) {
        return Err(
            "npm provenance attachment content_type does not match its Sigstore mediaType".into(),
        );
    }
    if !bundle
        .get("verificationMaterial")
        .is_some_and(serde_json::Value::is_object)
    {
        return Err("npm provenance Sigstore bundle is missing verificationMaterial".into());
    }

    let envelope = bundle
        .get("dsseEnvelope")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "npm provenance Sigstore bundle is missing dsseEnvelope".to_string())?;
    if envelope
        .get("payloadType")
        .and_then(serde_json::Value::as_str)
        != Some("application/vnd.in-toto+json")
    {
        return Err("npm provenance DSSE envelope has an unsupported payloadType".into());
    }
    let signatures = envelope
        .get("signatures")
        .and_then(serde_json::Value::as_array)
        .filter(|signatures| {
            !signatures.is_empty()
                && signatures.iter().all(|signature| {
                    signature
                        .get("sig")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|sig| !sig.is_empty())
                })
        })
        .ok_or_else(|| "npm provenance DSSE envelope has no signature".to_string())?;
    debug_assert!(!signatures.is_empty());

    let encoded_payload = envelope
        .get("payload")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "npm provenance DSSE envelope is missing its payload".to_string())?;
    let payload = base64::engine::general_purpose::STANDARD
        .decode(encoded_payload)
        .map_err(|error| format!("npm provenance DSSE payload is not valid base64: {error}"))?;
    let statement: serde_json::Value = serde_json::from_slice(&payload)
        .map_err(|error| format!("npm provenance DSSE payload is not valid JSON: {error}"))?;
    if statement.get("_type").and_then(serde_json::Value::as_str)
        != Some("https://in-toto.io/Statement/v1")
    {
        return Err("npm provenance payload is not an in-toto Statement v1".into());
    }
    let predicate_type = statement
        .get("predicateType")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "npm provenance statement has no predicateType".to_string())?
        .to_string();
    let subjects = statement
        .get("subject")
        .and_then(serde_json::Value::as_array)
        .filter(|subjects| subjects.len() == 1)
        .ok_or_else(|| "npm provenance statement must contain exactly one subject".to_string())?;
    let subject_name = subjects[0]
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "npm provenance subject has no name".to_string())?
        .to_string();
    let subject_sha512 = subjects[0]
        .pointer("/digest/sha512")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 128 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "npm provenance subject has no valid SHA-512 digest".to_string())?
        .to_ascii_lowercase();

    Ok(InspectedNpmProvenance {
        bundle,
        bytes,
        predicate_type,
        subject_name,
        subject_sha512,
    })
}

/// Turn npm's JSON envelope into the one tarball the package service owns.
///
/// Current npm builds the attachment key as exactly
/// `<manifest.name>-<manifest.version>.tgz`; retaining that spelling is also
/// what makes a second publish of the same version hit the package-file unique
/// constraint instead of being mistaken for an additional Maven-style file.
pub(crate) fn plan_npm_publish_packument(
    path_name: &str,
    packument: NpmPublishPackument,
    artifact_limit: usize,
) -> Result<PlannedNpmPublish, String> {
    if packument.id != path_name || packument.name != path_name {
        return Err(format!(
            "npm package name mismatch: URL names '{path_name}', packument names '{}'",
            packument.name
        ));
    }

    // "exactly one" is asserted by the pattern that takes it: a `len() != 1`
    // check followed by `.expect()` on `next()` were two statements agreeing
    // about a client-supplied packument, and only the first one was load
    // bearing for the process staying alive.
    let mut versions = packument.versions.into_iter();
    let (Some((version, manifest)), None) = (versions.next(), versions.next()) else {
        return Err("npm publish packument must contain exactly one version".into());
    };
    if manifest.name != path_name || manifest.version != version {
        return Err(format!(
            "npm version coordinates do not match URL and versions key '{version}'"
        ));
    }

    let Some((dist_tag, tagged_version)) = packument.dist_tags.iter().next() else {
        return Err("npm publish packument must contain exactly one dist-tag".into());
    };
    if packument.dist_tags.len() != 1 || tagged_version != &version {
        return Err("npm publish dist-tag must name the version being published".into());
    }
    rg_core::package_registry::service::validate_npm_dist_tag(dist_tag)
        .map_err(|error| format!("{error:#}"))?;
    let dist_tag = dist_tag.clone();

    let filename = format!("{path_name}-{version}.tgz");
    let provenance_filename = format!("{path_name}-{version}.sigstore");
    let mut attachments = packument.attachments;
    let tarball = attachments
        .remove(&filename)
        .ok_or_else(|| format!("npm publish packument is missing attachment '{filename}'"))?;
    if tarball.length > artifact_limit {
        return Err(format!(
            "npm tarball attachment exceeds the configured {artifact_limit}-byte artifact limit"
        ));
    }
    let provenance = match attachments.remove(&provenance_filename) {
        Some(attachment) => {
            if attachment.length > MAX_NPM_PROVENANCE_BYTES {
                return Err(format!(
                    "npm provenance attachment exceeds the configured {MAX_NPM_PROVENANCE_BYTES}-byte metadata limit"
                ));
            }
            Some((provenance_filename, attachment))
        }
        None => None,
    };
    if let Some(unexpected) = attachments.keys().next() {
        return Err(format!(
            "npm publish packument contains unsupported attachment '{unexpected}'"
        ));
    }

    Ok(PlannedNpmPublish {
        filename,
        version,
        dist_tag,
        tarball,
        provenance,
    })
}

pub(crate) fn finish_npm_publish_packument(
    path_name: &str,
    plan: PlannedNpmPublish,
    tarball: PackageArtifact,
    tarball_sha512: String,
    provenance_bytes: Option<Vec<u8>>,
) -> Result<DecodedNpmPublish, String> {
    let provenance = match (plan.provenance, provenance_bytes) {
        (Some((filename, attachment)), Some(bytes)) => {
            let inspected = inspect_npm_provenance_bytes(bytes, &attachment)?;
            let expected_name = npm_package_purl(path_name, &plan.version);
            if inspected.subject_name != expected_name {
                return Err(format!(
                    "npm provenance subject names '{}', expected '{expected_name}'",
                    inspected.subject_name
                ));
            }
            if inspected.subject_sha512 != tarball_sha512 {
                return Err(
                    "npm provenance subject SHA-512 does not match the tarball attachment".into(),
                );
            }
            Some(DecodedNpmProvenance {
                filename,
                bundle: inspected.bytes,
                predicate_type: inspected.predicate_type,
            })
        }
        (None, None) => None,
        _ => {
            return Err("npm publish packument attachment data does not match its metadata".into())
        }
    };

    Ok(DecodedNpmPublish {
        filename: plan.filename,
        version: plan.version,
        dist_tag: plan.dist_tag,
        tarball,
        provenance,
    })
}

#[cfg(test)]
pub(crate) fn decode_npm_publish_packument(
    path_name: &str,
    packument: NpmPublishPackument,
    artifact_limit: usize,
) -> Result<DecodedNpmPublish, String> {
    let plan = plan_npm_publish_packument(path_name, packument, artifact_limit)?;
    let tarball = base64::engine::general_purpose::STANDARD
        .decode(&plan.tarball.data)
        .map_err(|error| format!("npm tarball attachment is not valid base64: {error}"))?;
    if tarball.len() != plan.tarball.length {
        return Err(format!(
            "npm tarball attachment length mismatch: declared {}, decoded {}",
            plan.tarball.length,
            tarball.len()
        ));
    }
    let tarball_sha512 = hex::encode(Sha512::digest(&tarball));
    let provenance_bytes = plan
        .provenance
        .as_ref()
        .map(|(_, attachment)| attachment.data.as_bytes().to_vec());
    finish_npm_publish_packument(
        path_name,
        plan,
        PackageArtifact::from_bytes(tarball),
        tarball_sha512,
        provenance_bytes,
    )
}

pub(crate) fn decode_staged_npm_publish_packument(
    staged: StagedNpmPackument,
    path_name: &str,
    artifact_limit: usize,
    repo_root: &FsPath,
) -> Result<DecodedNpmPublish, NpmPackumentError> {
    let StagedNpmPackument {
        body,
        packument,
        mut data_ranges,
    } = staged;
    let plan = plan_npm_publish_packument(path_name, packument, artifact_limit)
        .map_err(NpmPackumentError::Invalid)?;
    let tarball_range = data_ranges.remove(&plan.filename).ok_or_else(|| {
        NpmPackumentError::Invalid(format!(
            "npm tarball attachment '{}' is missing its data string",
            plan.filename
        ))
    })?;
    let decoded = decode_npm_tarball_range(
        &body,
        tarball_range,
        &plan.filename,
        artifact_limit,
        repo_root,
    )?;
    if decoded.artifact.len() != plan.tarball.length as u64 {
        return Err(NpmPackumentError::Invalid(format!(
            "npm tarball attachment length mismatch: declared {}, decoded {}",
            plan.tarball.length,
            decoded.artifact.len()
        )));
    }

    let provenance_bytes = match plan.provenance.as_ref() {
        Some((filename, _)) => {
            let range = data_ranges.remove(filename).ok_or_else(|| {
                NpmPackumentError::Invalid(format!(
                    "npm provenance attachment '{filename}' is missing its data string"
                ))
            })?;
            Some(read_attachment_string(
                &body,
                range,
                filename,
                MAX_NPM_PROVENANCE_BYTES,
            )?)
        }
        None => None,
    };
    if let Some(filename) = data_ranges.keys().next() {
        return Err(NpmPackumentError::Invalid(format!(
            "npm publish packument contains unsupported attachment data '{filename}'"
        )));
    }

    finish_npm_publish_packument(
        path_name,
        plan,
        decoded.artifact,
        decoded.sha512,
        provenance_bytes,
    )
    .map_err(NpmPackumentError::Invalid)
}

pub(crate) fn npm_packument_error_response(error: NpmPackumentError) -> axum::response::Response {
    match error {
        NpmPackumentError::Invalid(message) => err(StatusCode::BAD_REQUEST, &message),
        NpmPackumentError::TooLarge(message) => {
            AppError::payload_too_large(message).into_response()
        }
        NpmPackumentError::Internal(message) => AppError::internal(message).into_response(),
    }
}

/// GET /api/v1/repos/:owner/:name/packages/npm/:pkg_name
///
/// npm registry "abbreviated" metadata protocol.
/// Returns JSON with dist-tags and versions.
pub async fn npm_registry_metadata(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Path((owner, name, pkg_name)): Path<(String, String, String)>,
    CiRead::<Packages> { .. }: CiRead<Packages>,
) -> axum::response::Response {
    let versions = match rg_core::package_registry::service::list_versions(
        &state.db, &owner, &name, "npm", &pkg_name,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return package_error_response(e),
    };

    let base_url = match package_base_url(&state, &headers) {
        Ok(url) => url,
        Err(e) => return e.into_response(),
    };

    let npm_versions: Vec<rg_core::package_registry::NpmVersionInfo> = versions
        .iter()
        // npm has no version-level yank marker. Leaving the object in the
        // packument makes an exact request install it as if it were live.
        .filter(|v| v.is_install_candidate())
        .map(|v| {
            // Find the tgz file
            let tgz_file = v
                .files
                .iter()
                .find(|f| f.filename.ends_with(".tgz") || f.filename.ends_with(".tar.gz"));

            rg_core::package_registry::NpmVersionInfo {
                version: v.version.clone(),
                description: None, // Version-level descriptions come from package detail
                // Digests of the tarball itself — the file the `dist` block
                // makes its promises about. The version-level `sha256` is only
                // the first file's and is used as a fallback for a version
                // stored before the per-file digests existed.
                sha256: tgz_file.and_then(|f| v.sha256_of(f)),
                sha1: tgz_file.and_then(|f| f.sha1.clone()),
                sha512: tgz_file.and_then(|f| f.sha512.clone()),
                filename: tgz_file.map(|f| f.filename.clone()),
                yanked: !v.is_install_candidate(),
                // Dependency tables live only in the `package.json` inside the
                // `.tgz`; the adapter lifted them here at publish, and this is
                // where npm's resolver reads them back.
                metadata: v.metadata.clone(),
            }
        })
        .collect();

    let dist_tags = match rg_core::package_registry::service::list_npm_dist_tags(
        &state.db, &owner, &name, &pkg_name,
    )
    .await
    {
        Ok(tags) => tags,
        Err(e) => return package_error_response(e),
    };

    // Same as the cargo index above: an unreadable row must not be served as a
    // packument saying the version depends on nothing.
    let metadata = match rg_core::package_registry::build_npm_metadata_with_dist_tags(
        &pkg_name,
        &npm_versions,
        &dist_tags,
        &base_url,
        &owner,
        &name,
    ) {
        Ok(metadata) => metadata,
        Err(e) => return package_error_response(e),
    };

    (StatusCode::OK, Json(metadata)).into_response()
}

#[cfg(test)]
mod npm_publish_packument_tests {
    use super::*;

    const NAME: &str = "@scope/matrix";
    const VERSION: &str = "1.2.3";
    const FILENAME: &str = "@scope/matrix-1.2.3.tgz";

    fn document() -> serde_json::Value {
        serde_json::json!({
            "_id": NAME,
            "name": NAME,
            "dist-tags": { "latest": VERSION },
            "versions": {
                VERSION: { "name": NAME, "version": VERSION }
            },
            "_attachments": {
                FILENAME: {
                    "data": base64::engine::general_purpose::STANDARD.encode(b"tarball"),
                    "length": 7
                }
            }
        })
    }

    fn decode(document: serde_json::Value) -> Result<DecodedNpmPublish, String> {
        decode_npm_publish_packument(NAME, serde_json::from_value(document).unwrap(), usize::MAX)
    }

    #[test]
    fn staged_packument_decodes_attachment_to_a_spool_and_honours_json_escapes() {
        let root = tempfile::tempdir().unwrap();
        let staging = rg_core::staging::StagingArea::PackageUploads.path_in(root.path());
        std::fs::create_dir_all(&staging).unwrap();
        let document = format!(
            r#"{{
                "_id":"{NAME}",
                "name":"{NAME}",
                "dist-tags":{{"latest":"{VERSION}"}},
                "versions":{{"{VERSION}":{{"name":"{NAME}","version":"{VERSION}"}}}},
                "_attachments":{{"{FILENAME}":{{
                    "data":"dGFy\u0059mFsbA==",
                    "length":7
                }}}}
            }}"#
        );
        let mut envelope = tempfile::NamedTempFile::new_in(&staging).unwrap();
        envelope.write_all(document.as_bytes()).unwrap();
        envelope.flush().unwrap();
        let body = PackageArtifact::spooled(
            envelope.into_temp_path(),
            document.len().try_into().unwrap(),
        );

        let staged = parse_staged_npm_packument(body).unwrap();
        assert_eq!(staged.packument.attachments[FILENAME].data, "");
        let decoded =
            decode_staged_npm_publish_packument(staged, NAME, usize::MAX, root.path()).unwrap();

        assert!(
            decoded.tarball.spool_path().is_some(),
            "the decoded tarball must remain file-backed"
        );
        assert_eq!(decoded.tarball.to_bytes().unwrap(), b"tarball");
    }

    #[test]
    fn duplicate_attachment_keys_are_refused_before_serde_can_overwrite_one() {
        let document = format!(
            r#"{{
                "_id":"{NAME}","name":"{NAME}",
                "dist-tags":{{"latest":"{VERSION}"}},
                "versions":{{"{VERSION}":{{"name":"{NAME}","version":"{VERSION}"}}}},
                "_attachments":{{
                    "{FILENAME}":{{"data":"dGFyYmFsbA==","length":7}},
                    "{FILENAME}":{{"data":"b3RoZXI=","length":5}}
                }}
            }}"#
        );
        let error = NpmPackumentScanner::new(document.as_bytes())
            .scan()
            .unwrap_err();
        assert!(
            error.to_string().contains("duplicate attachment"),
            "{error}"
        );
    }

    /// card_9903905d92a3: "exactly one version" used to be a `len() != 1` check
    /// followed by `.expect("one version checked above")` on the iterator, two
    /// statements agreeing about a document the publishing client wrote.
    #[test]
    fn a_packument_that_is_not_exactly_one_version_is_refused() {
        for (case, versions) in [
            ("no versions at all", serde_json::json!({})),
            (
                "two versions in one publish",
                serde_json::json!({
                    VERSION: { "name": NAME, "version": VERSION },
                    "9.9.9": { "name": NAME, "version": "9.9.9" }
                }),
            ),
        ] {
            let mut document = document();
            document["versions"] = versions;
            let error = decode(document).expect_err(case);
            assert_eq!(
                error, "npm publish packument must contain exactly one version",
                "{case} must keep the typed refusal"
            );
        }
    }

    fn provenance_attachment(subject_name: &str, subject_sha512: &str) -> serde_json::Value {
        let statement = serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{
                "name": subject_name,
                "digest": { "sha512": subject_sha512 }
            }],
            "predicateType": "https://slsa.dev/provenance/v1",
            "predicate": {}
        });
        let bundle = serde_json::json!({
            "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
            "verificationMaterial": {},
            "dsseEnvelope": {
                "payloadType": "application/vnd.in-toto+json",
                "payload": base64::engine::general_purpose::STANDARD
                    .encode(serde_json::to_vec(&statement).unwrap()),
                "signatures": [{ "sig": "structurally-present-signature" }]
            }
        });
        let data = bundle.to_string();
        let length = data.len();
        serde_json::json!({
            "data": data,
            "length": length,
            "content_type": "application/vnd.dev.sigstore.bundle.v0.3+json"
        })
    }

    fn add_matching_provenance(document: &mut serde_json::Value) {
        let sha512 = hex::encode(Sha512::digest(b"tarball"));
        document["_attachments"]["@scope/matrix-1.2.3.sigstore"] =
            provenance_attachment("pkg:npm/%40scope/matrix@1.2.3", &sha512);
    }

    #[test]
    fn scoped_packument_keeps_the_client_attachment_name_and_bytes() {
        let decoded = decode(document()).unwrap();
        assert_eq!(decoded.filename, FILENAME);
        assert_eq!(decoded.version, VERSION);
        assert_eq!(decoded.dist_tag, "latest");
        assert_eq!(decoded.tarball.to_bytes().unwrap(), b"tarball");
        assert!(decoded.provenance.is_none());
    }

    #[test]
    fn matching_sigstore_attachment_is_kept_with_its_predicate_type() {
        let mut value = document();
        add_matching_provenance(&mut value);

        let decoded = decode(value).unwrap();
        let provenance = decoded.provenance.expect("provenance was discarded");
        assert_eq!(provenance.filename, "@scope/matrix-1.2.3.sigstore");
        assert_eq!(provenance.predicate_type, "https://slsa.dev/provenance/v1");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&provenance.bundle).unwrap()["mediaType"],
            "application/vnd.dev.sigstore.bundle.v0.3+json"
        );
    }

    #[test]
    fn provenance_length_uses_the_javascript_utf16_contract() {
        let mut value = document();
        let sha512 = hex::encode(Sha512::digest(b"tarball"));
        let mut attachment = provenance_attachment("pkg:npm/%40scope/matrix@1.2.3", &sha512);
        let mut bundle: serde_json::Value =
            serde_json::from_str(attachment["data"].as_str().unwrap()).unwrap();
        bundle["verificationMaterial"]["certificateIdentity"] = serde_json::json!("München");
        let data = bundle.to_string();
        let npm_length = data.encode_utf16().count();
        assert!(
            data.len() > npm_length,
            "fixture must distinguish UTF-8 from JS length"
        );
        attachment["data"] = serde_json::json!(data);
        attachment["length"] = serde_json::json!(npm_length);
        value["_attachments"]["@scope/matrix-1.2.3.sigstore"] = attachment;

        assert!(decode(value).unwrap().provenance.is_some());
    }

    #[test]
    fn a_named_dist_tag_is_kept_as_part_of_the_publish() {
        let mut custom_tag = document();
        custom_tag["dist-tags"] = serde_json::json!({ "beta": VERSION });
        let decoded = decode(custom_tag).unwrap();
        assert_eq!(decoded.dist_tag, "beta");
        assert_eq!(decoded.version, VERSION);
    }

    #[test]
    fn inconsistent_or_unsupported_packuments_fail_closed() {
        let mut wrong_name = document();
        wrong_name["_id"] = serde_json::json!("other");
        assert!(decode(wrong_name)
            .unwrap_err()
            .contains("package name mismatch"));

        let mut wrong_length = document();
        wrong_length["_attachments"][FILENAME]["length"] = serde_json::json!(8);
        assert!(decode(wrong_length)
            .unwrap_err()
            .contains("length mismatch"));

        let mut wrong_tag_target = document();
        wrong_tag_target["dist-tags"] = serde_json::json!({ "beta": "9.9.9" });
        assert!(decode(wrong_tag_target)
            .unwrap_err()
            .contains("must name the version being published"));

        let mut multiple_tags = document();
        multiple_tags["dist-tags"] = serde_json::json!({ "latest": VERSION, "beta": VERSION });
        assert!(decode(multiple_tags)
            .unwrap_err()
            .contains("must name the version being published"));

        let mut unexpected = document();
        unexpected["_attachments"]["README.txt"] = serde_json::json!({
            "data": "readme",
            "length": 6
        });
        assert!(decode(unexpected)
            .unwrap_err()
            .contains("unsupported attachment"));
    }

    #[test]
    fn malformed_or_foreign_provenance_fails_closed() {
        let matching_sha512 = hex::encode(Sha512::digest(b"tarball"));

        let mut foreign_name = document();
        foreign_name["_attachments"]["@scope/matrix-1.2.3.sigstore"] =
            provenance_attachment("pkg:npm/other@1.2.3", &matching_sha512);
        assert!(decode(foreign_name).unwrap_err().contains("subject names"));

        let mut foreign_tarball = document();
        foreign_tarball["_attachments"]["@scope/matrix-1.2.3.sigstore"] = provenance_attachment(
            "pkg:npm/%40scope/matrix@1.2.3",
            &hex::encode(Sha512::digest(b"other tarball")),
        );
        assert!(decode(foreign_tarball)
            .unwrap_err()
            .contains("SHA-512 does not match"));

        let mut malformed = document();
        malformed["_attachments"]["@scope/matrix-1.2.3.sigstore"] = serde_json::json!({
            "data": "not json",
            "length": 8,
            "content_type": "application/vnd.dev.sigstore.bundle.v0.3+json"
        });
        assert!(decode(malformed).unwrap_err().contains("not valid JSON"));
    }

    #[test]
    fn declared_tarball_size_is_rejected_before_base64_decode() {
        let mut value = document();
        value["_attachments"][FILENAME]["length"] = serde_json::json!(8);
        value["_attachments"][FILENAME]["data"] = serde_json::json!("not base64");
        let error = decode_npm_publish_packument(NAME, serde_json::from_value(value).unwrap(), 7)
            .unwrap_err();
        assert!(error.contains("7-byte artifact limit"), "{error}");
    }
}
